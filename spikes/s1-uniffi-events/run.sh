#!/usr/bin/env bash
# spikes/s1-uniffi-events/run.sh — build and run the S1 UniFFI
# event-bridge benchmark on macOS and write its JSON report.
#
# Usage: spikes/s1-uniffi-events/run.sh <results.json>
# Set S1_SOAK_SECONDS=3600 for the one-hour memory and lifecycle probe.
#
# What it does:
#   1. Builds the s1-ffi release staticlib for aarch64-apple-darwin.
#   2. Generates the Swift bindings from that staticlib with
#      s1-bindgen, into swift/S1BenchCore/ (the same artifact the app
#      links, so bindings cannot describe a different binary).
#   3. Wraps the staticlib and generated headers in
#      S1BenchCoreFFI.xcframework.
#   4. Runs `swift run -c release Bench <results.json>`: the bench
#      prints a short human summary on stdout and writes the JSON.
#      Its exit code is 0 even when measurements miss their targets;
#      misses land in the JSON, not in this script's status.
#
# Requirements: the pinned Rust toolchain (rust-toolchain.toml, which
# lists the aarch64-apple-darwin target), Xcode with xcodebuild and
# the Swift toolchain.
set -euo pipefail

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
    # The header comment block is the help text.
    sed -n '2,/^$/p' "$0" | sed -e 's/^# \{0,1\}//' -e '/^$/d'
    exit 0
fi

if [ $# -ne 1 ]; then
    echo "run.sh: expected one argument, the results JSON path" >&2
    exit 2
fi

RESULTS_JSON="$1"
SPIKE_DIR="$(cd "$(dirname "$0")" && pwd)"
CRATE="s1_ffi"
TARGET="aarch64-apple-darwin"
DEPLOYMENT_TARGET="26.0"
CORE_DIR="$SPIKE_DIR/swift/S1BenchCore"
GEN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/s1-bindings.XXXXXX")"
trap 'rm -rf "$GEN_DIR"' EXIT

cd "$SPIKE_DIR"

# 1. The release staticlib for arm64 macOS. MACOSX_DEPLOYMENT_TARGET
# keeps dependency C code (if any) from targeting a newer SDK than
# the package's minimum.
MACOSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" \
    cargo build --release --target "$TARGET"
STATIC_LIB="$SPIKE_DIR/target/$TARGET/release/lib${CRATE}.a"
if [ ! -f "$STATIC_LIB" ]; then
    echo "run.sh: expected staticlib at $STATIC_LIB" >&2
    exit 1
fi

# 2. The Swift bindings. The bindgen reads UniFFI's metadata from the
# staticlib itself — the artifact Swift is about to link. `--config`
# names the Swift module and the FFI module the modulemap declares.
cargo run --release --bin s1-bindgen -- \
    --swift-sources --headers --modulemap \
    --module-name S1BenchCoreFFI \
    --modulemap-filename module.modulemap \
    --config "$SPIKE_DIR/swift-bindings.toml" \
    "$STATIC_LIB" "$GEN_DIR"

mkdir -p "$CORE_DIR/Sources/S1BenchCore" "$CORE_DIR/Headers"
for f in "$GEN_DIR"/*; do
    case "$f" in
        *.swift) cp "$f" "$CORE_DIR/Sources/S1BenchCore/" ;;
        *)       cp "$f" "$CORE_DIR/Headers/" ;;
    esac
done

# 3. The XCFramework the package's binaryTarget links.
rm -rf "$CORE_DIR/S1BenchCoreFFI.xcframework"
xcodebuild -create-xcframework \
    -library "$STATIC_LIB" \
    -headers "$CORE_DIR/Headers" \
    -output "$CORE_DIR/S1BenchCoreFFI.xcframework"

# 4. Build and run the benchmark under the strict flags; it prints
# the human summary.
cd "$SPIKE_DIR/swift"
# Swift Testing re-enters this executable's CLI on macOS; use XCTest discovery.
swift test -c release --disable-swift-testing
swift run -c release Bench "$RESULTS_JSON"

if [ ! -f "$RESULTS_JSON" ]; then
    echo "run.sh: Bench wrote no results to $RESULTS_JSON" >&2
    exit 1
fi
