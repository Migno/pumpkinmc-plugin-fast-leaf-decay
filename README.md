# pumpkinmc-plugin-fast-leaf-decay

A native plugin for the [Pumpkin](https://pumpkinmc.org) Minecraft server. When a tree is chopped down, its leaves decay within about a second instead of over several minutes.

**Supported server:** Pumpkin `0.2.2+26.3-26.60`, the official release binaries from <https://pumpkinmc.org/download/>.

## Install

1. Download the file for your server's platform from the latest release:

   | Server binary | Plugin file |
   | --- | --- |
   | `pumpkin-X64-Linux`, `pumpkin-X64-v3-Linux` | `…-X64-Linux.so` |
   | `pumpkin-ARM64-Linux` | `…-ARM64-Linux.so` |
   | `pumpkin-X64-Windows.exe`, `pumpkin-X64-v3-Windows.exe` | `…-X64-Windows.dll` |
   | `pumpkin-ARM64-Windows.exe` | `…-ARM64-Windows.dll` |
   | `pumpkin-ARM64-macOS` | `…-ARM64-macOS.dylib` |

2. Put it into the server's `plugins/` folder and restart. The log shows `Loaded fast-leaf-decay (0.1.0)`.

The plugin only works with the exact Pumpkin release it was built for. A native plugin shares Rust types with the server binary, so on any other build (another release, nightly, canary or your own build) it can crash the server instead of failing cleanly. If your host auto-updates Pumpkin, pin it to the supported release.

## How it works

- When a player breaks a log or a leaf block, or a leaf decays naturally, the plugin queues the leaves connected to that spot within 6 steps.
- Each queued leaf is checked after `4 + 2 × steps` ticks, so the tree clears from the cut outward.
- A leaf decays only if it is not persistent (player-placed leaves never decay) and no log is within 6 steps through leaves. This is vanilla's rule, computed directly instead of waiting for the `distance` updates to spread.
- A decay fires `LeavesDecayEvent`, so protection plugins can still cancel it. It then drops the leaf's loot table (saplings, sticks, apples), plays the block-break particles and sound, and replaces the leaf with air, or water if the leaf was waterlogged.
- Leaves next to unloaded chunks are left alone. At most 512 leaves decay per tick.

Breaks that have no player (explosions, fire) don't trigger the plugin directly, because `BlockBreakEvent` doesn't say which world they happened in. Those trees speed up as soon as their first leaf decays naturally.

## Building

The crate is built as a member of the Pumpkin workspace at the supported tag, in the same way Pumpkin's release workflow builds the official binary: same compiler, and a whole-workspace `cargo build --release`. That gives the plugin the same dependency versions, features and type layouts as the server.

- `PUMPKIN_VERSION` is the Pumpkin release tag to build against.
- `RUST_VERSION` is the compiler that release was built with (from its release workflow log).

```bash
git clone https://github.com/Pumpkin-MC/Pumpkin.git
git -C Pumpkin checkout "$(cat PUMPKIN_VERSION)"
git -C Pumpkin submodule update --init --recursive
rustup toolchain install "$(cat RUST_VERSION)" --profile minimal
scripts/build.sh Pumpkin            # release build, for the official binaries
scripts/build.sh Pumpkin --debug    # debug build, for a debug server built from the same checkout
```

`scripts/build.sh` copies the plugin into `Pumpkin/plugins/fast-leaf-decay` and adds it to the workspace members. Use a throwaway checkout for this.

### Releases

`.github/workflows/release.yml` builds every platform on the same runners Pumpkin uses. Pushing a `v*` tag publishes a GitHub release with all files and a `checksums.sha256`. Running the workflow manually only produces artifacts.

To support a new Pumpkin release: update `PUMPKIN_VERSION`, set `RUST_VERSION` from that release's workflow log (the `rustup update stable` step), fix anything the API changed, then tag a new version.

## Tuning

These are constants at the top of `src/lib.rs`:

| Constant | Default | Meaning |
| --- | --- | --- |
| `BASE_DELAY_TICKS` | 4 | Ticks from the break until the first leaf decays |
| `DELAY_PER_STEP_TICKS` | 2 | Extra ticks for each step away from the break |
| `MAX_DECAYS_PER_TICK` | 512 | Cap on leaves decayed per tick |
| `MAX_SEARCH_NODES` | 4096 | Cap on blocks visited by one search |

## License

GPL-3.0, because the plugin links against the GPL-licensed Pumpkin server crates.
