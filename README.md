<p align="center">
  <img src="assets/banner.png" alt="Fast Leaf Decay, a plugin for the Pumpkin Minecraft server" width="100%">
</p>

<p align="center">
  <a href="https://github.com/Migno/pumpkinmc-plugin-fast-leaf-decay/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/Migno/pumpkinmc-plugin-fast-leaf-decay?label=release&color=ff7a1a"></a>
  <img alt="Pumpkin 0.2.2+26.3-26.60" src="https://img.shields.io/badge/Pumpkin-0.2.2%2B26.3--26.60-ffb347">
  <img alt="Minecraft 26.3" src="https://img.shields.io/badge/Minecraft-26.3-4caf50">
  <img alt="WebAssembly" src="https://img.shields.io/badge/plugin-WebAssembly-654ff0">
  <a href="#license"><img alt="License MIT or Apache-2.0" src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue"></a>
</p>

# Fast Leaf Decay for Pumpkin

Chop down a tree on your [Pumpkin](https://pumpkinmc.org) server and its leaves are gone within about a second, not after several minutes of slow random decay. Saplings, sticks and apples drop just as they do in vanilla.

- ⚡ **Fast:** leaves clear from the cut outward, so a normal tree is gone in under a second.
- 🌱 **Vanilla loot:** every leaf rolls its normal loot table, so sapling farming keeps working.
- 🧱 **Vanilla rules:** a leaf only decays if no log is within 6 blocks through leaves, the same rule vanilla uses. Player-placed leaves never decay.
- 🧩 **One file, every platform:** a single WebAssembly plugin runs on Windows, Linux and macOS, on x64 and ARM.
- 🛡️ **Safe:** it runs in Pumpkin's Wasm sandbox and only uses the stable plugin API, so it can't crash the server.
- 🤫 **Quiet:** no chat messages, not even for ops. (The server console still logs each drop as "Dropped N items", because the plugin API has no way to drop loot without a command.)

## Install

1. Check your server version. This release supports **Pumpkin `0.2.2+26.3-26.60`**, the official release from [pumpkinmc.org/download](https://pumpkinmc.org/download/).
2. Download `fast-leaf-decay-*.wasm` from the [latest release](https://github.com/Migno/pumpkinmc-plugin-fast-leaf-decay/releases/latest).
3. Put it into the server's `plugins/` folder and restart. The log shows:

   ```
   WARN Plugin './plugins/fast-leaf-decay-….wasm' is unsigned. Unsigned plugins may come from untrusted sources.
   INFO Fast leaf decay enabled
   INFO Loaded fast-leaf-decay (0.1.0)
   ```

   The "unsigned" warning appears for every plugin that isn't from the Pumpkin marketplace and is harmless.

That's all. There is nothing to configure, and no permissions are requested.

## How it works

1. A player breaks a log or a leaf block, or a leaf decays on its own.
2. The plugin collects the non-persistent leaves connected to that spot within 6 steps. These are the only leaves the change can leave without support.
3. Each leaf is checked after `4 + 2 × distance` ticks, so the tree clears outward from the cut. Every decayed leaf also queues its leaf neighbours, so the decay reaches the whole group of unsupported leaves.
4. A leaf with no log within 6 steps through leaves decays the way vanilla's `LeavesBlock.randomTick` does: its loot table drops (through `/loot … mine`, which uses the same loot context as vanilla decay), then the block becomes air, or water if it was waterlogged.

The plugin works out a leaf's support itself rather than waiting for the game's `distance` updates to spread through the tree. That's why it can act right away without decaying the wrong leaves.

**Limits:**
- Explosions and fire don't trigger the plugin directly, because Pumpkin's `BlockBreakEvent` doesn't say which world they happened in. Those trees still speed up as soon as their first leaf decays naturally.
- The plugin API can't fire `LeavesDecayEvent`, so protection plugins can't cancel the plugin's decays. Breaks they cancel never start one.
- Leaves next to unloaded chunks are left alone.

## Compatibility

| Plugin | Pumpkin release | Minecraft |
| --- | --- | --- |
| 0.1.0 | `0.2.2+26.3-26.60` | Java 26.3 / Bedrock 1.26.60 |

The plugin uses Pumpkin's Wasm plugin API 0.1 (WASI 0.2). It loads on any Pumpkin build that hosts that API, but it is tested against the release above.

## Building from source

```bash
rustup target add wasm32-wasip2
cargo build --release
# -> target/wasm32-wasip2/release/fast_leaf_decay.wasm
```

`PUMPKIN_VERSION` names the Pumpkin release the plugin targets. The plugin SDK (`pumpkin-plugin-api`) is taken from that release tag.

### Tuning

These are constants at the top of [`src/lib.rs`](src/lib.rs):

| Constant | Default | Meaning |
| --- | --- | --- |
| `BASE_DELAY_TICKS` | 4 | Ticks from the break until the first leaf decays |
| `DELAY_PER_STEP_TICKS` | 2 | Extra ticks for each step away from the break |
| `MAX_DECAYS_PER_TICK` | 256 | Cap on leaves decayed per tick |
| `MAX_SEARCH_NODES` | 4096 | Cap on blocks visited by one search |

### Releasing

[`.github/workflows/release.yml`](.github/workflows/release.yml) builds the `.wasm` file. Pushing a `v*` tag publishes a GitHub release with the file and a `checksums.sha256`. Running the workflow manually only produces an artifact.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.

This is a community plugin and is not affiliated with the Pumpkin project.
