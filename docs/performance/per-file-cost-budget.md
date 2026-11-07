# Performance proposals: the per-file cost budget of a million-document SFTP sync

Foldover is a folder-sync tool: a Rust engine (`foldoverd`) compares two
folders (local disks, mounted shares, SFTP servers or paired Macs) and
copies what differs, previewing every change first. A sync run's hot path
is `PairRun::prepare` → `PairRun::execute` in
`crates/foldover-engine/src/run.rs`: it observes both sides
(`Endpoint::observe_from`, `crates/foldover-endpoint/src/local.rs` and
`crates/foldover-endpoint/src/sftp.rs`), plans the difference
(`plan_resolved`, `crates/foldover-engine/src/plan.rs`) and then applies
it, copies running on worker threads (`crates/foldover-engine/src/run/copies.rs`,
`crates/foldover-endpoint/src/sftp/readers.rs`) while the run's own thread
journals and commits base updates in batches of 256
(`COMMIT_BATCH`, `run.rs:52`). For the target workload of millions of
documents compared against, and copied to, an SFTP server on the LAN,
end-to-end time is almost entirely a *per-file* cost: filesystem
syscalls on the local side, SFTP protocol round trips on the server
side. This document measures that per-file budget on this host, using
`fs_usage` for syscall counts, a packet-counting proxy for SFTP request
counts, and the repository's criterion benches for the database and
planning paths, then ranks changes that shrink it. Ranking weighs
expected effect on end-to-end run time against effort and risk;
proposals that would weaken a correctness contract are listed separately
under "Requires approval", not in the ranking.

## Ranked proposals

1. Size each SFTP read request to the file being read
2. Cut the per-file SFTP request count on the copy path from eight to five or six
3. Raise copy concurrency into the 4-8 connection band PLAN.md §4.9 allows
4. Observe both sides concurrently instead of left then right
5. Merge the extended-metadata read into the content read on local roots
6. Parallelize local entry versioning across the worker pool
7. Skip the snapshot rewrite when the new observation equals the stored one
8. Make planning's ancestor walks linear and stop allocating per parent step

## What was measured, and where

Everything below comes from this host (Mac Studio, macOS 27, Apple
silicon, release profile, Rust 1.99.0 pinned by `rust-toolchain.toml`),
using scratch tests that were run and then removed; no repository file
was modified. Several other agents were building and testing on the same
machine while these numbers were taken, so absolute figures carry maybe
2x noise; each number is the minimum of three rounds where it mattered,
and the relative split inside one run is the reliable part.

| Measurement | Value |
|---|---|
| `scan_10k_files` criterion bench (`crates/foldover-endpoint/benches/scan.rs`) | 3.06 s for 10,000 files, 306 us/file (budget 1000 ms) |
| Local cold observe, 10,000 files of 4 KiB (`LocalEndpoint::observe_from`, no previous) | 1.42 s, 142 us/file |
| Local warm observe, same tree, previous snapshot supplied | 61 ms, 6.1 us/file (23x cheaper) |
| `scan_metadata` alone, 10,100 entries | 36.7 ms, 3.6 us/entry |
| `hash_file` alone, 10,000 files of 4 KiB | 1.09 s, 109 us/file |
| `read_extended_metadata` alone, 10,000 files | 2.01 s, 201 us/file |
| SFTP observe, 2,000 files of 8 KiB, four sessions | 192 ms, 96 us/file |
| SFTP copy, 200 files of 8 KiB, four worker threads | 2.48 s, 12.4 ms/file wall |
| SFTP requests the server read during that copy | 1,669 for 200 files plus 20 folders, about 8 per file |
| SFTP requests to read one file through `Session::read_all` (packet-counting proxy) | 0 B: 19; 1 KiB: 35; 8 KiB: 35; 32 KiB: 19; 32 KiB+1: 35; 512 KiB: 35, of which 16 or 32 are `READ` |
| `plan_100k_entries` bench | 58.6 ms (budget 350 ms) |
| `db_commit_100k` / `db_read_100k` / `db_snapshot_unchanged_100k` / `db_snapshot_rewrite_100k` | 116 ms / 74 ms / 23.6 ms / 192 ms |

The SFTP numbers come from `/usr/libexec/sftp-server` either spawned
directly by `SftpEndpoint::spawn` (the path the crate's own tests use) or
behind a small Python proxy that counts length-prefixed packets, so they
carry **no network latency and no SSH encryption**: they are a lower
bound on what a real LAN server costs, and they understate the
round-trip proposals below. The request counts, however, are exact and
latency-independent.

Two measured facts drive the ranking. First, reading one file over SFTP
costs 35 requests for anything from 1 KiB to 512 KiB, because
`Session::read_from` (`crates/foldover-net/src/sftp.rs:339`) always asks
for a full window of 16 chunks and then re-asks after a short read. The
observe path reads every file on every run, so this is the floor of
every sync: extrapolating the measured 96 us/file to a million documents
gives about 96 s against a zero-latency server, and on a real LAN it is
34 round trips per file. Second, a copy costs about eight requests per
file, so a first sync of a million new documents spends roughly 440 s
copying against 96 s observing, i.e. the copy is about 65 percent of the
run while the observation is about 94 percent of a steady-state run.

`fs_usage` on the server side during the copy shows, per placed file:
one `open`, one to two `write` (one per 32 KiB chunk), one `close`, one
`utimes` (the mtime `SETSTAT`), one `link` plus one `unlink` (the
server's own implementation of `SSH_FXP_RENAME`), and about four `lstat`.
`fs_usage` on the client side during `read_extended_metadata` shows about
fourteen filesystem syscalls per file: two `openat`, 2.6 `flistxattr`,
two `fgetxattr`, 3.25 `close`, 1.3 `fstat64`, 1.3 `fstatat64`, 1.3
`fcntl` and 0.65 `fstat64_extended` (the ACL lookup). The two most
expensive were `openat` at 11 us average and `fgetxattr` at 8.7 us
average; tracing perturbs timing, so treat the per-syscall durations as
indicative and the counts as solid.

---

### 1. Size each SFTP read request to the file being read

- Category: I/O and batching
- Evidence: `crates/foldover-net/src/sftp.rs:32` fixes `CHUNK` at 32 KiB
  and `:35` fixes `WINDOW` at 16. `Session::read_from` (`sftp.rs:339`)
  requests a full window of 16 chunks whatever the file's size, then
  loops again whenever a read came back short, so a 1 KiB document costs
  32 `READ` requests and a 32 KiB one costs 16. Measured with a
  packet-counting proxy in front of `sftp-server`:
  `read_all` on 0 B / 1 KiB / 8 KiB / 32 KiB / 32 KiB+1 / 512 KiB files
  issued 16 / 32 / 32 / 16 / 32 / 32 `READ` requests, plus `OPEN` and
  `CLOSE`. The observe path calls this for every file on every run
  (`crates/foldover-endpoint/src/sftp.rs:750`, `read_range_on`), and the
  measured SFTP observe of 2,000 files of 8 KiB took 96 us/file against
  a server with no network latency at all.
- Hypothesis: the window is sized for bulk transfer, not for the
  documents this workload is about, so almost every request is wasted:
  it reads past the end of the file and is answered with `EOF`, or is
  re-issued after a short read. Each request costs at least one round
  trip, so the per-file cost of observing the server is close to
  `WINDOW` round trips instead of `ceil(size / CHUNK)`. Asking only for
  what the listed size allows, where the attributes are already in hand
  from the listing, reduces a small file to one `READ`.
- Proposed change: in the read paths that know the expected size
  (`read_range_on`, `crates/foldover-endpoint/src/sftp.rs:750`, and the
  copy path's `Source::push` in `crates/foldover-endpoint/src/sftp/transfer.rs:70`),
  compute the window as `min(WINDOW, ceil(remaining / CHUNK))` and stop
  after a short read instead of re-asking, while keeping the growth
  probe that reports a file which grew past its stat as `Changed`
  rather than silently accepting a truncated read. Leave
  `Session::read_from`'s unbounded form for callers that have no size.
- Expected impact: 3-16x fewer requests per file in the observe path,
  which is the dominant term of every run against a latency-bound LAN:
  a steady-state million-document sync should drop from about 34 round
  trips per file to 3 (medium-high confidence: the request counts are
  measured exactly and the saving follows arithmetically, but no real
  LAN was measured).
- Effort and risk: small. The change is local to the window computation
  and the short-read handling; the risks are understating a file that
  grew, which the existing size check and `Changed` handling cover, and
  regressing bulk throughput for large files, which the `ceil` form
  avoids.
- Measurement plan: rerun the packet-counting proxy used here over a
  tree of small files and count `READ` requests per file; the target is
  one per file under 32 KiB. Then add a latency-injecting proxy and
  compare the observe wall time per file, which is the figure that
  predicts a LAN.

### 2. Cut the per-file SFTP request count on the copy path from eight to five or six

- Category: I/O and batching
- Evidence: `crates/foldover-endpoint/src/sftp/transfer.rs:120` (`copy`),
  `crates/foldover-endpoint/src/sftp.rs:940` (`set_metadata_on`),
  `sftp.rs:921` (`staged_version_on`), `sftp.rs:1034` (`place_on`),
  `sftp.rs:888` (`upload_on`), `sftp.rs:1672` (`write_new`). Tracing the
  server during a 200-file copy measured 1,669 requests, about eight per
  file. Walking the create path (`Expected::Absent`, the common case for
  a first backup) accounts for exactly eight: `receiving_on`'s `LSTAT`
  (`transfer.rs:419`), `create_new`'s `OPEN`, one `WRITE` per 32 KiB
  chunk, `CLOSE`, `set_metadata_on`'s `LSTAT` plus `SETSTAT`,
  `staged_version_on`'s `LSTAT`, and `place_on`'s `RENAME`.
- Hypothesis: on a LAN each request costs at least one round trip, so
  eight sequential requests per file make the copy phase
  round-trip-bound rather than bandwidth-bound for small documents. The
  local-pipe measurement (12.4 ms/file wall with four threads, about
  0.44 ms/file in a quieter run) is the floor; at a 2 ms RTT the same
  eight requests cost about 16 ms of thread time per file, roughly 67
  minutes per million files on four threads. Removing three requests
  removes about 37 percent of that.
- Proposed change: keep the destination handle open across the metadata
  step so `set_metadata_on` becomes `FSETSTAT` on the open handle
  instead of `LSTAT` + `SETSTAT` on a path (`write_new`,
  `sftp.rs:1672`, closes the handle before `set_metadata_on` runs;
  reorder so the handle outlives it). Fold `staged_version_on`
  (`sftp.rs:921`) into the same step by building the version from the
  attributes the server already reported for the handle, keeping the
  existing `ContentNotStored` check on the digest. For creates, drop
  `receiving_on`'s pre-`LSTAT` and let `create_new`'s
  `OPEN_WRITE | OPEN_CREATE | OPEN_EXCLUSIVE` (`crates/foldover-net/src/sftp.rs:313`)
  be the absence proof, mapping a refusal to the existing
  `DestinationChanged` error. Net: eight requests to five or six.
- Expected impact: 25-37 percent off the copy phase for small files on a
  real LAN (medium confidence: the request count is measured, the RTT
  model is not, and the local-pipe run showed only about 12 percent
  because it is not round-trip-bound).
- Effort and risk: medium. Touches the write path that every SFTP pair
  uses; the risks are losing the `DestinationChanged` diagnosis for a
  file that appeared concurrently, and `FSETSTAT` semantics differing
  from `SETSTAT` on servers without the fsync or posix extensions. The
  crate's 31 SFTP tests in `crates/foldover-endpoint/src/sftp/tests.rs`
  and 16 wire tests in `crates/foldover-net/src/sftp/tests.rs` cover the
  replaced-entry and cancelled-write cases and should be the gate.
- Measurement plan: rerun the copy trace used here (a scratch test that
  copies N files over a spawned `sftp-server` while `sudo fs_usage -w -f
  network,filesys -t <seconds> sftp-server` runs) and divide the server's
  `read` count by the file count; the target is five or six. Then add a
  latency-injecting proxy and compare wall time per file before and
  after, which is the number that actually predicts a LAN.

### 3. Raise copy concurrency into the 4-8 connection band PLAN.md §4.9 allows

- Category: concurrency and parallelism
- Evidence: `crates/foldover-endpoint/src/sftp.rs:68` sets
  `PARALLEL_COPIES` to 4 and `SESSIONS` to 5;
  `crates/foldover-endpoint/src/local.rs:743` returns 4 for a solid-state
  or network volume; `crates/foldover-engine/src/run.rs:1430` takes the
  minimum of the two sides, so a local-to-SFTP pair copies four files at
  a time. PLAN.md §4.9 is quoted in the code as allowing 4-8 network
  connections. Measured: 200 files of 8 KiB copied in 2.48 s on four
  threads against a zero-latency server.
- Hypothesis: with eight sequential requests per file, throughput on a
  real LAN is concurrency divided by (requests x RTT). Four in-flight
  files leaves the link idle whenever RTT exceeds the service time of a
  request, which for small documents is always. Doubling in-flight files
  roughly doubles copy throughput while the server still has headroom,
  since the session pool already adapts downward when a server refuses
  connections (`crates/foldover-endpoint/src/sftp/sessions.rs`).
- Proposed change: make `PARALLEL_COPIES` a per-pair value chosen from
  the smaller of the two endpoints' declared worker counts and a new
  option (default 8 for network endpoints, still 4 for rotational
  volumes), keeping `SESSIONS = PARALLEL_COPIES + 1` and the existing
  `workers()` minimum in `run.rs:1430`. Keep the two-SFTP-endpoint case
  at one worker, as `run.rs:1430` already does, so opposite-direction
  copies cannot deadlock on session pools.
- Expected impact: up to 2x on the copy phase for small files on a
  latency-bound LAN (low-medium confidence: unverified against a real
  server, and the gain vanishes if the server is the bottleneck or
  refuses extra sessions).
- Effort and risk: small to medium. The pool, the lease session and the
  copy workers already handle a configurable count; the risk is a server
  that caps sessions and answers more slowly under load, which the
  refusal-clamping logic in `sessions.rs` should absorb, plus more
  concurrent writers on one folder.
- Measurement plan: run the same copy trace with `PARALLEL_COPIES` at 4
  and 8 against a latency-injecting proxy and compare wall time per
  file; also record `Sessions`' `limit` and `refusals` counters to
  confirm the server accepted the extra sessions.

### 4. Observe both sides concurrently instead of left then right

- Category: concurrency and parallelism
- Evidence: `crates/foldover-engine/src/run.rs:773-774` observes the left
  side to completion, then the right. Measured on this host: the local
  cold observe costs 142 us/file and the SFTP observe 96 us/file, so a
  first sync pays both in series, about 238 us/file of compare time
  before any copy starts. The SFTP side already parallelizes its file
  reads across sessions (`crates/foldover-endpoint/src/sftp/readers.rs:46`),
  and the local side does not (see proposal 6).
- Hypothesis: the two observations are independent, since each endpoint
  is read through its own held root and its own session pool, so running
  them on two threads makes the compare phase cost `max(local, sftp)`
  instead of the sum, roughly a 40 percent saving on a first sync and a
  smaller one once the local side is cache-warm.
- Proposed change: in `prepare_on`, spawn the right side's observation on
  a scoped thread (the `observe` closure at `run.rs:764-772` is already
  `FnOnce`-shaped over `&mut dyn FnMut() -> ScanControl`; wrap the
  control so either side's cancel stops both), then join before
  `align_spellings`. Keep the progress calls per side as they are, and
  keep the local observation's ordering guarantees by having each side
  build its own `Observation`.
- Expected impact: 30-45 percent off the compare phase of a first sync,
  near zero once the local side is warm (medium confidence; the two
  per-file costs are measured, the overlap is not).
- Effort and risk: medium. Blast radius is `run.rs` plus the engine's
  cancellation contract: `ScanControl` must still reach both sides, a
  panic in one thread must not lose the other's observation, and the
  power assertion and lease timing stay on the run's thread. The
  existing simulation suite in `crates/foldover-engine/src/run/tests`
  is the regression gate.
- Measurement plan: a scratch test that times `PairRun::prepare` on a
  local-SFTP pair with a cold local tree before and after the change,
  with both sides' observe timings logged separately so the overlap is
  visible; assert the total is close to the slower side.

### 5. Merge the extended-metadata read into the content read on local roots

- Category: redundant work and caching
- Evidence: `crates/foldover-endpoint/src/local.rs:1233` (`entry_version`)
  calls `optional_extended_metadata` and then `hash_file`, and each of
  those opens the file again and re-verifies its stat:
  `crates/foldover-fs/src/root/extended.rs:170-208` does `parent_of` (an
  `openat` per path component), `fstatat`, `openat`, `fstat`, `capture`
  (two full `attribute_names` passes, about four `flistxattr`, plus an
  ACL lookup), `fstat`, and `self.metadata(path)` (another `parent_of`
  plus `fstatat`); `crates/foldover-fs/src/root/content.rs:44-77` and
  `:94-130` then repeat `parent_of`, `fstatat`, `openat`, `fstat`, the
  data reads, and a trailing `fstat` plus `fstatat`. `fs_usage` counted
  about fourteen filesystem syscalls per file for the extended read
  alone, with `openat` at 11 us and `fgetxattr` at 8.7 us average.
  Measured cost: 201 us/file for `read_extended_metadata` and 109
  us/file for `hash_file` against a cold observe of 142 us/file (the
  isolated numbers are noisier than the end-to-end one but agree on the
  ordering).
- Hypothesis: the extended-metadata pass and the content pass need the
  same open descriptor and the same before/after stat verification, so
  running them separately doubles the opens and the verification stats
  and re-walks the path twice. One fused read that opens once, lists and
  digests the attributes, then streams the content through the same
  hasher removes roughly half of the fourteen syscalls and both
  redundant `attribute_names` passes.
- Proposed change: add a single driver entry point in `foldover-fs`
  (for example `Root::observe_file(path, expected, policy, control)`)
  that performs one `openat`, one verification `fstat`, the existing
  `capture` once (keeping the second `attribute_names` comparison only
  where it is needed for the change check), the content stream, and one
  trailing verification pair; have `LocalEndpoint::entry_version`
  (`local.rs:1233`) call it for regular files and keep the current
  separate paths for directories and links. Preserve the materialization
  policy checks and the `Changed` contract exactly.
- Expected impact: 20-35 percent off the local cold observe, which is
  about 20 percent of a first sync's compare phase and nothing once the
  tree is cache-warm (medium confidence).
- Effort and risk: medium. It touches the driver that owns the
  change-detection contract (ADR 0012, ADR 0016), so the verification
  must stay byte-identical in effect; the risk is silently weakening the
  "the file was the observed one" guarantee for a small speedup. The
  crate's native tests around `read_extended_metadata` and
  `hash_file` must keep passing unchanged.
- Measurement plan: rerun the `fs_usage` trace used here (a scratch test
  that calls `read_extended_metadata` over 500 files while `sudo fs_usage
  -w -f filesys <pid>` runs) and compare the per-file syscall counts;
  then rerun the local cold/warm observe measurement, where the target
  is the cold figure falling by a fifth to a third with the warm figure
  unchanged.

### 6. Parallelize local entry versioning across the worker pool

- Category: concurrency and parallelism
- Evidence: `crates/foldover-endpoint/src/local.rs:1134`
  (`observe_reporting`) walks the tree, buffers every entry into a
  `Vec` (`local.rs:1146-1147`) and then computes each entry's version on
  the calling thread. The SFTP side already does the equivalent work on
  several sessions (`crates/foldover-endpoint/src/sftp/readers.rs:46`),
  and the run already owns a worker-pool pattern for copies
  (`crates/foldover-engine/src/run/copies.rs:57`). Measured: 109 us/file
  for `hash_file` and 201 us/file for `read_extended_metadata`, both
  single-threaded, on a volume whose medium is reported as solid state
  (`local.rs:743` returns four workers for it).
- Hypothesis: a cold local observe of a million files is about 142 s of
  serial per-file work that is mostly syscalls and BLAKE3, both of which
  parallelize; four workers on a solid-state volume should cut it to
  roughly a third, which matters for first syncs, for restored trees and
  for any run where many files changed.
- Proposed change: split the version loop into a work-stealing shape
  like `read_each`: clone the held root per worker (`Root::try_clone`,
  `crates/foldover-fs/src/root.rs:549`), hand each worker a slice of the
  buffered entries, and keep recording, progress and cancellation on the
  run's thread exactly as `readers.rs` does. Directories can stay
  serial; only files and links need fan-out.
- Expected impact: 2-3x on the local cold observe, i.e. roughly 100 s
  saved per million files on a first sync and nothing on a warm one
  (medium confidence; the per-file cost is measured, the speedup is
  not, and a rotational or network volume may not scale).
- Effort and risk: medium-high. `LocalEndpoint` must be `Sync` for this
  (it holds a cap-std `Dir`, so it should be, but that needs an explicit
  assertion), and the observation's ordering and `unstable` bookkeeping
  must not depend on completion order. Progress reporting, cancellation
  and the failure paths (`ScanStats::skip_error`) are the likely
  regression sites.
- Measurement plan: extend the local breakdown scratch test to observe
  with one and with four workers on the same tree and compare wall time;
  also run it on a rotational volume (or a disk image) to confirm the
  worker count is still the right choice there, since `local.rs:743`
  picks one worker for rotational media.

### 7. Skip the snapshot rewrite when the new observation equals the stored one

- Category: redundant work and caching
- Evidence: `crates/foldover-service/src/sync.rs:907-915` stores a fresh
  snapshot of both sides after every run, and
  `crates/foldover-state/src/pairs.rs:276` (`replace_snapshot`) builds a
  `Vec` of every entry with a cloned path, a cloned fingerprint and an
  encoded version before diffing it against the stored rows in
  `crates/foldover-db/src/database.rs:806`. Measured on this host:
  `db_snapshot_unchanged_100k` 23.6 ms and `db_snapshot_rewrite_100k`
  192 ms, so at a million entries per side the unchanged case is about
  0.24 s of SQLite work per side, plus the same order of allocation for
  the `Vec<SnapshotEntry>` that `PairRun::snapshot` (`run.rs:925`)
  builds. `run_inputs` (`crates/foldover-engine/src/configuration.rs:242`)
  already loads the previous snapshot for the cache, so the comparison
  input is in memory.
- Hypothesis: for the steady-state runs this workload spends most of its
  time in, the observation of each side is byte-identical to the stored
  one, and the whole diff is guaranteed to find nothing. Comparing in
  memory first and skipping the transaction removes the SQLite work, the
  row diff and the per-entry allocation for both sides.
- Proposed change: have `PairRun::snapshot` return the entries together
  with a cheap equality verdict against the snapshot the run was given
  (the same `ObservedEntry` values are already compared for the cache
  hit), and skip `store_snapshot` for a side that matches; keep the
  store unconditional when anything differs, when the pair revision
  moved, or when a previous store failed.
- Expected impact: about 0.5 s and a few hundred megabytes of transient
  allocation per million-entry pair per run, i.e. under 1 percent of a
  steady-state run (low-medium confidence; the SQLite figures are
  measured, the allocation cost is estimated).
- Effort and risk: small. The risk is a stale cache if the skip is too
  eager: a failed or partial earlier store must still be retried, so
  the verdict must be conservative and the existing "a cache only costs
  a reread" comment in `sync.rs:908` must stay true.
- Measurement plan: rerun the `db_snapshot_*` benches plus a scratch
  test that times a full `store_snapshot` of one million synthetic
  entries, before and after; the target is the unchanged case costing
  only the in-memory comparison.

### 8. Make planning's ancestor walks linear and stop allocating per parent step

- Category: algorithmic complexity
- Evidence: `crates/foldover-engine/src/plan.rs:577-604` walks every
  ancestor of every row whose entry changed, and `plan.rs:605-625` walks
  every ancestor of every row again to propagate conflicts; each step
  calls `SyncPath::parent()` (`crates/foldover-types/src/sync_path.rs:36`),
  which allocates a new `Vec<u8>`. Two more per-row `name_key`
  allocations (`plan.rs:421`, which builds a `String` and then a
  `Vec<u8>`) sit in `mark_name_clashes` (`plan.rs:351`) and in
  `protect_unavailable` / `protect_uncertain_absences`
  (`plan.rs:115-191`). Measured: `plan_100k_entries` takes 58.6 ms, so a
  million-entry pair is about 0.6 s of planning per run.
- Hypothesis: the ancestor walks are O(rows x depth) with an allocation
  per step, and rows arrive in path order, so a single downward pass
  that carries each branch's state on a stack removes both the repeated
  walks and the per-step allocation; caching or borrowing the name keys
  removes a third allocation per row. For deep trees the walk count
  matters more than for the shallow ones measured here.
- Proposed change: replace both loops with one pass over the
  path-ordered rows that maintains a stack of open ancestors (push on
  descent, pop on ascent), propagating conflict and archive state
  downward; compute each row's `name_key` once into a reusable buffer
  and reuse it for the clash map.
- Expected impact: maybe 20-40 percent off planning, about 0.15-0.25 s
  per million-entry run, plus lower peak memory (low-medium confidence:
  the 58.6 ms baseline is measured on a shallow synthetic tree, and the
  gain grows with depth).
- Effort and risk: medium. Planning is the correctness core, where
  ordering, conflict propagation and deletion review all depend on each
  other, so this needs the property tests in
  `crates/foldover-engine/src/plan.rs` and
  `crates/foldover-engine/src/run/tests` to stay green, plus a deep-tree
  case that the current benches do not have.
- Measurement plan: extend `crates/foldover-engine/benches/plan.rs` with
  a deep-tree variant (for example 10,000 paths at depth 20) and compare
  before and after; the target is the deep case scaling with rows rather
  than rows x depth.

## Requires approval (changes a correctness contract)

- **Caching SFTP content by stat.** `crates/foldover-endpoint/src/sftp.rs:468`
  states that SFTP stat cannot prove content stayed unchanged, so every
  run re-reads every file. Trusting size and mtime would remove the
  server-side read entirely and is by far the largest possible win for
  steady-state syncs, but it changes what a "no change" answer means and
  is rejected here for that reason; it needs an explicit owner decision.
- **Dropping the trailing pathname stat after a read or hash.**
  `crates/foldover-fs/src/root/content.rs:122-128` and
  `crates/foldover-fs/src/root/extended.rs:204` re-stat by path after the
  read to prove the entry was not swapped. Removing it saves one
  `fstatat` plus one `parent_of` walk per file, but weakens the
  change-detection contract (ADR 0012, ADR 0016).
- **Sharing one session between the two sides of an SFTP-to-SFTP copy.**
  `run.rs:1430` pins two-SFTP-endpoint pairs to one worker to avoid a
  session deadlock. Relaxing that could double throughput for
  server-to-server copies but needs a deadlock-freedom argument, not
  just a test.

## Open questions and unverified items

- No real LAN was measured. Every SFTP figure here comes from a
  `sftp-server` subprocess on this host, so the round-trip proposals are
  justified by request counts (measured exactly) and a round-trip model
  (hypothesis). A latency-injecting proxy is the missing measurement,
  and the expected impact of proposals 1, 2 and 3 should be treated as
  provisional until it exists.
- Absolute timings on this host are noisy: other agents were running
  builds and tests concurrently, and repeated runs of the same
  measurement varied by up to 3x. The minimum-of-three figures and the
  in-run splits are the reliable parts; single-run absolutes are not.
- The per-syscall durations from `fs_usage` (11 us for `openat`, 8.7 us
  for `fgetxattr`) are suspiciously slow for a local APFS volume and are
  probably inflated by tracing itself. The syscall counts are solid; the
  durations only indicate which calls dominate.
- The `link` plus `unlink` pair the server performs per placed file was
  observed but not explained: it is either OpenSSH's implementation of
  `SSH_FXP_RENAME` or a sign that the negotiated
  `posix-rename@openssh.com` extension (`crates/foldover-net/src/sftp.rs:67`)
  is not being used for placement. If the latter, using
  `rename_replacing` for placement would remove one server-side syscall
  per file, though not a round trip.
- The packet-counting proxy was verified against a direct
  `sftp-server` exchange, but its type table was initially wrong and had
  to be corrected; the counts reported here come from the corrected
  mapping, and re-running the proxy is cheap if any doubt remains.
- Whether the local volume's medium detection (`local.rs:743`) reports
  solid state on the target machines was not checked; proposals 3 and 6
  assume it does.
- The DB extrapolation from 100k to one million entries is linear
  scaling of measured benches; SQLite's B-tree behaviour at that size
  was not measured directly.
- No profile was taken with a sampling profiler; the breakdown comes
  from isolated measurements of `scan_metadata`, `hash_file` and
  `read_extended_metadata` plus an `fs_usage` syscall trace, which
  attributes cost to syscalls but not to userspace allocation.
