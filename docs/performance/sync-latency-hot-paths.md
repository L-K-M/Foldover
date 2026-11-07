# Performance proposals: end-to-end latency of syncs to an SFTP server

Foldover is a macOS app over a portable Rust engine (`crates/`) that syncs folder
pairs between local disks, NAS shares, SFTP servers and other Macs, previewing
every change before it runs. The main execution path of one run is
**observe both sides** (`crates/foldover-endpoint/src/local.rs`
`observe_reporting`, `crates/foldover-endpoint/src/sftp.rs`
`observe_reporting`/`observe_reads`), **plan** (`crates/foldover-engine/src/plan.rs`
`plan_resolved`), and **apply** (`crates/foldover-engine/src/run.rs` `execute`
over `run/copies.rs` workers), with durable progress through
`crates/foldover-state` into SQLite (`crates/foldover-db`). The workload studied
here is comparing and copying millions of documents to an SFTP server on the
local network; the primary metric is end-to-end wall-clock time of a sync or
backup run, so the ranking weights the two phases that dominate that workload —
the comparison pass that re-reads the remote corpus, and the per-file round-trip
cost of uploading millions of small files — above CPU-side costs that only
matter at the margin. Proposals are ranked by expected impact on that metric,
weighted by implementation effort and risk. Every code citation was read at the
current checkout; nothing was executed or profiled in this session, so all
magnitudes below are hypotheses. Numbers quoted from
[ADR 0046](../adr/0046-snapshot-archive-sftp-performance.md) and
[perf/budgets.toml](../../perf/budgets.toml) are measurements recorded in the
repository by earlier work, not measurements made here. Sibling proposals from
earlier passes live in [lan-sftp-end-to-end-sync.md](lan-sftp-end-to-end-sync.md),
[sftp-million-file-hot-paths.md](sftp-million-file-hot-paths.md) and the other
files in this directory; this document re-derives the ranking independently
against the current tree.

## Ranked proposals

1. Reuse stat-unchanged SFTP versions instead of re-reading every remote file
2. Cut the per-file round trips of the SFTP upload path
3. Hash planned creates at copy time, not at observe time
4. Parallelize SFTP directory listing during observe
5. Parallelize local content hashing during observe
6. Stop cloning whole version maps when preparing a plan
7. Replace per-row linear scans of failures and unavailable roots with indexes
8. Batch base commits to cut per-batch fsyncs
9. Build the release engine with LTO and one codegen unit

## Full entries

### 1. Reuse stat-unchanged SFTP versions instead of re-reading every remote file

- Category: redundant work and caching
- Evidence: `crates/foldover-endpoint/src/sftp.rs:469-471` documents the
  behavior: "Reobserve contents and link targets: SFTP stat cannot prove they
  stayed unchanged, even when size and modification time match." Every SFTP
  observation therefore computes each file's version by reading its full content
  (`sftp.rs:642` `let version = self.version(...)` runs before any cache check;
  `sftp.rs:681-695` `version_on` reads the whole file through
  `read_range_on(..., 0, ...)`) and only afterwards compares against the stored
  snapshot (`sftp.rs:647-651`) to avoid a DB rewrite. The local side does the
  opposite: `crates/foldover-endpoint/src/local.rs:1197-1204` reuses the stored
  version when `Fingerprint::proves_unchanged` holds (`local.rs:312-336`), with
  a bounded re-read window `REREAD` of seven days (`local.rs:288`,
  `local.rs:341-348`). [PLAN.md §4.8](../../PLAN.md#48-change-detection)
  (lines 553-560) already prescribes stat-based comparison with a weekly
  content-hash pass, so the SFTP implementation is stricter than the design
  requires.
- Hypothesis: for the workload "comparing millions of documents to an SFTP
  server", the comparison phase downloads the entire remote corpus on every
  run to recompute digests that almost always match. At even 100 MB/s of
  effective read throughput that is 17 minutes per 100 GB per run, plus one
  OPEN/READ/CLOSE/LSTAT sequence per file (four round trips) across four
  sessions. Reusing a stored version when the listing's stat key and size match
  the stored fingerprint turns the comparison into the directory walk alone,
  which is one to two orders of magnitude less work on a mostly-unchanged
  corpus. This is the single largest lever for repeated syncs.
- Proposed change: in `observe_entry` (`sftp.rs:631`), check
  `previous.get(path)` and `known.fingerprint.same_stat(&key, size)` (already
  available on `Fingerprint`, `local.rs:326`) *before* computing `version`;
  when they match and the fingerprint is inside a re-read window, return
  `known.clone()` without reading content, mirroring
  `Fingerprint::proves_unchanged`/`reread_after` (`local.rs:330-348`), which
  spreads re-reads between half and all of a `REREAD` interval by the stat
  hash. Deep verification stays as the weekly content pass of PLAN.md §4.8
  (extend `poll`/verification scheduling to force it), and a pair option
  "always verify content" restores today's behavior for servers that rewrite
  content without touching stat.
- Expected impact: comparison phase of an incremental run over an unchanged
  corpus drops from O(corpus bytes) to O(entries): estimated 10-50x faster
  observe on the SFTP side, and end-to-end run time cut by more than half
  whenever the corpus is mostly unchanged (medium confidence in the direction,
  low-medium in the magnitude; unverified).
- Effort and risk: moderate, confined to `sftp.rs` observe plus an option
  field. The risk is real: same-size, same-mtime remote edits inside the
  window would be missed until the next deep pass. PLAN.md §4.8 already
  accepts this staleness ("content-hash pass weekly (catches same-size,
  same-mtime edits)"), and the local side already accepts it for up to seven
  days, but this weakens a guarantee the code comments currently claim for
  SFTP, so it needs design sign-off (see below).
- Measurement plan: extend the ADR 0046 harness (SFTP through a relay that
  adds per-message latency) with a criterion bench `sftp_observe_unchanged`:
  observe a 1,000-file / 1 GiB tree twice with a stored snapshot, time the
  second pass and count bytes read; then repeat at 100,000 files. Compare
  before/after wall time and bytes over the wire. Budget id to add to
  `perf/budgets.toml` once calibrated.

### 2. Cut the per-file round trips of the SFTP upload path

- Category: I/O and batching
- Evidence: one small-file copy to SFTP runs, serially on its session:
  `crates/foldover-endpoint/src/sftp/transfer.rs:428` (`receiving_on` does an
  LSTAT to confirm absence), `crates/foldover-endpoint/src/sftp.rs:1672-1686`
  (`write_new`: OPEN via `create_new`, WRITE, CLOSE), `sftp.rs:940-979`
  (`set_metadata_on`: LSTAT, then SETSTAT when mode or times change),
  `sftp.rs:921-933` (`staged_version_on`: another LSTAT), and
  `sftp.rs:1034-1081` (`place_on`: RENAME). That is seven to eight sequential
  round trips per file before any bytes matter. `crates/foldover-net/src/sftp/wire.rs:10-31`
  implements LSTAT (7), SETSTAT (9) and FSETSTAT (10) but not FSTAT (8), so
  handle-based stat is impossible today; `foldover-net/src/sftp.rs:552-563`
  `set_handle_attributes` (FSETSTAT) exists but the upload path closes the
  handle before touching metadata.
- Hypothesis: for millions of small documents the copy phase is latency-bound,
  not bandwidth-bound: at a 1 ms LAN round trip, eight round trips per file on
  four workers (`sftp.rs:68` `PARALLEL_COPIES = 4`) caps throughput near 500
  files/s, about 35 minutes per million files in round trips alone. Removing
  two or three round trips per file, and/or pipelining the sequence across
  files on one session (the wire already tracks in-flight ids and stashes
  out-of-order replies, `foldover-net/src/sftp.rs:104`, `:46` `MAX_STASHED`),
  cuts the copy phase proportionally.
- Proposed change: (a) drop the pre-create LSTAT in `receiving_on` for
  `Expected::Absent` and map an `OPEN_CREATE|OPEN_EXCLUSIVE` failure to
  `ApplyError::DestinationChanged` (the exclusive create is already the
  authoritative absence check, `foldover-net/src/sftp.rs:313-328`); (b) set
  mode and times with FSETSTAT on the open handle before CLOSE and add
  `FSTAT = 8` to `wire.rs` so `staged_version_on` stats the handle instead of
  the path, keeping one post-write verification; (c) as a follow-up, issue the
  next file's OPEN while the previous file's RENAME is still pending on the
  same session (PLAN.md lines 428 and 571 already promise "pipelined
  requests"). Optionally raise `PARALLEL_COPIES` for SFTP toward the eight
  PLAN.md line 571 allows for network endpoints, letting the session pool's
  refusal handling (`sftp/sessions.rs:113-150`) degrade gracefully.
- Expected impact: per-file round trips fall from 7-8 to about 4-5 (steps a
  and b), roughly 30-40 percent off the small-file copy phase; pipelining (c)
  and eight workers could approach another 2x. Combined, a first sync of a
  million small documents could drop from hours toward tens of minutes (medium
  confidence on step a/b, low-medium on c; unverified).
- Effort and risk: steps (a) and (b) are small and localized to
  `transfer.rs`/`sftp.rs`/`wire.rs`; step (c) is a moderate rework of the
  synchronous `Session::request` API. What could regress: error
  classification (write failure vs destination change) must stay
  distinguishable for the conflict machinery, and the digest-vs-plan check in
  `send_on` (`transfer.rs:516-526`) must keep its guarantee. Servers that
  mishandle FSTAT/FSETSTAT on handles need a capability fallback.
- Measurement plan: time the ADR 0046 metric "300 x 1 KiB SFTP files" with and
  without the 5 ms relay before and after; count round trips per file by
  logging `Session::request` calls in a test build. Add a criterion bench
  `sftp_upload_300_small` beside the existing observe benches.

### 3. Hash planned creates at copy time, not at observe time

- Category: redundant work and caching
- Evidence: observe hashes every new local file to compute its version
  (`crates/foldover-endpoint/src/local.rs:1207` calls `entry_version`, which
  hashes at `local.rs:1233-1255` via `hash_file`, `local.rs:852-871`), and the
  copy then reads the file again and hashes the stream again to verify it
  against the plan (`transfer.rs:486-511`, the
  `digest.borrow_mut().update(chunk)` closure, checked at `transfer.rs:517`).
  `read_entry_from` (`local/foreign.rs:80-95`) re-reads the data even for a
  resumed copy. So every byte of a new file is read twice from the source disk
  and hashed twice on a first sync.
- Hypothesis: for a first sync of millions of documents, the observe-time
  hash is redundant for rows whose decision does not compare content: when the
  other side is absent and the base has no row, the plan creates the file
  whatever its digest is. The digest needed for the base record is already
  computed during the copy (`transfer.rs:510-511`), so deferring it would
  remove one full read and hash pass over the corpus — up to half of the
  source-side I/O of a first sync.
- Proposed change: give `ObservedEntry` a lazily computed content digest for
  files that are new on their side and absent on the other: the planner
  compares presence and metadata only for such rows, the copy computes the
  digest while streaming (as it already does), and `Executor::record`
  (`run.rs:1712-1723`) records that streamed digest as the source version.
  Rows where both sides hold the file, or the base holds one, keep today's
  eager hash because the decision compares digests.
- Expected impact: first-sync observe of a new corpus reads roughly half as
  much source data; end-to-end for a create-heavy first run improves by an
  estimated 20-40 percent when the source disk is the bottleneck (low-medium
  confidence; unverified). No effect on incremental runs.
- Effort and risk: high. It touches the plan/execute contract: the
  "content digest is checked against the plan" verification
  (`transfer.rs:1-8` header, `transfer.rs:517`) exists to catch a source that
  changes between plan and copy; with a lazy digest that check degenerates to
  the stat-before/stat-after guards in `read_observed`
  (`foldover-fs/src/root/content.rs:83-131`). The preview UI shows versions
  (`run.rs:337`), which would be unavailable for such rows before execution.
  This weakens a documented verification property and therefore needs design
  sign-off; if the sign-off is refused, discard this proposal.
- Measurement plan: build a 100,000-small-file corpus and time a first sync to
  an empty SFTP tree; count source bytes read (`iostat`/`fs_usage` on macOS)
  before and after; confirm the journal and base records carry the same
  digests as today (compare `PairStore::base` output across runs).

### 4. Parallelize SFTP directory listing during observe

- Category: concurrency and parallelism
- Evidence: `crates/foldover-endpoint/src/sftp.rs:495-505` walks directories
  one at a time on one session (`while let Some(directory) = pending.pop()`
  then `read_dir`), and `foldover-net/src/sftp.rs:267-301` `read_dir` is
  OPENDIR + one-or-more READDIR + CLOSE, each a synchronous round trip. File
  reads after the listing are already parallelized over up to four sessions
  (`sftp.rs:588-629` `observe_reads` via `sftp/readers.rs:46` `read_each`), but
  the listing walk that feeds them is serial.
- Hypothesis: with millions of documents spread over many folders, the
  listing phase costs at least three round trips per directory in sequence:
  100,000 directories at 1 ms round trip is five minutes before any file is
  checked. Spreading sibling directories across the session pool the way
  `observe_reads` spreads files would cut the listing phase by up to the
  number of sessions, without changing any semantics (ordering of the
  observation is already normalized afterwards, `sftp.rs:619-624`).
- Proposed change: collect a level of pending directories, hand each to
  `read_each`-style workers that borrow pooled sessions and return listings,
  recording results on the calling thread as `observe_reads` already does; or
  more simply, pipeline OPENDIR/READDIR/CLOSE of several sibling directories
  on one session using the existing in-flight id machinery. Keep the
  "folders settle first" rule (`sftp.rs:536-548`) by joining the listing phase
  before file reads start.
- Expected impact: listing phase of an observe over a deep/wide tree drops up
  to 3-4x with four sessions; end-to-end on a compare run improves by the
  listing share of its runtime (medium confidence on the phase, low-medium on
  end-to-end; unverified).
- Effort and risk: moderate; reuses `readers::read_each` patterns but must
  keep cancellation responsive and results deterministic (the code sorts
  issues precisely because reads land in any order). What could regress:
  error attribution of a failed listing and the "a folder that fails rules
  out what is below it" rule (`sftp.rs:537-538`) need careful merging.
- Measurement plan: extend the ADR 0046 relay harness with a tree of 20,000
  directories x 10 files; time the listing phase separately (progress already
  reports `ObserveStep::Listed`) before and after, with 0 ms and 5 ms relay
  latency.

### 5. Parallelize local content hashing during observe

- Category: concurrency and parallelism
- Evidence: `crates/foldover-endpoint/src/local.rs:1168-1225` computes each
  changed entry's version serially in one loop (`entry_version` at
  `local.rs:1233-1255` reads and hashes the file with a 64 KiB buffer,
  `foldover-fs/src/root/content.rs:12`). The SFTP side already parallelizes
  exactly this pattern with `sftp/readers.rs:46` `read_each`; the local side
  has no equivalent. The scan budget `scan_10k_files` in
  `perf/budgets.toml` sets a floor of 10,000 entries/s for a cold scan, and
  `hash_64_mib` bounds single-file hashing at 60 ms per 64 MiB.
- Hypothesis: on a first sync of millions of documents every file is hashed
  (no fingerprint proves it unchanged yet), and the serial loop caps observe
  at one file at a time — syscall-bound for small files, bandwidth-bound for
  large ones. Bounded parallelism across files (4-8 workers on an SSD,
  matching `LocalEndpoint::workers`, `local.rs:743-748`) would approach the
  storage's aggregate throughput and cut the first-run observe phase several
  fold.
- Proposed change: move `read_each` from `sftp/readers.rs` to a shared spot in
  `foldover-endpoint` (or duplicate the small helper) and run
  `entry_version` over a batch of entries with `LocalEndpoint::workers()`
  threads, recording on the calling thread as the SFTP path does. Keep the
  existing per-entry identity checks inside `entry_version`; only the
  independent reads move off the loop.
- Expected impact: observe phase of a create-heavy first run over local files
  improves an estimated 2-4x on SSD (medium confidence for small files, lower
  for a spinning disk where `workers()` is already 1; unverified).
- Effort and risk: moderate; the pattern exists, the risk is cancellation
  responsiveness and unstable-directory bookkeeping (`scan_filtered` marks
  `unstable` directories, `local.rs:1010-1019`) staying deterministic.
  Extra open file descriptors and readahead competition on slow disks could
  regress single-stream throughput — gate on `Root::medium`
  (`foldover-fs/src/root.rs:395-440`) as copies already do.
- Measurement plan: add a criterion bench `hash_10k_small_parallel` beside
  `scan_10k_files`, hashing 10,000 small files serially vs with N workers;
  also compare `PROPTEST_CASES` unaffected and rerun `cargo xtask bench-check`
  for budget regressions.

### 6. Stop cloning whole version maps when preparing a plan

- Category: allocation and memory layout
- Evidence: `crates/foldover-endpoint/src/local.rs:584-589`
  `Observation::versions` builds a fresh `BTreeMap<SyncPath, EntryVersion>`
  copying every path, and `crates/foldover-engine/src/run.rs:867-868` calls it
  for both sides to hand the planner its inputs. `RunInputs` additionally
  carries full `left_snapshot`/`right_snapshot` maps (`run.rs:584-585`), and
  `PairStore::base`/`snapshot` (`crates/foldover-state/src/pairs.rs:254`,
  `:353`) materialize further full copies. For millions of entries the run
  holds four or more whole-corpus maps of path bytes plus per-entry
  fingerprints (`Fingerprint` stores a heap `Vec<u8>` stat key,
  `local.rs:293-301`) at plan time.
- Hypothesis: at one to two million entries, cloning and re-keying these maps
  costs hundreds of megabytes of transient allocation and seconds of
  allocator and cache traffic per run, and contributes to paging on 8-16 GB
  machines. Borrowed views (plan over `&BTreeMap<SyncPath, ObservedEntry>`
  with an accessor, or an iterator of `(&SyncPath, EntryVersion)`) would
  remove most of it.
- Proposed change: change `plan_candidates`/`plan_resolved`
  (`plan.rs:461-530`) to take a small trait or iterator of
  `(&SyncPath, EntryVersion)` instead of `&BTreeMap<SyncPath, EntryVersion>`;
  delete `Observation::versions` and pass the observation maps directly.
  Keep `validate_tree` (`plan.rs:538-540`) over the same views.
- Expected impact: plan-phase allocation roughly halves and RSS at plan time
  drops by one to two copies of the corpus index; wall-clock improvement
  maybe 5-15 percent of the prepare phase at million-entry scale (low
  confidence on wall clock, high confidence on allocation volume; unverified).
- Effort and risk: moderate and mechanical; blast radius is `plan.rs`,
  `run.rs` and their tests plus the simulation call sites. Little behavioral
  risk since ordering and contents stay identical.
- Measurement plan: run `plan_100k_entries` (`crates/foldover-engine/benches/plan.rs`)
  and a 1M-entry variant under `/usr/bin/time -l` (macOS) or `valgrind
  --tool=massif` (Linux) before and after; compare peak RSS and the criterion
  median against `perf/budgets.toml`.

### 7. Replace per-row linear scans of failures and unavailable roots with indexes

- Category: algorithmic complexity
- Evidence: `crates/foldover-engine/src/run.rs:1707-1709`
  `Executor::below_failure` scans the whole `failed` vector with a
  path-prefix comparison for every row it checks (callers at `run.rs:1704`
  and `run.rs:1740` run per plan row), so a run with F failures over R rows
  costs O(R x F) prefix comparisons. Similarly
  `crates/foldover-endpoint/src/sftp.rs:578-581` and `sftp.rs:1713-1717`
  scan `observation.unavailable` per entry, and
  `local.rs:449-455` `UnavailablePaths::contains` does the same per scanned
  entry.
- Hypothesis: normally `failed` and `unavailable` are tiny, but the workload
  makes them large exactly when things go wrong: permission errors on a
  subtree of an SFTP target or a flaky server can put thousands of roots into
  `failed`/`unavailable`, and then every remaining row of a million-row plan
  pays a linear scan — minutes of avoidable CPU on the run thread, which also
  owns commits and progress.
- Proposed change: keep `failed` and `unavailable` sorted by path and test
  "below a failed root" by walking the path's ancestors in a `BTreeSet`
  (O(depth x log F) per query), or maintain the set of blocked subtree roots
  as a prefix trie. `SyncPath` already offers `is_ancestor_of`
  (used at `run.rs:1708`); only the container changes.
- Expected impact: eliminates a quadratic worst case; on healthy runs the
  change is neutral, on failure-heavy runs it can save minutes (medium
  confidence in the complexity fix, low on frequency; unverified).
- Effort and risk: small and contained in `run.rs`/`sftp.rs`/`local.rs`; the
  risk is a semantics slip around "a failure suppresses its descendants"
  ordering, which the regression tests in `run/tests.rs` already cover.
- Measurement plan: a unit test that plans 100,000 rows with 5,000 failed
  roots and asserts completion time under a bound (mirroring the ignored
  `unicode_respelling_prepare_budget` wall-clock test pattern in
  `foldover-engine`), plus `cargo xtask bench-check` to confirm no plan
  budget regression.

### 8. Batch base commits to cut per-batch fsyncs

- Category: I/O and batching
- Evidence: `crates/foldover-engine/src/run.rs:51-52` commits base updates in
  batches of `COMMIT_BATCH = 256` rows ("bounds the work a crash can make the
  next run redo"), each a transaction in `change_base`
  (`crates/foldover-state/src/pairs.rs:365-395` /
  `crates/foldover-db/src/database.rs:945-1003`) against a database opened
  with `synchronous = FULL` and WAL (`database.rs:647-652`). The stored
  budget `db_commit_100k` is 1000 ms per 100,000 base entries
  (`perf/budgets.toml`).
- Hypothesis: copying a million new files produces about 3,900 commit
  transactions, each with a WAL fsync at `synchronous = FULL`; on APFS that
  is seconds to tens of seconds of pure commit latency interleaved with the
  copy loop on the run thread. The batch size bounds crash-redo work, and
  the code already states the recovery rule ("a crash loses at most an
  uncommitted batch; the next run finds both sides equal and records them",
  `run.rs:13-18`), so a larger batch only enlarges safe redo.
- Proposed change: raise `COMMIT_BATCH` to a few thousand rows or make it
  byte/time-based (e.g. flush every 16 MiB of journal or every 2 s), keeping
  the same single-writer revision checks. Do not change `synchronous`:
  durability of the state store is a promise, not a tuning knob.
- Expected impact: commit count per million-file run falls about 10x,
  saving an estimated 5-30 s of run time and smoothing progress stalls on
  the run thread (medium confidence on the mechanism, low-medium on
  magnitude; unverified).
- Effort and risk: tiny change, tiny blast radius; the tradeoff is more redo
  after a crash (bounded by the next run's reconciliation, which is tested
  by the kill -9 suites in PLAN.md §5.2). Watch that `Commit::Base`
  consumers (`foldover-service`) keep up with larger payloads.
- Measurement plan: time `PairStore::update_base` of 100,000 entries with
  batch sizes 256/2048/8192 via the `db_commit_100k` bench; count fsyncs in
  a run with `dtruss -c`/`strace -c` on the agent process during a
  100k-file simulation.

### 9. Build the release engine with LTO and one codegen unit

- Category: build and startup configuration
- Evidence: the workspace manifest (`Cargo.toml`) sets no `[profile.release]`
  (it ends at `[workspace.lints]`), so `scripts/build.sh` and
  `cargo xtask ffi` ship default-codegen release binaries; `crates/*/Cargo.toml`
  pin `blake3 = "1.8.2"` with default features. The stored budgets
  `hash_64_mib`, `plan_100k_entries` and `scan_10k_files` are the CPU-side
  gates to compare against.
- Hypothesis: thin LTO with `codegen-units = 1` typically buys single-digit
  to low-double-digit throughput on the hashing and planning loops that run
  over every entry and every byte, at zero behavioral risk; `panic = "abort"`
  must not be used because copy workers rely on `catch_unwind`
  (`run/copies.rs:211`) for panic isolation.
- Proposed change: add `[profile.release] lto = "thin", codegen-units = 1`
  to the workspace `Cargo.toml` (leaving panic handling as-is), and record
  the before/after medians in `perf/README.md` per its update procedure.
- Expected impact: 2-10 percent on CPU-bound phases (hash, plan, snapshot
  merge); negligible for network-bound copy time (low confidence; unverified).
- Effort and risk: one manifest line; build times rise somewhat and the
  release pipeline must still produce the XCFramework and app zip. CI's
  `bench-check` catches regressions against `perf/budgets.toml`.
- Measurement plan: `cargo xtask bench-check` on an idle machine before and
  after, comparing every budget id's median.

## Requires design sign-off

Two entries above weaken verification properties the code currently claims,
so they should not land without an explicit owner decision (the constraints
block for this study is empty, but the repository's invariants are not):

- **Proposal 1** trades same-size, same-mtime remote-edit detection within a
  re-read window for speed. PLAN.md §4.8 already accepts a weekly content
  pass, and the local side accepts a seven-day window, but the SFTP comment
  at `sftp.rs:469-471` is a deliberate stance today.
- **Proposal 3** removes the plan-vs-landed digest check for created files,
  leaving the stat-before/stat-after guards as the only protection against a
  source changing between preview and copy.

## Open questions and unverified items

- Nothing in this study was executed or profiled: no benchmark, sync run, or
  profiler session was performed in this session. Every magnitude above is a
  hypothesis from reading code; only the ADR 0046 tables and
  `perf/budgets.toml` contain recorded measurements, and those were produced
  by earlier work on synthetic data ("No NAS or real WAN was measured",
  ADR 0046).
- The true round-trip count per small-file upload should be confirmed with an
  instrumented `Session::request` counter; the 7-8 figure in Proposal 2 is
  derived from reading `transfer.rs`/`sftp.rs`, not from a trace.
- Whether the copy phase or the comparison phase dominates for the user's
  corpus depends on the change rate: steady-state incremental runs are
  dominated by Proposal 1's re-read, first runs by Proposals 2, 3 and 5.
  Without a profile of the actual workload the ranking between proposals 2
  and 1 for a given user could invert.
- Per-file digest verification during upload (`transfer.rs:517`) and the
  stat re-checks around every read are safety features; none of the proposals
  remove them except Proposal 3, and only for rows the plan creates outright.
- The double `fstatat` per local scan entry (once in the readdir iterator,
  `foldover-fs/src/root.rs:386-399`, once in `scan_entry`,
  `local.rs:1073`) is a deliberate race check; replacing it with bulk stat
  enumeration (e.g. `getattrlistbulk` on macOS) could speed cold scans
  further but weakens a TOCTOU guard and was left out of the ranked list.
- Chunk and window sizing (`foldover-net/src/sftp.rs:32-37`, 32 KiB x 16 =
  512 KiB in flight per session) is secondary for small documents but would
  matter for large ones against PLAN.md's "SFTP large files on 1 GbE
  >= 100 MB/s" budget (PLAN.md line 681); not pursued here.
- `align_spellings` (`run.rs:1511-1562`) normalizes paths with NFC only when
  many base spellings go missing and returns early otherwise
  (`run.rs:1522-1524`); its cost at million-path scale is unverified and it
  has a dedicated budget test (`unicode_respelling_prepare_budget`).
- SFTP-to-SFTP pairs copy one file at a time by design (`run.rs:1432-1440`,
  ADR 0043); out of scope for this workload, which copies from a local
  folder.
