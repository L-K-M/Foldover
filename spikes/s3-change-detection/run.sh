#!/bin/bash
# run.sh <output.json>: build and run the S3 change-detection spike.
#
# Builds the spike harness in release mode and runs it. The harness probes
# what a non-app process gets from macOS FSEvents and DiskArbitration: per-
# operation item flags and delivery latency, event-ID replay after a stored
# checkpoint, device-relative streams as an unprivileged user, per-volume
# FSEvents UUIDs, attach/detach of APFS and ExFAT disk images, DiskArbitration
# mount callbacks on a CFRunLoop, and a 100,000-file burst. Results are
# written as JSON to <output.json>; a human summary prints on stdout.
#
# Needs no sudo. hdiutil runs as the runner user.
#
# Usage: spikes/s3-change-detection/run.sh <output.json>
set -euo pipefail

print_help() {
    # The header comment block above is the help text.
    grep '^#' "$0" | grep -v '^#!' | sed 's/^#\s\{0,1\}//'
}

if [ $# -ne 1 ]; then
    print_help
    exit 64
fi
case "$1" in
    -h|--help)
        print_help
        exit 0
        ;;
esac

SPIKE_DIR="$(cd "$(dirname "$0")" && pwd)"

cargo build --release --locked --manifest-path "$SPIKE_DIR/Cargo.toml"
"$SPIKE_DIR/target/release/spike-s3-change-detection" "$1"
