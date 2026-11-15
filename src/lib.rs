//! Fast leaf decay for Pumpkin.
//!
//! When a log or leaf block is broken, or a leaf decays naturally, the leaves
//! around it are queued. A queued leaf that is not persistent and has no log
//! within vanilla's support range (6 steps through leaves) decays a few ticks
//! later, closer leaves first, so a felled tree clears in about a second.
//!
//! A decay goes through the same steps as vanilla's `LeavesBlock.randomTick`:
//! it fires `LeavesDecayEvent` (so protection plugins can cancel it), drops the
//! leaf's loot table and replaces the block with air or water.

use std::{
    cell::Cell,
    collections::{BTreeMap, HashSet, VecDeque},
    sync::{Arc, LazyLock, Mutex, PoisonError, Weak, atomic::Ordering},
};

use pumpkin_core::{
    block::drop_loot,
    plugin::{
        BoxFuture, Context, EventHandler, EventPriority, PLUGIN_API_VERSION, Plugin, PluginFuture,
        PluginMetadata,
        block::{block_break::BlockBreakEvent, leaves_decay::LeavesDecayEvent},
        server::server_tick_start::ServerTickStartEvent,
    },
    server::Server,
    world::{World, loot::LootContextParameters},
};
use pumpkin_data::{
    Block, BlockDirection, BlockState, BlockStateId,
    block_properties::OakLeavesLikeProperties,
    tag::{self, Taggable},
    world::WorldEvent,
};
use pumpkin_util::math::position::BlockPos;
use pumpkin_world::world::BlockFlags;

/// Furthest a log can be (in steps through leaves) and still keep a leaf alive.
/// Vanilla's `LeavesBlock.DECAY_DISTANCE` is 7; a leaf at distance 7 decays.
const MAX_SUPPORT_DISTANCE: u32 = 6;
/// Ticks between a break and the first leaf decaying.
const BASE_DELAY_TICKS: i32 = 4;
/// Extra ticks per step away from the broken block, so a tree clears outward.
const DELAY_PER_STEP_TICKS: i32 = 2;
/// Upper bound on leaves decayed in one tick, to keep big jungles from spiking MSPT.
const MAX_DECAYS_PER_TICK: usize = 512;
/// Upper bound on leaves visited by one search, as a guard against huge leaf walls.
const MAX_SEARCH_NODES: usize = 4096;

thread_local! {
    /// Set while we fire `LeavesDecayEvent` ourselves, so our own listener ignores it.
    static FIRING_OWN_DECAY: Cell<bool> = const { Cell::new(false) };
}

struct Pending {
    world: Weak<World>,
    pos: BlockPos,
}

#[derive(Default)]
struct Queue {
    /// Due tick -> leaves to check on that tick.
    by_tick: BTreeMap<i32, Vec<Pending>>,
    /// (world pointer, position) of every queued leaf, so a leaf is queued once.
    queued: HashSet<(usize, BlockPos)>,
}

#[derive(Default)]
struct State {
    queue: Mutex<Queue>,
}

fn world_key(world: &Arc<World>) -> usize {
    Arc::as_ptr(world) as usize
}

fn is_leaves(block: &Block) -> bool {
    block.has_tag(&tag::Block::MINECRAFT_LEAVES)
}

/// Blocks that hold leaves up: logs and a few others (vanilla's `distance = 0`).
fn supports_leaves(block: &Block) -> bool {
    block.has_tag(&tag::Block::MINECRAFT_PREVENTS_NEARBY_LEAF_DECAY)
}

impl State {
    /// Queues every non-persistent leaf connected to `origin` within support range.
    /// Those are the only leaves that a change at `origin` can leave unsupported.
    fn queue_around(&self, world: &Arc<World>, origin: BlockPos, now: i32) {
        let mut visited = HashSet::from([origin]);
        let mut frontier = VecDeque::from([(origin, 0u32)]);
        let mut found = Vec::new();

        while let Some((pos, depth)) = frontier.pop_front() {
            if depth == MAX_SUPPORT_DISTANCE || visited.len() >= MAX_SEARCH_NODES {
                continue;
            }
            for direction in BlockDirection::all() {
                let next = pos.offset(direction.to_offset());
                if !visited.insert(next) {
                    continue;
                }
                let Some(state_id) = world.get_block_state_id_if_loaded(&next) else {
                    continue;
                };
                let block = Block::from_state_id(state_id);
                if !is_leaves(block) {
                    continue;
                }
                // Persistent leaves never decay, but they still carry distance.
                if !OakLeavesLikeProperties::from_state_id(state_id).persistent {
                    found.push((next, depth + 1));
                }
                frontier.push_back((next, depth + 1));
            }
        }

        if found.is_empty() {
            return;
        }
        let key = world_key(world);
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        for (pos, depth) in found {
            if !queue.queued.insert((key, pos)) {
                continue;
            }
            #[expect(clippy::cast_possible_wrap, reason = "depth is at most 6")]
            let due = now + BASE_DELAY_TICKS + depth as i32 * DELAY_PER_STEP_TICKS;
            queue.by_tick.entry(due).or_default().push(Pending {
                world: Arc::downgrade(world),
                pos,
            });
        }
    }

    fn process_due(&self, now: i32) {
        let due = {
            let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            let mut due = Vec::new();
            while due.len() < MAX_DECAYS_PER_TICK {
                let Some(mut entry) = queue.by_tick.first_entry() else {
                    break;
                };
                if *entry.key() > now {
                    break;
                }
                let room = MAX_DECAYS_PER_TICK - due.len();
                let bucket = entry.get_mut();
                if bucket.len() <= room {
                    due.append(&mut entry.remove());
                } else {
                    // Leave the rest of this bucket for the next tick.
                    let split = bucket.len() - room;
                    due.extend(bucket.drain(split..));
                }
            }
            for pending in &due {
                queue
                    .queued
                    .remove(&(pending.world.as_ptr() as usize, pending.pos));
            }
            due
        };

        for pending in due {
            if let Some(world) = pending.world.upgrade() {
                try_decay(&world, &pending.pos);
            }
        }
    }

    fn clear(&self) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        queue.by_tick.clear();
        queue.queued.clear();
    }
}

/// Whether a block that holds leaves up is within `MAX_SUPPORT_DISTANCE` steps
/// through leaves. This is the settled value of vanilla's `distance` property,
/// so it does not depend on how far the game's distance updates have spread.
fn is_supported(world: &World, start: BlockPos) -> bool {
    let mut visited = HashSet::from([start]);
    let mut frontier = VecDeque::from([(start, 0u32)]);

    while let Some((pos, depth)) = frontier.pop_front() {
        for direction in BlockDirection::all() {
            let next = pos.offset(direction.to_offset());
            if !visited.insert(next) {
                continue;
            }
            // Never decay next to an unloaded chunk; the support may be in there.
            let Some(state_id) = world.get_block_state_id_if_loaded(&next) else {
                return true;
            };
            let block = Block::from_state_id(state_id);
            if supports_leaves(block) {
                return true;
            }
            if is_leaves(block) && depth + 1 < MAX_SUPPORT_DISTANCE {
                if visited.len() >= MAX_SEARCH_NODES {
                    return true;
                }
                frontier.push_back((next, depth + 1));
            }
        }
    }
    false
}

/// Mirrors the decay branch of vanilla's `LeavesBlock.randomTick`.
fn try_decay(world: &Arc<World>, pos: &BlockPos) {
    let Some(state_id) = world.get_block_state_id_if_loaded(pos) else {
        return;
    };
    let block = Block::from_state_id(state_id);
    if !is_leaves(block) || OakLeavesLikeProperties::from_state_id(state_id).persistent {
        return;
    }
    if is_supported(world, *pos) {
        return;
    }

    if let Some(server) = world.server.upgrade() {
        let mut event = LeavesDecayEvent::new(*pos, world.clone());
        FIRING_OWN_DECAY.set(true);
        server.plugin_manager.fire_blocking(&server, &mut event);
        FIRING_OWN_DECAY.set(false);
        if event.cancelled {
            return;
        }
    }

    let params = LootContextParameters {
        block_state: Some(BlockState::from_id(state_id)),
        position: Some(pos.to_f64()),
        ..Default::default()
    };
    drop_loot(world, block, pos, true, &params);

    // Vanilla decay is silent; the break particles and sound show the speed-up.
    world.sync_world_event(
        WorldEvent::ParticlesAndSoundDestroyBlock,
        *pos,
        i32::from(state_id.as_u16()),
    );

    let new_state_id = if block.is_waterlogged(state_id) {
        Block::WATER.default_state.id
    } else {
        BlockStateId::AIR
    };
    world.set_block_state(pos, new_state_id, BlockFlags::NOTIFY_ALL);
}

fn current_tick(server: &Server) -> i32 {
    server.tick_count.load(Ordering::Relaxed)
}

struct BreakListener(Arc<State>);

impl EventHandler<BlockBreakEvent> for BreakListener {
    fn handle<'a>(
        &'a self,
        server: &'a Arc<Server>,
        event: &'a BlockBreakEvent,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if event.cancelled || !(supports_leaves(event.block) || is_leaves(event.block)) {
                return;
            }
            // The event carries no world. Breaks without a player (explosions,
            // fire) are still caught once the first leaf decays naturally.
            let Some(player) = &event.player else {
                return;
            };
            self.0
                .queue_around(&player.world(), event.block_position, current_tick(server));
        })
    }
}

struct DecayListener(Arc<State>);

impl EventHandler<LeavesDecayEvent> for DecayListener {
    fn handle<'a>(
        &'a self,
        server: &'a Arc<Server>,
        event: &'a LeavesDecayEvent,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if event.cancelled || FIRING_OWN_DECAY.get() {
                return;
            }
            self.0
                .queue_around(&event.world, event.block_pos, current_tick(server));
        })
    }
}

struct TickListener(Arc<State>);

impl EventHandler<ServerTickStartEvent> for TickListener {
    fn handle_blocking<'a>(
        &'a self,
        _server: &'a Arc<Server>,
        event: &'a mut ServerTickStartEvent,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.0.process_due(event.tick);
        })
    }
}

struct FastLeafDecay {
    state: Arc<State>,
}

impl Plugin for FastLeafDecay {
    fn on_load(&self, context: Arc<Context>) -> PluginFuture<'_, Result<(), String>> {
        Box::pin(async move {
            context.init_log();
            context.register_event::<BlockBreakEvent, _>(
                Arc::new(BreakListener(self.state.clone())),
                EventPriority::Lowest,
                false,
            );
            context.register_event::<LeavesDecayEvent, _>(
                Arc::new(DecayListener(self.state.clone())),
                EventPriority::Lowest,
                false,
            );
            context.register_event::<ServerTickStartEvent, _>(
                Arc::new(TickListener(self.state.clone())),
                EventPriority::Normal,
                true,
            );
            context.log("Fast leaf decay enabled");
            Ok(())
        })
    }

    fn on_unload(&self, _context: Arc<Context>) -> PluginFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.state.clear();
            Ok(())
        })
    }
}

// The three symbols the native loader looks up (see `plugin/loader/native.rs`).

#[unsafe(no_mangle)]
pub static PUMPKIN_API_VERSION: u32 = PLUGIN_API_VERSION;

#[unsafe(no_mangle)]
pub static METADATA: LazyLock<PluginMetadata> = LazyLock::new(|| PluginMetadata {
    name: env!("CARGO_PKG_NAME").to_string(),
    version: env!("CARGO_PKG_VERSION").to_string(),
    authors: Vec::new(),
    description: env!("CARGO_PKG_DESCRIPTION").to_string(),
    dependencies: Vec::new(),
    permissions: Vec::new(),
});

#[expect(
    clippy::no_mangle_with_rust_abi,
    reason = "the loader calls this as a Rust `fn() -> Box<dyn Plugin>`"
)]
#[unsafe(no_mangle)]
pub fn plugin() -> Box<dyn Plugin> {
    Box::new(FastLeafDecay {
        state: Arc::default(),
    })
}
