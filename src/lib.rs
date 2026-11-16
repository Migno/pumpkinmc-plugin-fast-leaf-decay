//! Fast leaf decay for Pumpkin, as a WebAssembly plugin.
//!
//! When a log or leaf block is broken, or a leaf decays naturally, the leaves
//! around it are queued. A queued leaf that is not persistent and has no log
//! within vanilla's support range (6 steps through leaves) decays a few ticks
//! later, closer leaves first, so a felled tree clears in about a second.
//!
//! A decay drops the leaf's loot table through `/loot spawn … mine …` (the same
//! loot context vanilla's `LeavesBlock.randomTick` uses: block state, position,
//! no tool) and then replaces the leaf with air, or water if it was waterlogged.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::{LazyLock, Mutex, PoisonError},
};

use pumpkin_plugin_api::{
    Context, GameRule, Plugin, PluginMetadata, Server,
    common::BlockPos,
    events::{BlockBreakEvent, EventHandler, EventPriority, LeavesDecayEvent},
    events_wit::{BlockBreakEventData, LeavesDecayEventData},
    register_plugin,
    scheduler::SchedulerExt,
    server::CommandSender,
    world::{BlockFlags, World, block_state_to_info, resolve_block_state},
};

/// Furthest a log can be (in steps through leaves) and still keep a leaf alive.
/// Vanilla's `LeavesBlock.DECAY_DISTANCE` is 7; a leaf at distance 7 decays.
const MAX_SUPPORT_DISTANCE: u32 = 6;
/// Ticks between a break and the first leaf decaying.
const BASE_DELAY_TICKS: u64 = 4;
/// Extra ticks per step away from the broken block, so a tree clears outward.
const DELAY_PER_STEP_TICKS: u64 = 2;
/// Upper bound on leaves decayed in one tick, to keep big jungles from spiking MSPT.
const MAX_DECAYS_PER_TICK: usize = 256;
/// Upper bound on blocks visited by one search, as a guard against huge leaf walls.
const MAX_SEARCH_NODES: usize = 4096;

const NEIGHBOURS: [(i32, i32, i32); 6] = [
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
    (-1, 0, 0),
    (1, 0, 0),
];

const AIR_STATE: u16 = 0;

type Pos = (i32, i32, i32);

/// What a block state means for leaf decay.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Leaves {
        persistent: bool,
        waterlogged: bool,
    },
    /// Holds leaves up: vanilla's `#minecraft:prevents_nearby_leaf_decay`.
    Support,
    Other,
}

#[derive(Default)]
struct Queue {
    /// Ticks counted by our repeating task.
    tick: u64,
    /// Due tick -> (world name, leaf position) to check on that tick.
    by_tick: BTreeMap<u64, Vec<(String, Pos)>>,
    /// Every queued (world name, position), so a leaf is queued once.
    queued: HashSet<(String, Pos)>,
}

static QUEUE: LazyLock<Mutex<Queue>> = LazyLock::new(Mutex::default);
/// Block state id -> kind. State ids are fixed for a server version.
static KINDS: LazyLock<Mutex<HashMap<u16, Kind>>> = LazyLock::new(Mutex::default);
static WATER_STATE: LazyLock<u16> =
    LazyLock::new(|| resolve_block_state("minecraft:water", &[]).unwrap_or(AIR_STATE));

/// Vanilla's `#minecraft:prevents_nearby_leaf_decay` is `#minecraft:logs`:
/// every `*_log` and `*_wood` (stripped or not) plus the crimson and warped
/// stems and hyphae. The v0.1 plugin API has no tag lookup, so match by name.
fn is_support_name(name: &str) -> bool {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    name.ends_with("_log")
        || name.ends_with("_wood")
        || ((name.ends_with("_stem") || name.ends_with("_hyphae"))
            && (name.contains("crimson") || name.contains("warped")))
}

fn is_leaves_name(name: &str) -> bool {
    name.ends_with("_leaves")
}

fn kind_of(state: u16) -> Kind {
    if let Some(kind) = KINDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&state)
    {
        return *kind;
    }
    let kind = match block_state_to_info(state) {
        Some(info) => {
            let prop = |key: &str| {
                info.properties
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.as_str())
            };
            // Only leaves have a `persistent` property, and all of them have `distance`.
            match (prop("persistent"), prop("distance")) {
                (Some(persistent), Some(_)) => Kind::Leaves {
                    persistent: persistent == "true",
                    waterlogged: prop("waterlogged") == Some("true"),
                },
                _ if is_support_name(&info.name) => Kind::Support,
                _ => Kind::Other,
            }
        }
        None => Kind::Other,
    };
    KINDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(state, kind);
    kind
}

/// Reads blocks of one world, never loading chunks.
struct Reader<'a> {
    world: &'a World,
    loaded: HashMap<(i32, i32), bool>,
}

impl<'a> Reader<'a> {
    fn new(world: &'a World) -> Self {
        Self {
            world,
            loaded: HashMap::new(),
        }
    }

    /// The kind of block at `pos`, or `None` if its chunk is not loaded.
    fn kind(&mut self, (x, y, z): Pos) -> Option<Kind> {
        let chunk = (x >> 4, z >> 4);
        let world = self.world;
        let loaded = *self
            .loaded
            .entry(chunk)
            .or_insert_with(|| world.get_chunk(chunk.0, chunk.1).is_some());
        loaded.then(|| kind_of(world.get_block_state_id(BlockPos { x, y, z })))
    }
}

const fn offset((x, y, z): Pos, (dx, dy, dz): Pos) -> Pos {
    (x + dx, y + dy, z + dz)
}

/// Queues every non-persistent leaf connected to `origin` within support range.
/// Those are the only leaves that a change at `origin` can leave unsupported.
fn queue_around(world: &World, origin: Pos) {
    let mut reader = Reader::new(world);
    let mut visited = HashSet::from([origin]);
    let mut frontier = VecDeque::from([(origin, 0u32)]);
    let mut found = Vec::new();

    while let Some((pos, depth)) = frontier.pop_front() {
        if depth == MAX_SUPPORT_DISTANCE || visited.len() >= MAX_SEARCH_NODES {
            continue;
        }
        for step in NEIGHBOURS {
            let next = offset(pos, step);
            if !visited.insert(next) {
                continue;
            }
            // Persistent leaves never decay, but they still carry distance.
            if let Some(Kind::Leaves { persistent, .. }) = reader.kind(next) {
                if !persistent {
                    found.push((next, depth + 1));
                }
                frontier.push_back((next, depth + 1));
            }
        }
    }

    if !found.is_empty() {
        let delays = found.into_iter().map(|(pos, depth)| {
            (
                pos,
                BASE_DELAY_TICKS + u64::from(depth) * DELAY_PER_STEP_TICKS,
            )
        });
        enqueue(&world.get_name(), delays);
    }
}

fn enqueue(world_name: &str, items: impl IntoIterator<Item = (Pos, u64)>) {
    let mut queue = QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
    let now = queue.tick;
    for (pos, delay) in items {
        if queue.queued.insert((world_name.to_owned(), pos)) {
            queue
                .by_tick
                .entry(now + delay)
                .or_default()
                .push((world_name.to_owned(), pos));
        }
    }
}

/// Whether a block that holds leaves up is within `MAX_SUPPORT_DISTANCE` steps
/// through leaves. This is the settled value of vanilla's `distance` property,
/// so it does not depend on how far the game's distance updates have spread.
fn is_supported(reader: &mut Reader<'_>, start: Pos) -> bool {
    let mut visited = HashSet::from([start]);
    let mut frontier = VecDeque::from([(start, 0u32)]);

    while let Some((pos, depth)) = frontier.pop_front() {
        for step in NEIGHBOURS {
            let next = offset(pos, step);
            if !visited.insert(next) {
                continue;
            }
            match reader.kind(next) {
                // Never decay next to an unloaded chunk; the support may be in there.
                None | Some(Kind::Support) => return true,
                Some(Kind::Leaves { .. }) if depth + 1 < MAX_SUPPORT_DISTANCE => {
                    if visited.len() >= MAX_SEARCH_NODES {
                        return true;
                    }
                    frontier.push_back((next, depth + 1));
                }
                _ => {}
            }
        }
    }
    false
}

/// Mirrors the decay branch of vanilla's `LeavesBlock.randomTick`:
/// drop the loot table, then remove the block without breaking it.
fn try_decay(
    server: &Server,
    reader: &mut Reader<'_>,
    world_name: &str,
    dimension: &str,
    pos: Pos,
) {
    let Some(Kind::Leaves {
        persistent: false,
        waterlogged,
    }) = reader.kind(pos)
    else {
        return;
    };
    if is_supported(reader, pos) {
        return;
    }

    let (x, y, z) = pos;
    // `loot … mine` reads the block, so it has to run while the leaf is still there.
    // The v0.1 API has no way to drop loot directly (v0.2's `break-block` does,
    // but release servers do not load v0.2 plugins yet). `tick` mutes the
    // feedback this command sends to ops.
    server.execute_command(
        &format!(
            "execute in {dimension} run loot spawn {} {} {} mine {x} {y} {z}",
            f64::from(x) + 0.5,
            f64::from(y) + 0.5,
            f64::from(z) + 0.5,
        ),
        CommandSender::Console,
    );
    let replacement = if waterlogged { *WATER_STATE } else { AIR_STATE };
    reader.world.set_block_state(
        BlockPos { x, y, z },
        replacement,
        BlockFlags::NOTIFY_NEIGHBORS | BlockFlags::NOTIFY_LISTENERS,
    );

    // Carry the decay through the whole unsupported leaf group, even past the
    // range that was queued when it started.
    let neighbours: Vec<(Pos, u64)> = NEIGHBOURS
        .iter()
        .map(|&step| offset(pos, step))
        .filter(|&next| {
            matches!(
                reader.kind(next),
                Some(Kind::Leaves {
                    persistent: false,
                    ..
                })
            )
        })
        .map(|next| (next, DELAY_PER_STEP_TICKS))
        .collect();
    enqueue(world_name, neighbours);
}

fn tick(server: &Server) {
    let due = {
        let mut queue = QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
        queue.tick += 1;
        let now = queue.tick;
        let mut due: Vec<(String, Pos)> = Vec::new();
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
        for item in &due {
            queue.queued.remove(item);
        }
        due
    };
    if due.is_empty() {
        return;
    }

    // No lock is held from here on: host calls can re-enter our event handlers.
    let mut by_world: BTreeMap<String, Vec<Pos>> = BTreeMap::new();
    for (name, pos) in due {
        by_world.entry(name).or_default().push(pos);
    }
    let worlds: Vec<(World, String, Vec<Pos>)> = by_world
        .into_iter()
        .filter_map(|(name, positions)| {
            server
                .get_world_by_name(&name)
                .map(|world| (world, name, positions))
        })
        .collect();
    let Some((rules, ..)) = worlds.first() else {
        return;
    };

    // `/loot` reports every drop as "Dropped N items" to all online ops. That
    // broadcast is gated on `sendCommandFeedback`, so turn it off while our
    // commands run. Setting a rule through the API sends no feedback of its own,
    // and every world shares the server's game rules, so one world is enough.
    let mute = rules.get_game_rule_bool(GameRule::SendCommandFeedback) == Some(true);
    if mute {
        rules.set_game_rule_bool(GameRule::SendCommandFeedback, false);
    }
    for (world, name, positions) in &worlds {
        let dimension = world.get_dimension();
        let mut reader = Reader::new(world);
        for &pos in positions {
            try_decay(server, &mut reader, name, &dimension, pos);
        }
    }
    if mute {
        rules.set_game_rule_bool(GameRule::SendCommandFeedback, true);
    }
}

struct BreakListener;

impl EventHandler<BlockBreakEvent> for BreakListener {
    fn handle(&self, _server: Server, event: BlockBreakEventData) -> BlockBreakEventData {
        if !event.cancelled && (is_support_name(&event.block) || is_leaves_name(&event.block)) {
            // The event carries no world. Breaks without a player (explosions,
            // fire) are still caught once the first leaf decays naturally.
            if let Some(player) = &event.player {
                let pos = event.block_pos;
                queue_around(&player.get_world(), (pos.x, pos.y, pos.z));
            }
        }
        event
    }
}

struct DecayListener;

impl EventHandler<LeavesDecayEvent> for DecayListener {
    fn handle(&self, _server: Server, event: LeavesDecayEventData) -> LeavesDecayEventData {
        if !event.cancelled {
            let pos = event.block_pos;
            queue_around(&event.target_world, (pos.x, pos.y, pos.z));
        }
        event
    }
}

struct FastLeafDecay;

impl Plugin for FastLeafDecay {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: env!("CARGO_PKG_NAME").into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Migno".into()],
            description: env!("CARGO_PKG_DESCRIPTION").into(),
            dependencies: vec![],
            permissions: vec![],
        }
    }

    fn on_load(&self, context: Context) -> pumpkin_plugin_api::Result<()> {
        // Non-blocking handlers run after all blocking ones, so they see the
        // final `cancelled` state set by protection plugins.
        context.register_event_handler(BreakListener, EventPriority::Lowest, false)?;
        context.register_event_handler(DecayListener, EventPriority::Lowest, false)?;
        context.schedule_repeating_task(1, 1, |server| tick(&server));
        tracing::info!("Fast leaf decay enabled");
        Ok(())
    }
}

register_plugin!(FastLeafDecay);
