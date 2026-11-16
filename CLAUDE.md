# fast-leaf-decay

Pumpkin WebAssembly plugin (target `wasm32-wasip2`, plugin API v0.1). One `.wasm` runs on every platform.

## Versioning — bump on every change that ships

- Any change to plugin behaviour or code that will be released needs a version bump in `Cargo.toml`. Do it as part of the change, not later.
  - Bug fix: patch (`0.1.0` → `0.1.1`)
  - New feature or config: minor (`0.1.x` → `0.2.0`)
- Check the last released version with `git tag` / `gh release list`. If the current `Cargo.toml` version is already released, bump before building anything for testing.
- After bumping, run `cargo build --release` so `Cargo.lock` updates too. CI builds with `--locked` and fails on a stale lock file.
- When targeting a new Pumpkin release, also update `PUMPKIN_VERSION` and the `tag` of `pumpkin-plugin-api` in `Cargo.toml` (they must match).

## Releasing

1. Commit the change and the version bump.
2. Tag `v<version from Cargo.toml>` (e.g. `v0.1.1`). The tag must match `Cargo.toml`; the workflow does not check.
3. Push the commit and the tag. `.github/workflows/release.yml` builds the `.wasm` and publishes the GitHub release.

Confirm with the user before committing, tagging or pushing.

## Local test build

`cargo build --release`, then copy `target/wasm32-wasip2/release/fast_leaf_decay.wasm` to
`dist/fast-leaf-decay-<version>-pumpkin-<PUMPKIN_VERSION>.wasm` (same name the release uses).
