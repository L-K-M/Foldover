# Performance proposals: hot paths for syncing millions of documents to an SFTP server

Foldover is a macOS app over a portable Rust engine (`crates/`) that syncs folder
pairs between local disks, NAS shares, SFTP servers and other Macs. For an SFTP
side, one run has three phases over the wire: **observe** (recursive `READDIR`
listing, then a per-file version computed by reading and BLAKE3-hashing the whole
file — `crates/foldover-endpoint/src/sftp.rs` `observe_reporting`/`observe_reads`/
`version_on`/`read_range_on`), **plan** (`crates/foldover-engine/src/plan.rs`
`plan_resolved`), and **apply** (per-file staged upload, metadata set, place rename,
driven by `crates/foldover-engine/src/run.rs` over `PARALLEL_COPIES` copy workers on
a pool of `SESSIONS` `ssh` subprocess sessions speaking SFTP v3 in
`crates/foldover-net/src/sftp.rs`). The workload studied here is comparing and
copying millions of documents to an SFTP server on the local network; the primary
metric is end-to-end wall-clock time of a sync/backup run. Because the metric is
per-file latency and bytes moved rather than raw bandwidth, the proposals below
target round trips per file, redundant content reads, and CPU-side per-entry costs.
They are ranked by expected impact on that metric, weighted by effort and risk.
A sibling pass already recorded proposals in
[lan-sftp-end-to-end-sync.md](lan-sftp-end-to-end-sync.md); this document adds the
hot paths that pass did not cover (parallel listing, per-file request counts in the
copy path, read-window sizing, planner complexity, allocation churn) and re-derives
the shared ones against the current tree. All citations were read in source at the
current checkout; nothing was executed or profiled in this session, so every impact
magnitude is a hypothesis to be confirmed by the stated measurement plan, not a
measured fact. The existing criterion benches and budgets in
[perf/README.md](../../perf/README.md) (`scan_10k_files`, `hash_64_mib`,
`plan_100k_entries`, `db_*_100k`) are the natural harness for the CPU-side items.

## Ranked proposals

1. Skip the content re-read for stat-matched SFTP entries, with a bounded re-hash spread
2. Parallelize the SFTP directory-listing walk across the session pool
3. Cut redundant per-file round trips in the copy-to-SFTP path
4. Pipeline `READDIR` batches inside `Session::read_dir`
5. Size the SFTP read window to the file being hashed
6. Raise the streaming window and copy parallelism for LAN SFTP
7. Replace per-row ancestor walks in planning with a single downward pass
8. Cut path-buffer allocation churn in the SFTP hot path
9. Add a release profile with LTO for the engine crates

---

### 1. Skip the content re-read for stat-matched SFTP entries, with a bounded re-hash spread

- **Category:** redundant work and caching
- **Evidence:** `crates/foldover-endpoint/src/sftp.rs:631-659` — `observe_entry` calls `self.version(...)` at line 640 *before* consulting `previous` at lines 646-651. For a file, `version` (sftp.rs:661-679) → `version_on` (sftp.rs:681-710) → `read_range_on` (sftp.rs:750-805), which opens the file, reads every byte over SFTP, BLAKE3-hashes it, and finishes with a trailing `lstat` (sftp.rs:797-800). The comment at sftp.rs:644-647 states the intent: "SFTP versions are never reused unread". The local endpoint does the opposite ordering: `crates/foldover-endpoint/src/local.rs:1199-1203` checks `previous.get(&entry.path)` with `fingerprint.proves_unchanged(...)` *before* hashing, and `Fingerprint::proves_unchanged_by` (local.rs:330-337) already implements the safety logic — racy window (`RACY_WINDOW`, local.rs:283) plus a per-key hash-spread re-read deadline (`reread_after`/`read_within`, local.rs:341-355, `REREAD` local.rs:288). `Fingerprint::same_stat` is already `pub(crate)` (local.rs:326-328) and SFTP fingerprints are built with `Fingerprint::from_key` (sftp.rs:655).
- **Hypothesis:** on every run after the first, the SFTP-side scan downloads and re-hashes 100% of file bytes even when size+mtime+mode (the SFTP stat key, sftp.rs:1735-1745) is unchanged — which is the common case for a document corpus. For millions of documents this is likely the largest single term in end-to-end time: pure network I/O that the local endpoint already avoids.
- **Proposed change:** in `SftpEndpoint::observe_entry`, before calling `version()`, look up `previous.get(path)` and, when `known.fingerprint.same_stat(&key, size)` holds and the fingerprint's `observed_at` is past the racy window relative to the listed mtime and within `reread_after()` of now, return `known.clone()` without any read. Port the `proves_unchanged_by` predicate onto `Fingerprint` so local and SFTP share one implementation. Keep the always-read behavior available behind a pair option for conservative users.
- **Expected impact:** on steady-state runs over a mostly unchanged tree, scan-phase network bytes drop from ~100% of corpus size to ~the changed fraction, so scan wall time falls from "read everything at LAN throughput" to "listings plus reads of changed files" — plausibly 30-60% off total end-to-end time for a million mostly unchanged documents, and near-zero cost on a first sync. Confidence: **high** that the redundant read happens (verified in code), **medium** on the end-to-end magnitude (depends on the changed-file fraction and on RTT vs. throughput balance).
- **Effort and risk:** medium — a reordered check plus a shared predicate; blast radius is `observe_from`/`observe_reporting` for SFTP roots. Regression risk: an edit that keeps size and whole-second mtime is missed until that entry's spread re-read (up to `REREAD`, 7 days) — the same tradeoff local roots already accept, but it reverses a stated SFTP stance (sftp.rs:470-472), so the safety argument and the option gate must be explicit in the PR.
- **Measurement plan:** use the spawned-`sftp-server` harness from `crates/foldover-endpoint/src/sftp/sessions.rs` tests (`FOLDOVER_SFTP_SERVER`) to build a 100k-file tree; run `observe_from` once to populate `previous`, then time a second `observe_from` and count `READ` requests/bytes (instrument `Session::read_from`, `crates/foldover-net/src/sftp.rs:339-415`, or a request counter around `Session::request`, sftp.rs:689-694) before and after. End-to-end: run the CLI sync twice against the fixture and take the second run's wall clock.

### 2. Parallelize the SFTP directory-listing walk across the session pool

- **Category:** concurrency and parallelism
- **Evidence:** `crates/foldover-endpoint/src/sftp.rs:495-543` — the listing phase is a serial `while let Some(directory) = pending.pop()` loop that borrows one session per directory (`session_unless_cancelled(control)?.read_dir(&remote)` at sftp.rs:503). Each `read_dir` costs OPENDIR + one READDIR per server batch + CLOSE, all serialized (`crates/foldover-net/src/sftp.rs:267-301`, where `request` at sftp.rs:689-694 is send-then-wait). The file-read phase right below already fans out over the pool: `observe_reads` (sftp.rs:588-627) calls `readers::read_each` (sftp.rs:608; `crates/foldover-endpoint/src/sftp/readers.rs:55-92`) with `workers()` sessions (sftp.rs:220-227, `PARALLEL_COPIES` = 4, `SESSIONS` = 5, sftp.rs:68-70). During observe no run mutations happen, so the shared `listings` cache (`Mutex<HashMap<.., FolderView>>`, sftp.rs:103-105, `spelling.rs:145-198`) is only read, not mutated.
- **Hypothesis:** for a tree with many directories (millions of documents in tens of thousands of folders), the listing phase pays 3+ serialized round trips per directory on a single session while four other sessions sit idle; fanning the walk out over the pool cuts listing wall time roughly by the worker count.
- **Proposed change:** distribute pending directories across the pooled sessions the way `readers::read_each` distributes files — e.g. a parallel BFS where each worker pops directory paths from a shared atomic index and lists them on its own borrowed session, pushing discovered subdirectories back; land results on the run thread exactly as `read_each` does. Keep the `listings`/`note_*` discipline untouched (no mutations during observe).
- **Expected impact:** listing-phase wall time up to ~4x faster on many-directory trees; on end-to-end time this is large for listing-heavy trees (deep or many-folder corpora) and small for a few huge folders — estimate 10-40% of end-to-end on a first sync of a many-folder tree, less on steady state. Confidence: **medium** (mechanism is certain; magnitude depends on the directory/file ratio, which is unknown for the target tree).
- **Effort and risk:** medium — new parallel walk reusing the `readers.rs` pattern and the session pool; risk is session-pool contention with the lease session and cancel handling, both already solved in `readers.rs`. No correctness change: results still land on the run thread in a deterministic post-pass (the existing code already sorts `observation.issues`, sftp.rs:622-626).
- **Measurement plan:** fixture `sftp-server`, tree of 20k directories × 50 files; time the `ObserveStep::Listed` phase (progress callback) before/after, and count `OPENDIR` requests per unit time. End-to-end: second-run wall clock (after proposal 1, listing dominates) on the fixture vs. a real NAS share.

### 3. Cut redundant per-file round trips in the copy-to-SFTP path

- **Category:** I/O and batching
- **Evidence:** per copied file, `send_on` (`crates/foldover-endpoint/src/sftp/transfer.rs:446-531`) issues, strictly serialized within the file: `receiving_on` → `attributes_on` → `lookup_on` → `lstat` (`spelling.rs:94-118`, lstat at :101); `upload_on` → `write_new` → `create_new` (OPEN) + pipelined WRITEs + CLOSE (`sftp.rs:888-919`, `write_new` at sftp.rs:1672-1688); then `set_metadata_on` does its own `lstat` (sftp.rs:947); then `staged_version_on` does another `lstat` (sftp.rs:928); then `place_on` → `renamed_on` (sftp.rs:1034-1081). That is ~6 sequential round trips per file plus the data. For a *new* file (`Expected::Absent`), `place_on` takes the `None` branch (sftp.rs:1057-1066) and needs no lookup, and `receiving_on`'s lstat is redundant with `place_on`'s needs.
- **Hypothesis:** for millions of small documents, the apply phase is dominated by fixed per-file request chains, not bytes; merging the two post-write `lstat`s (set-metadata + staged-version) into one `lstat` after `set_attributes`, and skipping the destination lstat when the plan row is a create into a known-empty slot, removes 1-2 RTTs out of ~6 per file.
- **Proposed change:** in `send_on`'s file branch (transfer.rs:505-528), restructure to: OPEN+WRITE+CLOSE once, then a single `lstat`, then `set_attributes` only if mode/times actually differ (the early-return already exists at sftp.rs:968-970 but only after its own lstat), then derive the staged version from the post-`set_attributes` attributes (re-lstat only when `set_attributes` was actually sent). Pass the `receiving_on` result down so `place_on` does not re-stat a path already verified absent.
- **Expected impact:** ~15-30% off apply-phase wall time for small-file-heavy runs at 4-8 workers (1-2 of ~6 RTTs removed per file); negligible for large files. Confidence: **medium-high** on request-count reduction (verified by reading), **medium** on wall-clock magnitude (depends on RTT and worker count).
- **Effort and risk:** small-medium, contained in `send_on`/`place_on`/`set_metadata_on`/`staged_version_on`; risk is recording a version from stale attributes — must keep the "recorded reality" rule (PLAN.md §4.10: record what the target actually stored), so the lstat that produces the recorded version must be the *last* metadata-affecting call before close/rename.
- **Measurement plan:** shim `Session::request` (net/sftp.rs:689-694) to count request types during a 10k-small-file first sync to the fixture server; compare per-file request count and end-to-end wall clock before/after. Existing suite: `crates/foldover-endpoint/src/sftp/tests.rs` covers place/replace/undo semantics.

### 4. Pipeline `READDIR` batches inside `Session::read_dir`

- **Category:** I/O and batching
- **Evidence:** `crates/foldover-net/src/sftp.rs:267-301` — `read_dir` sends one `READDIR` and blocks on its reply per iteration (`self.request(...)` at :273-276; `request` at :689-694 is send+wait). The same session already pipelines data requests: `read_from` queues `WINDOW` = 16 reads before collecting (sftp.rs:339-415, send loop :350-366), `queue_write` keeps `WINDOW` writes in flight (sftp.rs:471-500), and out-of-order replies are stashed up to `MAX_STASHED = WINDOW` (sftp.rs:46, `receive_for` :740-767).
- **Hypothesis:** a directory with hundreds of thousands of entries needs one RTT per server-chosen READDIR batch; pipelining up to `WINDOW` READDIRs on the open handle cuts that nearly 16x for wide directories, which matters when millions of documents sit in few large folders.
- **Proposed change:** in `read_dir`, send up to `WINDOW` READDIR requests for the handle up front, then consume replies in id order with the existing `receive_for` stash, stopping at the first EOF reply and draining the remaining owed replies (the `drain` at sftp.rs:782-801 already exists); keep CLOSE last. Reuse the `pending: VecDeque` pattern from `read_from`.
- **Expected impact:** up to ~10x lower listing latency for very wide directories; small-to-medium end-to-end effect overall (listing is usually a minority of run time unless directories are huge). Confidence: **medium-high** for wide directories, **low-medium** on total end-to-end.
- **Effort and risk:** small, self-contained in `read_dir`; the desync risk is bounded by the existing `MAX_STASHED` cap and drain-on-error discipline that `read_from` already proves in production.
- **Measurement plan:** fixture server, one directory with 100k entries; count READDIR packets and wall time of `read_dir` before/after (shim `Session::request`). Scale-up check: a flat 200k-file tree scan via an `sftp` variant of the `scan_10k_files` bench.

### 5. Size the SFTP read window to the file being hashed

- **Category:** redundant work and caching (request-count reduction)
- **Evidence:** `Session::read_from` (`crates/foldover-net/src/sftp.rs:339-415`) always sends a full `WINDOW` = 16 READ requests per window (send loop :350-366), regardless of file size. The caller knows the size: `read_range_on` (`crates/foldover-endpoint/src/sftp.rs:750-805`) holds `expected.size` and enforces it (overrun check :783-787, short-read check :793-795). `version_on` (sftp.rs:681-710) drives this for every file during observe.
- **Hypothesis:** for documents smaller than 32 KiB (one `CHUNK`, sftp.rs:32), each file generates ~16 READ packets of which 1-2 carry data; the rest return EOF and are discarded (`finished`/`short` handling :388-409). For millions of small documents during the observe-hash phase that is ~8-16x more packets than needed, costing CPU and syscalls on both ends and inflating per-file packet processing even though they are pipelined into one RTT.
- **Proposed change:** give `read_from` an optional known length and set the per-window request count to `min(WINDOW, ceil(remaining / CHUNK))` (at least 1); same for the trailing partial window. No protocol change — servers already answer short/EOF.
- **Expected impact:** large reduction in packet count per small file (16 → 1-2) during scan-hash; wall-clock gain is smaller because the requests were pipelined, but per-file CPU and server-side work drop substantially — estimate 10-30% of the observe phase for small-file corpora, a few percent end-to-end. Confidence: **medium** (packet-count fact verified in code; wall-clock effect is a hypothesis until measured).
- **Effort and risk:** small; risk is mishandling the EOF/short-read edge cases — covered by keeping the existing `finished`/`short` logic and the `read_from` tests in `crates/foldover-net/src/sftp/tests.rs`.
- **Measurement plan:** instrument a packet counter in `Session::send_for` (sftp.rs:696-701) during `observe_from` of 10k small files on the fixture server; compare READ-request counts and observe-phase wall time before/after.

### 6. Raise the streaming window and copy parallelism for LAN SFTP

- **Category:** concurrency and parallelism (configuration)
- **Evidence:** `crates/foldover-net/src/sftp.rs:32-46` — `CHUNK` = 32 KiB, `WINDOW` = 16 (512 KiB in flight per session per direction), `MAX_PACKET` = 1 MiB, `MAX_STASHED = WINDOW`. `crates/foldover-endpoint/src/sftp.rs:68-70` — `PARALLEL_COPIES` = 4, `SESSIONS` = 5; `workers()` (sftp.rs:220-227) feeds `Run::workers` (`crates/foldover-engine/src/run.rs:1430-1441`, min of both sides, 1 for SFTP↔SFTP). The pool already clamps adaptively when a server refuses sessions (`crates/foldover-endpoint/src/sftp/sessions.rs`, `REFUSALS`/`RETRY`, `acquire`). PLAN.md §4.9 sanctions "network 4-8 connections with pipelined requests".
- **Hypothesis:** per-session throughput ≈ `WINDOW × CHUNK / RTT`; on a 1-10 GbE LAN with ≥1 ms RTT the bandwidth-delay product exceeds 512 KiB, so large-file streams under-fill the pipe, and 4 copy workers under-use a server that allows more sessions. For millions of small files, more workers directly cut the fixed per-file request chains.
- **Proposed change:** raise `WINDOW` to 64 (2 MiB in flight; move `MAX_STASHED` with it) and `PARALLEL_COPIES` to 8, both as constants first, pair-configurable later; keep the adaptive pool clamp as the guard against `MaxSessions`. Optionally raise `CHUNK` with a fallback on oversized-write failure (the sibling pass covers chunk sizing in [lan-sftp-end-to-end-sync.md](lan-sftp-end-to-end-sync.md) proposal 5).
- **Expected impact:** up to ~2-4x on large-file transfer throughput under jitter/high BDP, and up to ~2x on the apply phase for small-file-heavy first syncs if the server accepts 8 sessions; little effect when bandwidth-bound. Confidence: **medium** (server `MaxSessions` and ssh-process contention may cap gains).
- **Effort and risk:** trivial (constants); risk is server-side load and ~1 MiB max-packet buffers per extra session — bounded and adaptive.
- **Measurement plan:** fixture server with configurable `MaxSessions`; one 1 GiB file up/down at WINDOW 16 vs 64; 100k small files first-synced at `PARALLEL_COPIES` 4 vs 8 vs 16, recording session-refusal clamping from `Sessions`. Also re-run `cargo xtask bench-check` to confirm no CPU-side regression.

### 7. Replace per-row ancestor walks in planning with a single downward pass

- **Category:** algorithmic complexity (data structures)
- **Evidence:** `crates/foldover-engine/src/plan.rs:605-630` — for *every* row, the loop walks `rows[index].path.parent()` up to the root, doing a `BTreeMap` lookup per ancestor; `SyncPath::parent()` allocates a new `Vec` per step (`crates/foldover-types/src/sync_path.rs:36-41`). The same parent-walk pattern appears in the deleted-directory check at plan.rs:546-576 and `settle_folder_choices` at plan.rs:279-306. Rows are already sorted so ancestors precede descendants (comment plan.rs:602-604). The existing budget `plan_100k_entries` = 350 ms (`perf/budgets.toml`) shows this phase is measurable.
- **Hypothesis:** the ancestor-blocking pass is O(n × depth × log n) with an allocation per ancestor; at 1M entries and depth ~6 that is millions of lookups plus allocations — seconds of CPU on the run thread while the network waits.
- **Proposed change:** exploit the sort order: one forward pass over `rows`, carrying each row's blocked/conflict state downward — when row `i` is blocked or a conflict, mark the span of its descendants (computable from the next row whose path is not below it) instead of walking up from every descendant. Same rewrite for the deleted-directory ancestor check, which can propagate downward from changed directories instead of walking up from changed rows.
- **Expected impact:** plan phase from O(n·depth) to O(n); plausibly 2-5x on `plan_100k_entries`-scale inputs at 1M entries, i.e. a few percent of end-to-end time (plan is CPU-only and usually small next to network I/O). Confidence: **medium** on the complexity win, **low-medium** on end-to-end magnitude.
- **Effort and risk:** medium — decision-equivalence must be proven; `plan.rs` has unit tests and `crates/foldover-engine/src/plan_properties.rs` property tests to lean on. Risk of changing conflict/blocked semantics if the downward propagation misses a case.
- **Measurement plan:** `cargo xtask bench-check` (`plan_100k_entries`) before/after, plus a new 1M-entry variant of the bench; run the `plan_properties` proptests with `PROPTEST_CASES=500000` (AGENTS.md) to confirm identical decisions.

### 8. Cut path-buffer allocation churn in the SFTP hot path

- **Category:** allocation and memory layout
- **Evidence:** `SftpEndpoint::remote()` (`crates/foldover-endpoint/src/sftp.rs:253-259`) allocates a fresh `Vec<u8>` per call and is invoked from `on_stored`/`on_stored_with` (`crates/foldover-endpoint/src/sftp/spelling.rs:48-84`) on every lookup, read, upload and place; `child_path` in the listing loop (sftp.rs:525) builds a `SyncPath` per entry; the whole tree's `(SyncPath, Attributes)` pairs are held in one `listed` vec (sftp.rs:489-543) before the read phase. `SyncPath::parent()` allocates per call (sync_path.rs:36-41).
- **Hypothesis:** with millions of files, each observe/copy operation performs several path-length heap allocations; the allocator churn and cache misses are CPU-side costs that scale with entry count even when the network dominates. (Unverified by profiling — see measurement plan.)
- **Proposed change:** thread a reusable `Vec<u8>` scratch buffer through `remote()`-style joins (e.g. `remote_into(&self, path, &mut Vec<u8>)`) at the hot call sites (`on_stored_with`, `lookup_on`, `read_range_on`, `upload_on`/`place_on`), and consider boxing or compacting `Attributes` in the `listed` vec. Keep the public `Endpoint` API unchanged.
- **Expected impact:** likely single-digit to ~10% of end-to-end on very large trees (CPU-side only). Confidence: **low-medium** — hypothesis until profiled.
- **Effort and risk:** medium, many small call-site edits in `sftp.rs`/`spelling.rs`/`transfer.rs`; risk is a borrow conflict where a `&[u8]` remote path is held across another call — mechanical to resolve, covered by `crates/foldover-endpoint/src/sftp/tests.rs`.
- **Measurement plan:** `perf record` (or Instruments) on a 100k-file observe+apply against the fixture server, before/after; optionally a counting `#[global_allocator]` in a bench to compare allocation counts per entry.

### 9. Add a release profile with LTO for the engine crates

- **Category:** build and startup configuration
- **Evidence:** the workspace `Cargo.toml` defines no `[profile.release]` section (verified: no `profile` key in the root manifest), so release builds use Cargo defaults (`codegen-units = 16`, no LTO). Hot CPU code includes BLAKE3 streaming (`crates/foldover-fs/src/root/content.rs:12,103`, 64 KiB buffer), planning (`plan.rs`), and SFTP wire encode/decode (`crates/foldover-net/src/sftp/wire.rs`, where every request allocates a packet `Vec` — `Writer::finish`, wire.rs:158-164).
- **Hypothesis:** thin LTO plus `codegen-units = 1` improves the hash/plan/wire hot loops by a few percent; on a run that processes millions of entries for hours, that compounds.
- **Proposed change:** add to the workspace `Cargo.toml`:
  ```toml
  [profile.release]
  lto = "thin"
  codegen-units = 1
  ```
  keeping dev builds untouched.
- **Expected impact:** ~5-15% off the CPU-bound portions (hash, plan, wire codec) — a few percent of end-to-end for large trees. Confidence: **medium**.
- **Effort and risk:** trivial; risk is longer release link times and slightly larger binaries; no behavioral change.
- **Measurement plan:** `cargo xtask bench-check` (`hash_64_mib`, `plan_100k_entries`, `scan_10k_files`) on an idle host before/after, comparing medians per `perf/README.md`; record build-time delta.

---

## Requires approval

These ideas reverse documented decisions and are listed separately rather than ranked:

- **Pure stat-trust for SFTP versions (no re-read spread at all).** The strongest form of proposal 1 — reusing the stored version whenever the stat key matches, with no `REREAD`-spread re-hash — would fully reverse the stated stance that "SFTP stat cannot prove a file unchanged, even when size and modification time match" (`crates/foldover-endpoint/src/sftp.rs:470-472`) and the comment "SFTP versions are never reused unread" (sftp.rs:644-647). Proposal 1 keeps a bounded re-hash and mirrors the local endpoint's accepted tradeoff; dropping the re-read entirely needs an explicit product decision because SFTP mtimes have whole-second granularity.
- **Resuming interrupted copies to SFTP destinations.** `kept()` returns 0 for SFTP targets (`crates/foldover-endpoint/src/sftp/transfer.rs:336-357`, `Endpoint::Sftp(_) => Ok(0)` at :354) with the comment that pipelined writes may land out of order leaving holes. Resuming would mean lstat-ing the partial temporary and continuing from there; this reverses a decision documented in ADR 0034 and needs sign-off, especially since SFTP WRITE carries explicit offsets.
- **Pipelining `Session::copy_data`** (`crates/foldover-net/src/sftp.rs:502-551`), which strictly alternates READ and WRITE per chunk on one session for same-server copies (`stage_copy_on`, `crates/foldover-endpoint/src/sftp.rs:1505-1560`). Overlapping the next READ with the previous WRITE would halve its RTTs, but SFTP↔SFTP pairs currently run a single worker (`run.rs:1430-1441`), so the win is bounded; revisit together with that deadlock rationale.

## Open questions and unverified items

- Nothing was executed in this session: no build, bench, or profile was run (this pass only read source). Every impact figure above is a hypothesis; the per-proposal measurement plans are the way to confirm or refute them. The bench harness is `cargo xtask bench-check` (`perf/README.md`, `perf/budgets.toml`).
- The shape of the real tree (files per directory, depth, size distribution) is unknown; the relative weight of proposal 2 (many folders) vs. proposal 4 (few huge folders) vs. proposal 3 (per-file copy chain) depends on it. A quick `find | wc -l` per top directory on the NAS would settle it.
- Server-side facts are unverified: the target NAS's READDIR batch size, `MaxSessions`, and whether it accepts >32 KiB READ/WRITE requests (relevant to proposal 6 and to the chunk-size idea in the sibling doc).
- Whether the pair under study is local→SFTP (assumed; `copy_data` then only matters for same-server moves) or SFTP→SFTP (forced single worker at `run.rs:1430-1441`, where proposal 6's parallelism half does not apply).
- DB snapshot/commit costs (`crates/foldover-db/src/database.rs:806-899` `replace_snapshot` already diffs added/removed/changed rows in one transaction, and `COMMIT_BATCH` = 256 at `crates/foldover-engine/src/run.rs:52` bounds base commits) are benched (`db_*_100k`) and were not profiled here; assumed not to dominate the SFTP end-to-end time.
- The untracked `crates/foldover-endpoint/tests/` directory present in the working tree was not created by this pass and was left untouched.
