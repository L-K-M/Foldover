#!/usr/bin/env bash
# spikes/s9-peer-pairing/run.sh: build, test and benchmark the S9
# peer-pairing spike.
#
# Usage: spikes/s9-peer-pairing/run.sh [results.json]
#
# Builds this spike's Cargo workspace in release mode, runs the
# pairing test suite (cargo test --release), then the loopback
# throughput bench, and writes a machine-readable JSON report to the
# path given as the first argument (default: results.json in the
# current directory). A short human summary goes to stdout.
#
# Exit status: 0 when the harness ran to completion. A failing test or
# a weak measurement is a recorded result, not a harness failure.
# Exit status 1 means the harness itself broke: build failure, missing
# tool, or a crashed bench.

set -euo pipefail

usage() {
    # Print this file's leading comment block as the help text.
    awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
}

case "${1-}" in
    -h | --help)
        usage
        exit 0
        ;;
esac

INVOCATION_DIR=$PWD
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)

# Resolve the results path against the invocation directory before
# moving into the spike directory.
RESULTS=${1:-results.json}
case "$RESULTS" in
    /*) ;;
    *) RESULTS="$INVOCATION_DIR/$RESULTS" ;;
esac
mkdir -p "$(dirname "$RESULTS")"

# Cargo writes build output into the spike's own (gitignored) target/.
SCRATCH="$SCRIPT_DIR/target/spike-run"
mkdir -p "$SCRATCH"

cd "$SCRIPT_DIR" || exit 1

echo "== building (cargo test --release --no-run)"
if ! cargo test --release --no-run 2>&1 | tee "$SCRATCH/build.log"; then
    echo "run.sh: harness failure: the spike workspace did not build" >&2
    exit 1
fi

echo "== running tests (cargo test --release)"
# A failing test is a measured result, not a harness failure: capture
# the status and keep going so it lands in the JSON report.
test_status=0
cargo test --release 2>&1 | tee "$SCRATCH/test.log" || test_status=$?

echo "== running bench (1 GiB loopback, 1 and 4 streams)"
if ! cargo run --release --bin bench -- --out "$SCRATCH/bench.json" 2>&1 | tee "$SCRATCH/bench.log"; then
    echo "run.sh: harness failure: the bench binary crashed" >&2
    exit 1
fi

# Assemble the JSON report and the human summary in one place.
python3 - "$SCRATCH/test.log" "$SCRATCH/bench.json" "$RESULTS" "$test_status" <<'PY'
import json
import re
import sys

test_log, bench_path, results_path, test_status = sys.argv[1:5]
passed = []
failed = []
for line in open(test_log, encoding="utf-8", errors="replace"):
    ok = re.match(r"^test (\S+) \.\.\. ok$", line)
    if ok:
        passed.append(ok.group(1))
        continue
    bad = re.match(r"^test (\S+) \.\.\. FAILED$", line)
    if bad:
        failed.append(bad.group(1))

tests = {name: "ok" for name in passed}
tests.update({name: "FAILED" for name in failed})
with open(bench_path, encoding="utf-8") as f:
    bench = json.load(f)

report = {
    "spike": "s9-peer-pairing",
    "cargo_test_exit_status": int(test_status),
    "tests": tests,
    "bench": bench,
}
with open(results_path, "w", encoding="utf-8") as f:
    json.dump(report, f, indent=2, sort_keys=True)
    f.write("\n")

total = len(passed) + len(failed)
print(f"S9 peer pairing: {len(passed)}/{total} tests passed")
for name in sorted(passed):
    print(f"  ok     {name}")
for name in sorted(failed):
    print(f"  FAILED {name}")
mib = bench["total_bytes"] / 1048576
one, four = bench["one_stream"], bench["four_streams"]
print(
    f"bench: {one['streams']} stream {one['mib_per_sec']} MiB/s, "
    f"{four['streams']} streams {four['mib_per_sec']} MiB/s "
    f"({mib:.0f} MiB loopback, TLS 1.3 mutual auth)"
)
PY

echo "results: $RESULTS"
