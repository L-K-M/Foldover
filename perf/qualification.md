# End-to-end qualification harness

Build the engine example in release mode, then generate a new fixture:

```sh
cargo build --release --locked -p foldover-engine --example performance
FOLDOVER_PERF_ROOT="$PWD/build/foldover-perf-example"
mkdir -p "$FOLDOVER_PERF_ROOT"
python3 perf/make-corpus.py "$FOLDOVER_PERF_ROOT/source" --files 10000
mkdir "$FOLDOVER_PERF_ROOT/target"
python3 perf/run-qualification.py --runner target/release/examples/performance \
  --state "$FOLDOVER_PERF_ROOT/state" \
  --left "$FOLDOVER_PERF_ROOT/source" \
  --right "$FOLDOVER_PERF_ROOT/target" \
  --iterations 3 --label development --output "$FOLDOVER_PERF_ROOT/results"
```

Use an absolute `sftp://user@host/path` root for real LAN runs. Authentication
uses the user's ordinary noninteractive OpenSSH configuration. Create a
dedicated remote fixture directory first. The runner accepts only roots and
state paths containing `foldover-perf`, refuses an existing mismatched catalog,
and never opens the installed agent's state. It mirrors left to right, keeps
normal archives/leases and approves its freshly computed synthetic plan. Never
point it at user data. Failed/conflicting/incomplete runs exit unsuccessfully.

The `foldover-perf` marker is a naming convention on a path-component prefix,
not a security sandbox: it does not prove filesystem ownership and does not
defend against a hostile concurrent rename. `make-corpus.py` resolves the
physical path before checking the marker and writes only to that resolved
path; the runner checks state and parsed local/SFTP path components
lexically, rejects `..`, and never inspects the SFTP user or host.

The tool measures actual endpoint open, readiness, each observation, alignment,
planning, input load, preview, snapshot export/store, apply, cumulative commit
time and total duration. `RunProgress::measured` is a default diagnostic hook:
normal UI consumers ignore it, and callbacks stay on the run thread. The example
uses the same public engine/store APIs as the agent; it measures storage time,
not contention on the service's mutex. Test concurrent service activity separately.

Results retain a TSV trace, `/usr/bin/time` resource report including peak RSS,
and JSON host/revision/load/phase samples with RSS and CPU readings. Sampling and
reporting add overhead to total time. CPU percentages are point-in-time snapshots
reported by the OS for the whole process, not per-phase CPU measurements. Kernel
peak RSS covers transient peaks between samples. Phases can overlap only when
the implementation supports it; do not add overlapping phase times to predict
total duration.
For saved binaries, pass `--revision` with their source commit rather than the
current checkout's commit. The binary's SHA-256 is recorded independently.

Use 10k files for development, 100k for comparisons and 1M for qualification.
Vary `--bytes` (1/8/32 KiB and a separate large-file set) and `--width` for flat
and nested trees. Corpus bytes are deterministic, unique, incompressible SHAKE
output so matching fixtures can be generated independently on each host. First
copy uses an empty target. No-change runs reuse the same state and roots; edits,
deletions and permission changes must affect only synthetic entries. Record all
fixture mutations, NAS hardware/filesystem, RTT and cache conditions separately.

Run three paired before/after samples with matching fixtures and source revisions.
Keep initial population and warm no-change runs separate. In particular, a local
`sftp-server` run omits SSH encryption, authentication and LAN latency. Do not
describe it as NAS qualification. Keep existing performance budgets unchanged.

Generate only bounded corpora, monitor free space, and remove only the exact
uniquely named fixture directories created for the experiment after preserving
results. Neither corpus generator nor qualification runner cleans arbitrary paths.

## Service preview and status qualification

The opt-in `performance_tests::million_file_service_preview` test exercises
`Service::handle`, snapshot persistence and a held preview while polling
status. It accepts only an explicit synthetic catalog with one local pair;
both roots must be inside the catalog's parent fixture directory. It creates
no corpus. Seed a matching, no-change pair first with the engine runner.
Clone the closed catalog before forcing fingerprint expiry, and record any
file-time/header changes as fixture provenance. Keep the source/target roots
and the copied catalog under the same `foldover-perf` container.

Build the release test executable and use Cargo's reported `executable` path
for the `foldover_service` test target (this honors custom target directories):

```sh
cargo test --release -p foldover-service --lib --no-run --message-format=json
FOLDOVER_SERVICE_TEST=/absolute/path/reported/by/cargo
FOLDOVER_PERF_SERVICE_STATE="$FOLDOVER_PERF_ROOT/state-service" \
  /usr/bin/time -l "$FOLDOVER_SERVICE_TEST" --ignored --exact \
  performance_tests::million_file_service_preview --nocapture
```

Measure the test executable, not the Cargo wrapper. The macOS resource report
gives kernel peak RSS; the test prints preview duration, maximum handler
latency and maximum timer lateness. A successful test asserts a no-change
preview; compare its RSS separately with the unchanged budget. This direct
service test does not start an installed daemon, IPC listener, scheduler or
peer discovery. It changes no authentication policy or home-directory setting.
See [qualification results](qualification-results.md) for the real fixtures,
retained failed samples and limits.

## Diagnostic units and focused probes

The engine runner emits duration records as `metric`/`phase` with nanoseconds,
and `counter` records with unitless integer `value` fields in the JSON summary.
`PlannedLogicalBytes` and `AppliedLogicalBytes` count logical file data reported
by the engine, not encrypted network traffic or resumed-transfer payloads.
Use the framed relay for SFTP request/response and DATA-byte counts.

Two more opt-in tests use the same explicitly marked local catalog and the
same release test executable:

```sh
FOLDOVER_PERF_SERVICE_STATE="$FOLDOVER_PERF_ROOT/state-service" \
  /usr/bin/time -l "$FOLDOVER_SERVICE_TEST" --ignored --exact \
  performance_tests::million_file_service_input_lock --nocapture
FOLDOVER_PERF_SERVICE_STATE="$FOLDOVER_PERF_ROOT/state-service" \
  /usr/bin/time -l "$FOLDOVER_SERVICE_TEST" --ignored --exact \
  performance_tests::million_file_service_scan_cancellation --nocapture
```

The input probe times the real capture and mount lookup under the service
engine mutex, then decodes outside it. A second caller contends for that
mutex; the barrier handshake is excluded from capture time. This measures
input-lock contention, not a complete second-pair RPC or installed daemon.
The cancellation probe waits for at least 10,000 checked entries, sends the
real service cancellation request and polls completion every 5 ms. It covers
local observation cancellation; input decoding, transfers and stalled remote
I/O have separate behavior and are not qualified by this sample.
