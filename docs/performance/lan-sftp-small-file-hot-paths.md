# Millions of documents over LAN SFTP: ranked performance proposals

Foldover previews and synchronizes folder pairs, including one-way mirrors and backups, through a native Swift UI or CLI, a background Rust service, and local/SFTP/peer endpoint drivers. Its main path is request → load state → open endpoints → observe both trees → reconcile and plan → gated, staged copies/archives → journal and base commits. This study targets end-to-end elapsed time when comparing and copying millions of documents to a LAN SFTP server; bytes transferred, requests per file, CPU time, and peak memory explain that mixed metric. The ranking favors broadly applicable reductions in protocol work, then replacement-heavy and directory-heavy improvements, then memory and CPU reductions, weighted by effort and correctness risk. Except for explicitly marked measurements, performance explanations and estimates are hypotheses, not promises; no real NAS or LAN sync was timed.

## Ranked proposals

1. Bound SFTP reads to the observed file size
2. Cache confirmed archive directories within a run
3. Parallelize the SFTP directory inventory
4. Index cached SFTP folder names
5. Plan against borrowed observation versions
6. Serialize snapshots once in path order
7. Borrow path prefixes during ancestor checks
8. Bound the SFTP startup access probe

## Scope, execution paths, and evidence quality

Analyzed source revision: `c997ae9d3231e862d74f74efb1e577d0fe07ff04`, fetched from `origin/main` on 2026-11-07. Line references below refer to that revision. The document branch subsequently incorporated `5be11da`, which only added another performance document; the analyzed source was unchanged. No existing repository file is changed by this study.

The initial layout review covered `README.md`, `Cargo.toml`, `scripts/build.sh`, `perf/README.md`, `perf/budgets.toml`, `xtask/src/bench.rs`, the plan/DB benches, and the CI workflows. Concrete execution-path locations, established by source inspection rather than a runtime profile:

- Entry points: `crates/foldover-cli/src/main.rs:255` and `crates/foldoverd/src/main.rs:24`.
- Job preparation and persistence: `crates/foldover-service/src/sync.rs:844-977`, `perform`; state loading occurs under the engine lock at lines 854-858.
- Tree observation and planning: `crates/foldover-engine/src/run.rs:696-900`, `PairRun::prepare_on`; left and right observations are sequential at lines 789-790. Planning consumes materialized version maps at lines 869-876.
- SFTP inventory and content comparison: `crates/foldover-endpoint/src/sftp.rs:482-804`, `observe_reporting`, `observe_reads`, and `read_range_on`.
- Application: `crates/foldover-engine/src/run.rs:997-1331`, `execute`; `crates/foldover-engine/src/run/copies.rs:57-118` dispatches bounded concurrent copies. `crates/foldover-endpoint/src/sftp/transfer.rs:448-532`, `send_on`, stages, hashes, checks metadata, archives, and places uploads.
- Persistence: `crates/foldover-state/src/pairs.rs:254-295` and `crates/foldover-db/src/database.rs:806-882` serialize and merge snapshots; `run.rs:52,1236-1275` bounds base/journal batches at 256.

Static evidence establishes possible work, not its production frequency or elapsed-time share. All unmeasured costs, scaling claims, and benefits in the proposals are hypotheses. Existing optimizations are not new proposals: four pooled SFTP content readers, four eligible copy workers, pipelined 32 KiB content requests, incremental snapshot writes, and indexed poll lookups already exist. See [ADR 0043](../adr/0043-parallel-copies.md) and [ADR 0046](../adr/0046-snapshot-archive-sftp-performance.md). Their historical measurements are not measurements of this study.

### Measurements performed

Host: Apple M4 Max, 14 cores, 36 GiB, macOS 27.0, Rust 1.99.0, optimized benchmark profile. The host was shared, with load averages roughly 31-47 during setup and execution. These short warm-cache microbenchmarks are descriptive, not release qualification or million-file extrapolations.

```sh
# PATH needs the installed rustup shims on this host.
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export CARGO_TARGET_DIR="$PWD/build/cargo"
cargo bench --locked -p foldover-engine --bench plan -- --warm-up-time 1 --measurement-time 3
cargo bench --locked -p foldover-state --bench db -- --warm-up-time 1 --measurement-time 3
cargo test --locked -p foldover-net sftp:: -- --nocapture
cargo test --locked -p foldover-endpoint sftp:: -- --nocapture
```

The target-directory override avoids this checkout's broken `target` symlink; it does not change source or compiler optimization. These were individual bench runs, not a passing `cargo xtask bench-check` run. That command also includes scan/hash/archive benches and reads estimates from `target/criterion` directly (`xtask/src/bench.rs:14-20,58`).

| Measured benchmark | Criterion median | Scope limitation |
|---|---:|---|
| `plan_100k_entries` | 57.742 ms | Pure exact-name candidate planning, not full prepare or SFTP |
| `db_commit_100k` | 103.107 ms | One 100k-row steady-state transaction, not actual 256-row journal batches |
| `db_read_100k` | 38.015 ms | Base loading only |
| `db_snapshot_unchanged_100k` | 23.331 ms | Existing incremental replacement, warm unchanged rows |
| `db_snapshot_rewrite_100k` | 155.367 ms | Every fingerprint changes |

Medians were read from `build/cargo/criterion/<id>/new/estimates.json`, `median.point_estimate / 1_000_000`. Fixture sizes and timing boundaries are in `crates/foldover-engine/benches/plan.rs:56-131` and `crates/foldover-state/benches/db.rs:80-183`. Both test commands passed: 17 net tests and 44 endpoint tests. `/usr/libexec/sftp-server` was present; the real-server cases did not report being skipped. These tests establish baseline behavior, not performance gains.

**Measured protocol counts:** a temporary optimized Rust probe linked the current `foldover-net` library, opened each fixture through `Session::spawn`, called `open_read` → `read_all` → `close`, and checked the returned byte count. A Python framed-packet relay forwarded traffic to the local OpenSSH `sftp-server` and counted client packet type 5 (`READ`) between each `OPEN` and `CLOSE`.

| File bytes | READ requests | Returned data bytes |
|---:|---:|---:|
| 0 | 16 | 0 |
| 1,024 | 32 | 1,024 |
| 32,768 | 16 | 32,768 |
| 32,769 | 32 | 32,769 |
| 524,288 | 32 | 524,288 |

This measures actual current-client packet counts, not SSH encryption, network RTT, upload time, or complete-run speed. The temporary probe is not a committed benchmark; proposal 1 specifies permanent regression coverage to reproduce the counts.

## Proposals

### 1. Bound SFTP reads to the observed file size

- **Title:** Bound SFTP reads to the observed file size
- **Category:** I/O and batching
- **Evidence:** Measured counts above. `crates/foldover-net/src/sftp.rs:31-35,339-412`, `Session::read_from`, sends 16 requests before consuming replies. A short DATA reply sets `short`; subsequent replies in that window are ignored, including EOF, so another complete window can be sent. `crates/foldover-endpoint/src/sftp.rs:750-803`, `read_range_on`, already has the expected size, enforces exact length, hashes data, and checks the trailing stat, but does not pass the size to the transport.
- **Hypothesis:** Most tiny documents pay for many unnecessary remote reads, status packets, allocations, and server syscalls. Eliminating speculative beyond-EOF requests reduces comparison cost on every SFTP run, including no-op runs. The packet-count defect is measured; its end-to-end share is unverified.
- **Proposed change:** Add a size-bounded transport read API for known-length files. Limit in-flight offsets to the remaining observed bytes; request only the remaining length in the last chunk. Reissue short replies from the first unconsumed offset without gaps. Retain an EOF/growth probe at the expected end, exact byte-count checks, trailing stat verification, cancellation, request-ID validation, and draining of every outstanding reply on failure. Keep the unknown-length API for callers without a reliable bound. Do not disable hashing or weaken growth detection.
- **Expected impact:** Hypothesis: 15-40% lower total elapsed time on tiny-file, comparison-dominated runs; little gain for a byte-bandwidth-bound corpus. A stable 1 KiB read should need about two READs including the terminal probe, versus the measured 32, but that is not a 16× whole-sync speedup. **Confidence: medium** for elapsed time, high for the measured excess requests.
- **Effort and risk:** Medium, roughly 2-4 engineering days across net/endpoint APIs and tests. Risks: short reads, empty files, files changing during a read, resume offsets, reordered replies, and leaving the wire misaligned. Preserve existing failure/deadline behavior.
- **Measurement plan:** Extend the scripted server in `crates/foldover-net/src/sftp/tests.rs:185-265` with a proposed `known_length_read_packet_budget` test for sizes 0, 1 KiB, 32 KiB, 32 KiB+1, and 512 KiB; count READ offsets/lengths and require exact returned bytes. Include short, reordered, growing, truncated, and cancelled reads. After adding it, run `cargo test --locked -p foldover-net known_length_read_packet_budget -- --nocapture`, then both baseline SFTP test commands. In the end-to-end matrix below, compare total time, READs/file, response bytes/file, and server CPU for unchanged 1 KiB and 8 KiB trees. Keep the change only if elapsed time improves without increasing missed-change cases.

### 2. Cache confirmed archive directories within a run

- **Title:** Cache confirmed archive directories within a run
- **Category:** redundant work and caching
- **Evidence:** Static evidence, runtime cost unverified: `crates/foldover-endpoint/src/sftp.rs:1083-1102`, `set_aside_on`, calls `make_directories_on` for every archived file's parent. At `sftp.rs:1606-1640`, that function attempts MKDIR for every component and, on the usual already-exists refusal, performs LSTAT before proceeding. It does not consult a confirmed-directory cache. Replacement uploads reach this path through `place_on`, lines 1034-1072.
- **Hypothesis:** A replacement-heavy backup repeatedly reconstructs the same `.foldover/archive/<run>/...` chain. For F siblings and d existing parent components, the source implies roughly `2*d*(F-1)` repeat requests beyond the first sibling's setup. That request-count formula and its elapsed-time contribution have not been measured.
- **Proposed change:** Keep a synchronized, endpoint/run-scoped set of exact remote directory paths confirmed by successful MKDIR or directory LSTAT. Create each private archive parent once and coordinate concurrent creators. Invalidate affected prefixes on moves/removals and uncertain outcomes. If a later archive operation reports a missing parent, invalidate and retry directory creation once; propagate other errors. Never use a name-only listing entry as proof that a path is a directory, and never cache across runs.
- **Expected impact:** Hypothesis: 10-30% lower total time for small-file overwrite/archive-heavy runs with reused parents; near zero on first-copy or no-op runs. **Confidence: medium**, conditional on replacement fraction and path depth.
- **Effort and risk:** Medium, roughly 2-4 days. Scope is SFTP namespace management. External clients can remove/replace cached parents; failure recovery, Unicode spelling, exclusive target creation, and archive retention must remain correct. This cache must not replace destination-version checks or root/lease checks.
- **Measurement plan:** Use the proposed end-to-end matrix with 100k then 1m files, 100 siblings per folder, 100% replacements, archive depth 2/8/16. Count MKDIR and LSTAT by path prefix; setup should approach once per distinct parent instead of once per replacement. Compare total and apply time, and inject parent removal and connection loss between siblings. Run `cargo test --locked -p foldover-endpoint sftp:: -- --nocapture` and `cargo test --locked -p foldover-engine sftp_folders_sync_like_local_ones -- --nocapture`.

### 3. Parallelize the SFTP directory inventory

- **Title:** Parallelize the SFTP directory inventory
- **Category:** concurrency and parallelism
- **Evidence:** Static evidence, runtime cost unverified: `crates/foldover-endpoint/src/sftp.rs:494-544`, `observe_reporting`, pops one directory and completes `read_dir` before visiting another. `crates/foldover-net/src/sftp.rs:267-301` serializes OPENDIR, directory batches, and CLOSE within that call. The pool is already available (`sftp.rs:67-70,202-224`); concurrent content checking begins only after directory settlement, at lines 545-621.
- **Hypothesis:** Many small folders leave the existing pool mostly idle during inventory, paying independent folder RTTs serially. Bounded overlap across independent folders can reduce this phase without changing how content is verified. A single giant folder will not benefit from cross-directory concurrency.
- **Proposed change:** Dispatch discovered directories through a bounded coordinator-owned queue, borrowing at most the existing four reader sessions. Workers return listings or errors; only the coordinator updates the observation, progress, and cancellation state. Finish the entire inventory and settle directories before starting file checks, preserving the current progress contract and unavailable-subtree protection. Do not simply raise the global session limit or combine this with simultaneous destructive work.
- **Expected impact:** Hypothesis: 10-25% lower total time for directory-heavy trees if listing consumes at least 20-40% of the run; up to roughly 2-4× within that phase. Near zero for a flat directory or a bandwidth-saturated server. **Confidence: low-medium** until directory shape and phase shares are measured.
- **Effort and risk:** Medium-large, roughly 4-7 days. Risks include unbounded queued paths, progress ordering, server connection limits, directory disappearance, and cancellation while workers wait for sessions. Keep the existing one-session fallback.
- **Measurement plan:** Compare flat, 10k-folder, and depth-16 matrix shapes at fixed entry count, with 0/1/5 ms injected response delay and a real LAN server. Record maximum concurrent listings, session count, queue high-water mark, inventory time, and total time. Run `cargo test --locked -p foldover-endpoint the_whole_tree_is_listed_before_any_entry_is_checked -- --nocapture` plus the full endpoint SFTP filter; add serial-versus-parallel observation equality tests with directory errors and cancellation.

### 4. Index cached SFTP folder names

- **Title:** Index cached SFTP folder names
- **Category:** data structures
- **Evidence:** Static evidence, runtime cost unverified: `crates/foldover-endpoint/src/sftp/spelling.rs:35-43` stores `FolderView::Cached(Arc<Vec<Vec<u8>>>)`. `listed_on`, lines 123-147, scans exact names and may normalize every candidate. `note_added`, lines 206-233, linearly checks membership; `note_removed`, lines 237-253, linearly retains names. These operations run under the listings mutex, and `Arc::make_mut` may copy a whole listing if another reader holds it. A folder is cached only when the spelling path requires a listing; this is not a claim that every ASCII-only folder has this cost.
- **Hypothesis:** Once a wide folder is cached, repeated temporary additions, removals, and replacements can incur O(files × folder width) CPU work, serializing workers. Unicode-heavy folders can additionally repeat normalization. This could dominate on very wide folders even on a fast LAN; no allocator or lock profile was captured.
- **Proposed change:** Replace the name vector's linear membership/mutation path with an exact-byte name index plus an NFC-to-stored-name index. Preserve exact-spelling precedence and deterministic handling when several raw names normalize alike. Use short per-folder state locking for incremental updates rather than copying a full shared vector on each mutation. Release locks before remote I/O and preserve Fetching/Dirty invalidation for in-flight listings and uncertain mutations; retain bounded cache ownership.
- **Expected impact:** Hypothesis: 10-40% lower total time on wide, cached, Unicode/replacement-heavy directories; often below 1-5% on ordinary shallow ASCII trees. **Confidence: low-medium** for total time, with strong source support for the linear scans.
- **Effort and risk:** Medium, roughly 3-5 days. Blast radius is stored-name resolution and cache mutation. Ambiguous aliases, lost rename responses, concurrent readers, and memory overhead are the main risks.
- **Measurement plan:** Add an isolated folder-index benchmark at widths 1k/10k/100k and a replacement run over those widths, including NFC/NFD aliases and one respellable name that activates caching in a mostly ASCII folder. Count comparisons, normalization calls, copied listing bytes, and lock wait time. Require subquadratic total mutation CPU while preserving results. Run `cargo test --locked -p foldover-endpoint sftp:: -- --nocapture`, especially existing cached-listing invalidation and spelling tests.

### 5. Plan against borrowed observation versions

- **Title:** Plan against borrowed observation versions
- **Category:** allocation and memory layout
- **Evidence:** Static evidence, allocation impact unverified: `crates/foldover-endpoint/src/local.rs:583-590`, `Observation::versions`, clones every path into a new BTreeMap. `crates/foldover-engine/src/run.rs:869-876` builds both maps solely to call `plan_resolved`. That planner already merges ordered iterators (`crates/foldover-engine/src/plan.rs:526-552`), but clones the selected path at line 542 and again into a row at line 562. The measured 57.742 ms bench passes prebuilt version maps, so it excludes the observation projection cost.
- **Hypothesis:** At millions of entries, temporary trees and duplicate path ownership increase allocation, peak RSS, and cache misses before copying begins. Under memory pressure they may cause paging. Their actual memory and elapsed-time contribution has not been measured.
- **Proposed change:** Introduce a narrowly scoped borrowed version view over `Observation`, supporting ordered iteration and lookup without cloning paths. Let planning consume that view for observations while retaining the existing BTreeMap-facing API for other callers. Move each selected owned path into its final row after necessary directory indexing instead of cloning it a second time. Preserve raw-byte ordering and validation; do not redesign all tree storage or weaken complete-observation requirements.
- **Expected impact:** Hypothesis: 20-40% less transient memory in the affected planning-input phase, not necessarily total agent RSS; 1-8% lower total elapsed time in memory-heavy million-entry runs, potentially more only if paging is demonstrated. **Confidence: low** without allocation/RSS profiles.
- **Effort and risk:** Medium-large, roughly 4-7 days. Public planner compatibility, borrow lifetimes, tree validation, move pairing, and name-clash logic are affected. Keep the adapter private unless a concrete external consumer needs it.
- **Measurement plan:** Retain `cargo bench --locked -p foldover-engine --bench plan -- --warm-up-time 1 --measurement-time 3` as a core-planner control. Add a proposed observation-to-plan benchmark including `versions()`/its replacement at 100k/1m/3m entries; collect allocations, allocated bytes, peak RSS, prepare time, and total matrix time. Compare exact plan rows/review tokens and run `cargo test --locked -p foldover-engine`.

### 6. Serialize snapshots once in path order

- **Title:** Serialize snapshots once in path order
- **Category:** redundant work and caching
- **Evidence:** Static evidence, allocation/lock impact unverified: `crates/foldover-engine/src/run.rs:925-941`, `snapshot`, builds an intermediate BTreeMap of references, then clones paths and encodes fingerprints into a Vec. `crates/foldover-state/src/pairs.rs:276-295` copies every path and fingerprint into another Vec and allocates encoded versions. `crates/foldover-db/src/database.rs:829-831` builds and sorts a pointer vector although the engine output is already path ordered. `crates/foldover-service/src/sync.rs:908-916` stores each side before execute, under the engine lock. The existing unchanged snapshot median was 23.331 ms per 100k entries; incremental SQL writes are already implemented.
- **Hypothesis:** Snapshot handoff does unnecessary O(entries) copying and preparation even when no database row changes. Reducing those copies should reduce pre-copy latency and lock occupancy, but does not remove the intentional scan or SQL comparison pass.
- **Proposed change:** Merge the two ordered cache/observation iterators directly, preserving the current duplicate-path precedence. Serialize fingerprints and fixed-size versions once outside the engine lock. Pass borrowed blob views through the state-to-DB boundary instead of cloning path/fingerprint bytes again. Provide a validated sorted-input path to the DB merge while preserving the existing general API for unsorted callers. Keep one revision-checked atomic snapshot transaction and the existing changed-row-only writes; do not split commits or change journal durability.
- **Expected impact:** Hypothesis: 15-35% less snapshot-preparation CPU/allocation and 1-5% lower total elapsed time when state handoff is material; less when network reads dominate. **Confidence: low-medium**, because the measured DB benchmark excludes `PairRun::snapshot` and the full service handoff.
- **Effort and risk:** Medium, roughly 3-5 days across engine/state/DB. Risks include ordering around `/`, excluded-entry retention, duplicate precedence, stale revisions, and borrowing encoded storage for too short a lifetime.
- **Measurement plan:** Run `cargo bench --locked -p foldover-state --bench db -- --warm-up-time 1 --measurement-time 3` and add a full `PairRun::snapshot` → store benchmark at 100k/1m entries, unchanged and 1%/100% changed. Record full-handoff time, peak allocated bytes, engine-lock duration, and SQL rows written; unchanged rows must remain unwritten. Run `cargo test --locked -p foldover-db snapshot` and `cargo test --locked -p foldover-engine snapshot` for semantic controls.

### 7. Borrow path prefixes during ancestor checks

- **Title:** Borrow path prefixes during ancestor checks
- **Category:** algorithmic complexity
- **Evidence:** Static evidence, runtime allocation cost unverified: `crates/foldover-types/src/sync_path.rs:35-40`, `SyncPath::parent`, allocates and copies its prefix. `crates/foldover-engine/src/plan.rs:265-293,577-599,603-623` repeatedly walks owned parents in folder-choice settlement, delete-versus-edit protection, and ancestor blocking. `validate_tree`, lines 714-740, creates another parent per entry. The core three-way merge is already ordered and linear in input count; it should not be replaced with another sort/merge.
- **Hypothesis:** The ancestor passes add depth-dependent prefix copying and tree lookups to every path, especially deep document trees. Borrowed component boundaries and reused ancestor indexes can reduce CPU and allocations while leaving the algorithm's decisions unchanged. The current 100k bench has shallow paths and does not establish deep-tree cost.
- **Proposed change:** Add private borrowed-byte ancestor iteration or equivalent cached component boundaries. Use borrowed-key directory lookups so normal traversal does not allocate `SyncPath`; allocate only for errors or final blocked-by-ancestor output. Reuse parent/directory row indexes across passes where practical. If using a stack, derive it from component ancestry, not raw string adjacency: byte-sorted paths such as `a`, `a-b`, `a/x`, and `a0` are an important trap. Retain cancellation checkpoints and choose the same outer conflict as today.
- **Expected impact:** Hypothesis: 15-35% less planning CPU on depth-16/32 trees, translating to roughly 0-3% lower complete-run time when SFTP dominates. **Confidence: low-medium**; promote this rank only if a CPU profile shows preparation materially contributes to elapsed time.
- **Effort and risk:** Medium, roughly 3-5 days. Incorrect ancestor ordering could expose destructive child operations or change review tokens. Cover missing parents, nested conflicts, folder choices, name aliases, and cancellation before changing the traversal.
- **Measurement plan:** Extend the existing plan bench with equal-count trees at depths 2/16/32 and prefix-order adversaries, at 100k and 1m entries. Record ns/path and parent allocations/path, compare exact candidate rows, then run `cargo test --locked -p foldover-engine plan` and the full engine suite. Use the matrix's total time, not an isolated planner speedup, for acceptance.

### 8. Bound the SFTP startup access probe

- **Title:** Bound the SFTP startup access probe
- **Category:** build and startup configuration
- **Evidence:** Static evidence, startup cost unverified: `crates/foldover-endpoint/src/endpoint.rs:164-167`, `Endpoint::open`, calls `probe_access` immediately after connecting. `crates/foldover-endpoint/src/sftp.rs:261-266` implements that probe with a complete `read_dir(&self.root)` whose returned entries are discarded. `observe_reporting`, lines 494-503, later lists the root again. This proposal concerns endpoint startup I/O, not compiler flags.
- **Hypothesis:** A flat million-document root is enumerated and materialized once merely to establish list permission, then again for the real inventory. A bounded permission probe avoids an unnecessary root-width-dependent startup delay and allocation spike. The full scan/content cost remains.
- **Proposed change:** Add a transport list-access probe that opens the directory, requests a bounded first batch to detect READDIR-specific permission failures, and closes the handle. Treat EOF as successful access to an empty directory; propagate real failures. Keep full enumeration in observation, rather than caching a preflight listing across mutations or reusing it as proof of absence. Do not replace this with LSTAT alone, since opening/statting a directory does not establish listing permission.
- **Expected impact:** Hypothesis: 1-5% lower total time on very wide-root, tiny-file runs, with a potentially much larger reduction in endpoint-opening latency; negligible benefit on small roots. **Confidence: low-medium** until root listing is timed separately.
- **Effort and risk:** Small, roughly 1-2 days in the net/endpoint boundary. Preserve the behavior for empty directories, servers refusing READDIR after OPENDIR, timeouts, and CLOSE errors; do not accidentally leave an open handle or hide denial of access.
- **Measurement plan:** Add a proposed `list_access_probe_is_bounded` scripted-server test and run `cargo test --locked -p foldover-net list_access_probe_is_bounded -- --nocapture` after implementation. Assert bounded READDIR count, successful empty-directory EOF, and visible permission errors. Compare endpoint-open time, READDIR response bytes, peak RSS, and full matrix time on roots of 1k/100k/1m entries, leaving the subsequent inventory unchanged.

## Shared end-to-end measurement specification

This harness is **proposed, not present or executed**. Add an ignored release test named `sftp_end_to_end_matrix` beside the existing injectable-endpoint fixtures in `crates/foldover-engine/src/run/tests.rs:383-403`. Reuse the local OpenSSH server/relay pattern, but use a real temporary Engine/PairStore for state loading, snapshot persistence, and journal commits rather than the fixture's in-memory commit callback. Do not run against user folders. Implement `FOLDOVER_PERF_FILES`, `FOLDOVER_PERF_SIZE`, `FOLDOVER_PERF_SHAPE`, `FOLDOVER_PERF_CASE`, and `FOLDOVER_PERF_DELAY_MS` as fixture controls. An exact future invocation would be:

```sh
FOLDOVER_SFTP_SERVER=/usr/libexec/sftp-server \
FOLDOVER_PERF_FILES=1000000 FOLDOVER_PERF_SIZE=8192 \
FOLDOVER_PERF_SHAPE=folders-100 FOLDOVER_PERF_CASE=replace-all \
FOLDOVER_PERF_DELAY_MS=1 \
cargo test --release --locked -p foldover-engine sftp_end_to_end_matrix -- --ignored --nocapture
```

Run 10k files for harness validation, 100k for iteration, then 1m for qualification. The million-file 8 KiB corpus needs about 7.6 GiB per full copy, plus archive and fixture-reset space. Include flat, 100-files/folder, and depth-16 layouts; 1 KiB and 8 KiB content; first-copy, unchanged, 1%-changed, and replace-all cases. Test standard durability, preserve the configured metadata policy, use no bandwidth cap, and retain identity, lease, deletion-review, and content-change checks. Do not silently switch to an unrecorded backup target to make the comparison faster.

For each revision, restore the same target/base/archive fixture before each mutating trial. Exclude corpus generation/reset from timing, but include state loading, endpoint opening, observation, planning, snapshot handoff, execution, and final commits/maintenance. Record those phases separately as well as total elapsed time. Use at least five paired trials, report individual values plus median and range, and keep cold/warm-cache runs separate. Count SFTP request kinds, payload and protocol bytes in both directions, sessions, queue depths, client/server CPU, and agent peak RSS. Injection of per-response delay is a stress model, not a faithful network RTT measurement.

Finally validate on the intended LAN/NAS using a disposable, reviewed pair. For an installed Mac build, end-user elapsed time can be measured with:

```sh
FOLDOVER=/Applications/Foldover.app/Contents/Library/LaunchServices/foldover
# PAIR_ID must name an isolated benchmark pair with first-run approval completed.
/usr/bin/time -p "$FOLDOVER" sync "$PAIR_ID" --progress
```

Sample the **agent and SSH/server processes**, not just CLI RSS/CPU. Keep client/server versions, storage, network link, pair mode, metadata options, changed fraction, and cache state fixed. Reject a microbenchmark win that regresses complete-run time or any data-integrity test. Estimates across proposals overlap and must not be added together.

## Rejected or requires separate approval

- **Stat-only SFTP hash reuse or destructive-operation verification:** rejected for this ranking. SFTP attributes have no stable file ID and whole-second times. `crates/foldover-endpoint/src/sftp/tests.rs:730-784`, `unchanged_sftp_attributes_cannot_hide_a_content_change`, deliberately tests content changes hidden behind unchanged attributes. A local racy-window heuristic is not equivalent proof for a remote server. Keep content revalidation unless a separately approved protocol/change-journal design supplies equivalent guarantees; a faster but different safety policy is not a transparent optimization.
- **Blindly increasing connections/chunks or relaxing root checks:** not ranked without a bottleneck measurement. Four workers already exist, copy concurrency is the minimum of both endpoints (`run.rs:1430-1442`), and local spinning disks can legitimately cap it at one. Larger transfer windows do not fix tiny-file EOF overreads. The session pool's refusal handling is already implemented. Preserve root identity checks and 256-entry crash-recovery batching.
- **Mandatory remote helper, tar/rsync transport, disabled archives, or weaker durability:** outside this proposal set. These change deployment/behavior rather than optimize standard SFTP with the current safety contract.
- **Release LTO/codegen tuning:** the manifest has no custom release profile and `scripts/build.sh:150-152` already uses `--release`. Additional CPU tuning is a hypothesis, not a demonstrated SFTP bottleneck. Revisit only after a production CPU profile; do not give it a network-throughput guarantee.

## Open questions and unverified items

- Actual file-size distribution, root width, folder depth, changed fraction, sync mode, and retention/archive volume are unknown. These determine whether ranks 2-4 apply; ranks are intentionally conditional, not universal.
- No million-file run, real SSH/LAN/NAS run, profiler capture, allocation trace, or agent RSS/lock profile was collected. SSH/server CPU, storage IOPS, server request limits, observed RTT, and metadata policy remain unverified.
- The local packet probe used a direct server subprocess through a relay, not SSH. Its scripts/logs were temporary, not added as permanent tests. Reproduce its counts in the proposed transport regression test before implementation.
- Only the two listed benchmarks and SFTP-filtered test suites were executed for this study. Full workspace CI, scan/hash/archive budgets, device-matrix testing, and release qualification are separate checks; no claim is made that these were run locally.
- The current plan benchmark omits observation-to-version projection and most name-policy/prepare work; the DB bench omits full service lock duration and real journal-batch sizes. Use them as controls, not proof of complete-run performance.
- The safest first implementation is proposal 1: it has measured protocol waste, broad comparison-path coverage, a bounded acceptance test, and no need to weaken file-change detection. Its end-to-end benefit still requires the LAN measurement above.
