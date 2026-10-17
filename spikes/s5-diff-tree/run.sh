#!/bin/bash
# s5-diff-tree: can the Preview's diff tree hold a 200,000-row plan,
# streamed while the scan runs, at 60 fps with flat memory?
#
# Usage: spikes/s5-diff-tree/run.sh <report.json>
#
# Builds the DiffTreeBench app with XcodeGen and xcodebuild (Release,
# ad-hoc signed), launches it with `-runBench <report.json>` through
# `open -W` (both tree implementations measured in fresh windows),
# writes the machine-readable report to <report.json> and prints a
# short summary to stdout. Requires macOS 27, Xcode 27 and xcodegen on
# PATH (CI installs the pinned release). A measured miss is data for
# the ADR, not a script failure: the script fails only when the
# harness itself breaks: no report, or a report that is not JSON.

set -euo pipefail

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
    sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
fi

if [ $# -ne 1 ]; then
    echo "usage: $0 <report.json>" >&2
    exit 64
fi

# The app is launched by `open` (launchd), not this shell: a relative
# report path would resolve against the app's own working directory.
mkdir -p "$(dirname "$1")"
REPORT_PATH="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUILD_DIR="$SCRIPT_DIR/build"
DERIVED_DATA="$BUILD_DIR/DerivedData"
APP="$DERIVED_DATA/Build/Products/Release/DiffTreeBench.app"
APP_LOG="$BUILD_DIR/app.log"

command -v xcodegen >/dev/null 2>&1 || {
    echo "run.sh: xcodegen not found on PATH" >&2
    exit 69
}
command -v xcodebuild >/dev/null 2>&1 || {
    echo "run.sh: xcodebuild not found on PATH" >&2
    exit 69
}

cd "$SCRIPT_DIR"
mkdir -p "$BUILD_DIR" "$(dirname "$REPORT_PATH")"
rm -f "$REPORT_PATH" "$APP_LOG"

xcodegen generate --spec project.yml

xcodebuild -project DiffTreeBench.xcodeproj \
    -scheme DiffTreeBench \
    -destination 'platform=macOS,arch=arm64' \
    -derivedDataPath "$DERIVED_DATA" \
    -configuration Release \
    CODE_SIGN_IDENTITY=- \
    build

# `open -W` blocks until the app quits; --stdout/--stderr capture the
# app's summary and progress lines. The app's own watchdog bounds this
# even when an implementation wedges, so a nonzero `open` result is
# still followed by a report check, not an immediate failure.
open -W -n -a "$APP" \
    --stdout "$APP_LOG" --stderr "$APP_LOG" \
    --args -runBench "$REPORT_PATH" \
    -LogForEachSlowPath YES || true

if [ -f "$APP_LOG" ]; then
    cat "$APP_LOG"
fi

# Harness failure is the only failure: a verdict of "fail" or "wedged"
# is a measurement the ADR records, not a broken run.
if [ ! -f "$REPORT_PATH" ]; then
    echo "run.sh: the app produced no report at $REPORT_PATH" >&2
    exit 1
fi
if ! python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$REPORT_PATH" 2>/dev/null; then
    echo "run.sh: the report at $REPORT_PATH is not valid JSON" >&2
    exit 1
fi

echo "run.sh: report written to $REPORT_PATH"
