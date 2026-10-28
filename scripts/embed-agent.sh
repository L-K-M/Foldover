#!/usr/bin/env bash
# Build and ad-hoc-sign the agent/CLI inside an Xcode build product,
# and write the bundle's third-party license notices.
# Usage: bash scripts/embed-agent.sh <Foldover.app>
# Requires macOS 27, Xcode 27, Python 3 and the pinned Rust toolchain.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $# -ne 1 || "$1" != */Foldover.app ]]; then
  echo "usage: bash scripts/embed-agent.sh <Foldover.app>" >&2
  exit 2
fi
APP="$1"
TARGET="aarch64-apple-darwin"
BUNDLE_ID="ch.lkmc.foldover"
export PATH="$HOME/.cargo/bin:$PATH"

MACOSX_DEPLOYMENT_TARGET=27.0 cargo build --manifest-path "$ROOT/Cargo.toml" \
  --locked --release --target "$TARGET" -p foldoverd -p foldover-cli
TARGET_DIR="$(cargo metadata --manifest-path "$ROOT/Cargo.toml" --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"

HELPERS="$APP/Contents/Library/LaunchServices"
AGENTS="$APP/Contents/Library/LaunchAgents"
mkdir -p "$HELPERS" "$AGENTS"
cp "$TARGET_DIR/$TARGET/release/foldoverd" "$HELPERS/foldoverd"
cp "$TARGET_DIR/$TARGET/release/foldover" "$HELPERS/foldover"
cp "$ROOT/packaging/macos/$BUNDLE_ID.agent.plist" "$AGENTS/"

# Notices for the Rust crates and the Rust standard library linked into
# the app (FoldoverCoreFFI staticlib) and both helpers. Written here so
# Xcode's signature covers them. The generator fails when a linked crate
# has no license text; licenses/rust-overrides.json records the gaps.
RESOURCES="$APP/Contents/Resources"
mkdir -p "$RESOURCES"
python3 "$ROOT/scripts/rust-notices.py" --manifest-path "$ROOT/Cargo.toml" \
  -p foldoverd -p foldover-cli -p foldover-ffi --target "$TARGET" \
  --title Foldover --overrides "$ROOT/licenses/rust-overrides.json" \
  --out "$RESOURCES/Third-Party-Notices.txt" \
  --rust-std "$RESOURCES/Rust-Licenses.html"

for executable in foldoverd foldover; do
  identifier="$BUNDLE_ID.cli"
  [[ "$executable" == foldoverd ]] && identifier="$BUNDLE_ID.agent"
  codesign --force --sign - --timestamp=none --options runtime \
    --identifier "$identifier" "$HELPERS/$executable"
  codesign --verify --strict "$HELPERS/$executable"
done

# Service Management retains an ad-hoc helper's old exact code constraint
# across unregister/register. A code-hash label gives this helper a fresh
# constraint without changing the signing identifier used by private IPC.
python3 "$ROOT/scripts/agent-plist.py" stamp "$HELPERS/foldoverd" \
  "$AGENTS/$BUNDLE_ID.agent.plist"
