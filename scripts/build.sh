#!/usr/bin/env bash
# Builds the plugin inside a Pumpkin checkout, the same way Pumpkin's release
# workflow builds the official server binary.
#
# Usage: scripts/build.sh <pumpkin checkout> [cargo profile flag]
#   scripts/build.sh ../Pumpkin            # release build (matches official binaries)
#   scripts/build.sh ../Pumpkin --debug    # debug build, for a server built in debug
#
# The checkout must be at the tag in PUMPKIN_VERSION, and the Rust toolchain
# must be the one in RUST_VERSION; the script checks both.
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
PUMPKIN="$(cd "${1:?usage: build.sh <pumpkin checkout> [--debug]}" && pwd)"
MODE="${2:-}"
TAG="$(tr -d '[:space:]' < "$HERE/PUMPKIN_VERSION")"
RUST="$(tr -d '[:space:]' < "$HERE/RUST_VERSION")"

die() { echo "error: $*" >&2; exit 1; }

[ "$(git -C "$PUMPKIN" rev-parse HEAD)" = "$(git -C "$PUMPKIN" rev-parse "$TAG^{commit}")" ] \
    || die "$PUMPKIN is not checked out at $TAG"
export RUSTUP_TOOLCHAIN="$RUST"
rustc -V | grep -q "^rustc $RUST " || die "need rustc $RUST (rustup toolchain install $RUST)"

# Copy the plugin into the workspace and register it as a member.
DEST="$PUMPKIN/plugins/fast-leaf-decay"
rm -rf "$DEST"
mkdir -p "$DEST"
cp -r "$HERE/Cargo.toml" "$HERE/src" "$DEST/"
if ! grep -q '"plugins/fast-leaf-decay"' "$PUMPKIN/Cargo.toml"; then
    perl -0pi -e 's/^members = \[/members = [\n    "plugins\/fast-leaf-decay",/m' "$PUMPKIN/Cargo.toml"
fi

# Build the whole workspace like the release workflow (`cargo build --release`),
# so cargo unifies features exactly as it did for the official binary.
#
# The official workspace is built first and the plugin second, so the fat-LTO
# links of the server and the plugin don't run at the same time. Running them
# together ran a GitHub runner out of memory. The plugin only uses features the
# server already enables, so both steps resolve the same features and the
# second step reuses everything the first one built.
cd "$PUMPKIN"
if [ "$MODE" = "--debug" ]; then
    PROFILE_FLAG=""
    PROFILE=debug
else
    PROFILE_FLAG="--release"
    PROFILE=release
fi
# shellcheck disable=SC2086 # PROFILE_FLAG is empty or one word
cargo build $PROFILE_FLAG --workspace --exclude fast-leaf-decay
# shellcheck disable=SC2086
cargo build $PROFILE_FLAG

for f in fast_leaf_decay.dll libfast_leaf_decay.so libfast_leaf_decay.dylib; do
    if [ -f "target/$PROFILE/$f" ]; then
        echo "$PUMPKIN/target/$PROFILE/$f"
    fi
done
