#!/usr/bin/env bash
# Build and ad-hoc-sign the agent/CLI inside an Xcode build product.
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

for executable in foldoverd foldover; do
  identifier="$BUNDLE_ID.cli"
  [[ "$executable" == foldoverd ]] && identifier="$BUNDLE_ID.agent"
  codesign --force --sign - --timestamp=none --options runtime \
    --identifier "$identifier" "$HELPERS/$executable"
  codesign --verify --strict "$HELPERS/$executable"
done
