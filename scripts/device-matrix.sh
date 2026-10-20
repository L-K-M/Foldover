#!/usr/bin/env bash
# Run the on-disk simulation between an APFS folder and one on each file
# system Foldover meets on external disks (PLAN.md §5.2 device matrix),
# using disk images: case-sensitive APFS, HFS+, exFAT and FAT32. macOS
# only; needs no administrator rights.
#
#   scripts/device-matrix.sh [WORK_DIRECTORY]
#
# FOLDOVER_SIMULATION_CASES sets the cases per file system, and
# FOLDOVER_DEVICES (e.g. "FOFAT FOHFS") limits the run to those images.
# Images are detached on exit, however the run ends.
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "device-matrix: disk images need macOS" >&2
  exit 1
fi

work="${1:-$(mktemp -d)}"
mkdir -p "$work"
# Each image must hold the runs plus the 256 MiB free-space reserve
# every sync keeps; sparse images only take what is written.
readonly size="1g"
mounted=()

# macOS's bash 3.2 calls an empty array unbound under `set -u`.
detach() {
  for mount in ${mounted[@]+"${mounted[@]}"}; do
    hdiutil detach -quiet -force "$mount" || true
  done
}
trap detach EXIT

# attach NAME FILESYSTEM: create and mount an image at $work/NAME, which
# it records for detaching. Not run in a subshell, so the record stays.
attach() {
  local name="$1" filesystem="$2"
  hdiutil create -quiet -type SPARSE -size "$size" -fs "$filesystem" \
    -volname "$name" "$work/$name"
  mkdir -p "$work/$name"
  hdiutil attach -quiet -nobrowse -mountpoint "$work/$name" \
    "$work/$name.sparseimage"
  mounted+=("$work/$name")
}

attach FOAPFS "APFS"
left="$work/FOAPFS"
failed=()
ran=0
for target in "FOCASE:Case-sensitive APFS" "FOHFS:HFS+" "FOEXFAT:ExFAT" \
  "FOFAT:MS-DOS FAT32"; do
  name="${target%%:*}"
  filesystem="${target#*:}"
  if [[ -n "${FOLDOVER_DEVICES:-}" && " $FOLDOVER_DEVICES " != *" $name "* ]]; then
    continue
  fi
  attach "$name" "$filesystem"
  right="$work/$name"
  ran=$((ran + 1))
  echo "== APFS and $filesystem"
  if ! FOLDOVER_SIMULATION_LEFT_VOLUME="$left" \
    FOLDOVER_SIMULATION_RIGHT_VOLUME="$right" \
    cargo test --locked --release -p foldover-engine \
    -- random_edits_and_interrupted_runs; then
    failed+=("$filesystem")
  fi
done

if (( ran == 0 )); then
  echo "device-matrix: FOLDOVER_DEVICES '${FOLDOVER_DEVICES:-}' names no image" >&2
  exit 1
fi
if (( ${#failed[@]} > 0 )); then
  echo "device-matrix: failed on ${failed[*]}" >&2
  exit 1
fi
echo "device-matrix: every file system passed"
