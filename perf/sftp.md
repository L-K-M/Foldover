# SFTP protocol measurements

`sftp-relay.py` forwards framed requests and responses without changing their
contents. It records JSON lines containing packet type, bytes, request ID,
outstanding count, process ID, elapsed time and DATA payload bytes. Use an
isolated fixture: the log describes the workload, and the server command runs
with your ordinary privileges.

```sh
python3 perf/sftp-relay.py --log events.jsonl -- /usr/libexec/sftp-server
```

The client must speak SFTP on the relay's standard streams. The endpoint
tests' `counted` fixture demonstrates spawning it through `Session::spawn`.
For a real network measurement, replace the server command with
`ssh -o BatchMode=yes -s HOST sftp`. An optional `--delay-ms` delays every
response frame; that measures per-message sensitivity, not actual network RTT.

## Relay packet counts over SSH

`sftp_probe` can count the SFTP packets a read actually uses. Add
`--relay RELAY_SCRIPT LOG_PATH` to spawn the relay through `/usr/bin/python3`
with `Session::spawn`; the relay forwards the same fixed `/usr/bin/ssh`
transport arguments as `Session::connect` (`BatchMode=yes`,
`ServerAliveInterval=15`, `ServerAliveCountMax=4`, `ConnectTimeout=60`,
`-l USER`, `-s -- HOST sftp`) and logs every framed request and response. This
is a benchmark-only path and does not change `Session::connect` or its trusted
SSH path.

```sh
cargo run --locked --release -p foldover-net --example sftp_probe -- \
  USER@HOST /absolute/foldover-perf/fixture 100 bounded \
  --relay perf/sftp-relay.py events-bounded.jsonl
cargo run --locked --release -p foldover-net --example sftp_probe -- \
  USER@HOST /absolute/foldover-perf/fixture 100 unknown \
  --relay perf/sftp-relay.py events-unknown.jsonl
```

The CSV keeps the per-round `*_ns` latency columns and adds `data_bytes`, the
payload bytes the read delivered. The probe still stats the fixture before and
after every round and fails if it changed. In each JSONL log, response events
with `"kind":103` are `SSH_FXP_DATA`; count those per run to compare bounded
versus unknown read packet counts. Keep the target, fixture, rounds and host
load with the result, and compare at least three paired runs. No run is
recorded in this revision: this only documents the invocation.

Run the deterministic request budgets with:

```sh
cargo test --locked -p foldover-net known_length_read_packet_budget -- --nocapture
cargo test --locked -p foldover-endpoint unchanged_staged_metadata_uses_one_stat -- --nocapture
cargo test --locked -p foldover-endpoint archive_parent_reuse -- --nocapture
```

The unchanged staged-metadata regression failed before the fix with two
LSTATs instead of one. The final version now reuses the attributes read by
metadata handling only when no mutation followed them. A SETSTAT still needs
a final read-back; `changed_staged_metadata_reads_back_the_final_state` checks
both that request and equality with the server's final stored version.

The archive-parent regression failed because each repeated sibling issued
four refused MKDIRs for `.foldover/archive/run/parent`, followed by four
LSTATs. Stat-first reuse reduces that to four LSTATs. Initial missing parents
cost an extra stat per component, amortized across siblings. Each component
is still checked, including an external directory-to-symlink substitution;
there is no cache claiming a directory stayed unchanged without checking it.

These are request-count measurements against a local OpenSSH server, not
complete LAN speedups. Keep target, corpus, source revision, host load and
cache conditions with timing results, and compare at least three paired runs.

## LAN no-change qualification runs

These runs compare saved binaries on the same warm NAS corpus over SSH. The
host was a Mac Studio (M4 Max, 14 cores, macOS 27.0.1) shared with other
agents, so the varying load averages are not an idle qualification. Each
corpus holds 8 KiB deterministic files, 1,000 per folder. Every run reads and
hashes the full content of every file and reports no changes; SFTP stat is
never trusted as proof a file is unchanged. Each session is a separate `ssh`
process; SSH multiplexing is not assumed. ICMP is blocked on this network, so
no RTT is claimed. The source RPC probe's LSTAT/open/close medians of 5-6 ms
are RPC times, not network RTT.

| Run | Binary | Revision | Remote observe, three pairs (s) | Median (s) |
|---|---|---|---|---|
| 1k, four readers | `bounded-a6` | `00af7fe` | 14.118, 13.792, 14.485 | 14.118 |
| 1k, eight readers | `eight-readers-a6` | `19929ac` | 7.021, 7.408, 7.350 | 7.350 |
| 10k, four readers | `bounded-a6` | `00af7fe` | 130.501, 134.597, 133.013 | 133.013 |
| 10k, eight readers | `eight-readers-a6` | `19929ac` | 69.623, 70.548, 76.648 | 70.548 |
| 10k, eight readers, serial finish | `eight-readers-a6` | `19929ac` | 68.678, 67.586, 86.364 | 68.678 |
| 10k, eight readers, one-write finish | `batched-eight-a6` | `718ffa8` | 56.158, 56.135, 54.777 | 56.135 |

| Binary | Source revision | Binary SHA-256 |
|---|---|---|
| `bounded-a6` | `00af7fe1c986188dda5019a4438b78b58d1127d4` | `7f0eb1f6a30b1ff5ee6ebfdf661a90d9a2cf338d2d81cbe0910a6882d4172190` |
| `eight-readers-a6` | `19929ac3776365e26f0b455c002f319ce60c5206` | `f3695d91571438615a53337eddd813e435b102ed17d9746f7eb65024ab647bfa` |
| `batched-eight-a6` | `718ffa8` | `934202cf58e73ad5a6ff78330a3f33a931544ff2bbd0a1e6b96fb252e0948d16` |
| `pipelined-eight-a6` (rejected) | `606face` | `f82d00135c8b3e3e2c80ff32ac05e7a6ef353e3a49da129767e50ff90bf44cfd` |

The paired runs are retained in the `threads1k-paired`, `threads10k-paired`
and `batch10k-paired` artifact directories as per-run JSON summaries, TSV
traces and RSS/CPU samples. All eighteen runs completed with return code 0
and no failures. The four- and eight-reader pairs are separate three-pair
sets, as are the serial and one-write finish pairs; the gains are not
additive. The remote observation is the phase that reads and hashes every
file; local observation and apply times vary with host load and are not
compared here.

A two-write finish variant (`pipelined-eight-a6`, `606face`) was measured and
rejected as noisy and regressive: its 10k remote observations were 85.206,
57.436 and 56.373 s against 69.571, 71.202 and 71.547 s for the same
eight-reader baseline, and a rerun with the SSH sampler still showed two
86-second outliers (86.234, 85.765, 58.008 s). It was not promoted.

The attempted 100k paired comparison stopped when its first baseline
lost SSH; that failed sample is retained. The 1M NAS diagnostic also failed after an SSH disconnect.
These LAN qualifications are incomplete. The [qualification results](qualification-results.md)
record completed local memory cases and the retained failures.

Reproduce with the isolated harness in [qualification.md](qualification.md).
Generate an 8 KiB, 1,000-per-folder corpus, create an empty target, and run
three paired no-change iterations against your own `sftp://user@host/absolute/foldover-perf-example/target`
root with a saved binary and `--revision`:

```sh
FOLDOVER_PERF_ROOT="$PWD/build/foldover-perf-example"
mkdir -p "$FOLDOVER_PERF_ROOT"
python3 perf/make-corpus.py "$FOLDOVER_PERF_ROOT/source" --files 10000 --bytes 8192 --width 1000
mkdir "$FOLDOVER_PERF_ROOT/target"
python3 perf/run-qualification.py --runner target/release/examples/performance \
  --state "$FOLDOVER_PERF_ROOT/state" \
  --left "$FOLDOVER_PERF_ROOT/source" \
  --right "sftp://user@host/absolute/foldover-perf-example/target" \
  --iterations 3 --label development --output "$FOLDOVER_PERF_ROOT/results"
```

Keep the initial population and warm no-change runs separate.

## Cached spelling lookups

A folder's cached listing used to be scanned linearly for every
respellable name, composing each cached name until a match. The cache now
indexes exact bytes and composed forms, so one lookup is a hash probe per
map and a mutation touches the name it changes. The index keeps the
server's listing order for aliases that compose alike, leaves invalid
UTF-8 alone, and is bounded by folder count (512) and a conservative byte
estimate (~32 MiB) that charges key and table overhead; past either bound
the cache starts over rather than grow. Folder keys and Dirty views count too,
and writes evict views when they cross the limit. A fetched folder index that
exceeds the limit can serve its current lookup but is not cached; later
lookups refetch it. The limit bounds retained hints, not a transient directory
listing returned by the server.

`cached_spelling_lookup_bench` (ignored) measures folders of 1k, 10k and
100k mostly-ASCII names with one composed Unicode name, three samples of
200 lookups each, asking for an absent decomposed name so the whole cache
is walked. On this host, before the index (revision `d89d474`) and after:

| cached names | before, per lookup | after, per lookup |
|---|---|---|
| 1k | 184, 192, 194 µs | 359, 372, 369 ns |
| 10k | 1.90, 1.87, 1.89 ms | 308, 343, 346 ns |
| 100k | 18.2, 18.1, 17.7 ms | 343, 351, 352 ns |

The after column is flat in folder size. These are local subprocess
timings against `sftp-server`, not a LAN or NAS result: they measure the
cache's CPU, not wire behaviour, and say nothing about SSH overhead or
throughput. Reproduce with

```sh
cargo test --locked --release -p foldover-endpoint --lib cached_spelling_lookup_bench -- --ignored --nocapture
```
