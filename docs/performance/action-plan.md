# Performance action plan

Reviewed against `2301115` on 2026-11-07. This is an implementation backlog;
no engine behavior or performance budgets change with this document.
Scope: eleven reviews on main plus the two review documents in PRs 255 and
256, pinned to the revisions linked in the source routing table.

## Recommendation

Implement the excess SFTP read-request and repeated journal-scan fixes first.
Then reduce write-path round trips, local metadata work, pathological path
searches and whole-tree copies. Measure concurrency and build-profile changes
before choosing new defaults. Keep content verification, durable commits,
root checks and preview approval binding intact.

The reviews identify real work in the hot paths, but their estimated speedups
cannot be added together. Most timings came from temporary harnesses on a
busy Mac, sometimes through a local pipe rather than SSH over a network.
They establish leads, not a million-file LAN baseline. Repeated recommendations
are not independent measurements. No new end-to-end performance run was made
for this consolidation.

## Evidence and corrections

- **Excess SFTP reads are present.** `Session::read_from` sends sixteen
  32 KiB requests before collecting replies. A short reply causes subsequent
  replies in that window to be discarded and another window to be sent.
  The packet probes in [per-file cost budget](per-file-cost-budget.md) and
  [pipeline review](sftp-lan-million-doc-pipeline.md) report 16 READs for an
  empty file and 32 for a 1 KiB file. The code matches that explanation.
  Packet reduction is not the same factor as wall-clock improvement because
  requests already overlap.
- **The journal cleanup scans accumulated rows.**
  `database/journal.rs::apply` runs `DELETE ... pair_id=? AND run!=?`
  for every nonempty recording batch. A fresh local SQLite query-plan check
  using the same primary key confirms a seek on `pair_id` only; separate
  less-than/greater-than run ranges can seek on both columns. The engine/state review measured quadratic
  growth in an isolated harness; its million-row minutes are extrapolations.
  The existing `db_commit_100k` bench does not model repeated journal batches.
- **Stat reuse is not equivalent to current SFTP verification.**
  `observe_entry` hashes before reusing an observation, and `verified_at_on`
  hashes before destructive changes. Coarse mtime plus size, even with a
  racy-time exclusion and periodic rehash, cannot detect all edits preserving
  those fields. Local fingerprint rules do not prove the same guarantee on
  SFTP. The stat-trust proposals are excluded from the accepted backlog.
- **Exclusive staging does not prove final-path absence.** `write_new`
  exclusively creates a temporary beside the destination. It cannot replace
  `receiving_on`'s check of the actual destination. Likewise, a successful
  `SETSTAT`/`FSETSTAT` status reply does not contain final stored attributes.
- **Raw sorted paths require care.** `a`, `a-b`, `a.txt`, `a/x`, `a0`
  sort in that order. Popping an ancestor at the first non-descendant would
  lose `a` before reaching `a/x`. Use component-prefix indexes, explicit
  `a/` ranges, or a proven traversal order. This corrects the simple stack
  proposals for skips, failures and parent validation.
- **Some suggested fixes already exist.** Local `UnavailablePaths` already
  uses a prefix set. Snapshot persistence already writes only differences,
  and SFTP file observation already uses four pooled readers
  ([ADR 0046](../adr/0046-snapshot-archive-sftp-performance.md)). Remaining
  opportunities concern SFTP prefix searches, upstream snapshot allocation
  and directory listing, not reimplementing those completed optimizations.
- **Pool capacity is not effective copy concurrency.** `PairRun::workers`
  takes the smaller endpoint limit; local SSDs currently advertise four.
  Raising SFTP's constant alone cannot give local-to-SFTP runs eight workers.
  Account for run-thread and lease connections too. Separate `ssh` processes
  and multiplexed SSH channels have different server limits.

## A0: Establish reproducible baselines

Do this first, adding each focused regression harness with its owning change.
Keep deterministic packet/operation assertions in normal tests and wall-clock
qualification on idle hosts or dedicated benchmark runs.

1. Add a permanent framed SFTP relay/scripted-server counter in `foldover-net`
   tests: request types, returned bytes, outstanding requests and injected
   latency. Reuse the existing spawned `sftp-server` and wire test machinery.
   Include at least one real SSH-over-LAN run; a local subprocess omits SSH
   startup, authentication, encryption and server connection limits.
2. Measure prepare phases (input load, each observation, alignment, planning,
   preview and snapshot persistence), apply phases and total run time. Record
   DB commit time, engine-lock hold/wait time, peak agent RSS, packet counts,
   bytes transferred, CPU, active sessions and cancellation latency.
   Diagnostics belong in drivers/services, not user-facing screens.
3. Use a 10k development corpus, 100k comparison corpus and a 1M qualification
   corpus. Cover first copy, no-change, 1% edits, replacement/deletion-heavy,
   denied-subtree/skip-heavy and Unicode/deep-name runs. Use 1/8/32 KiB
   documents plus a separate large-file corpus; flat and nested trees;
   local-to-SFTP and SFTP-to-local directions. Add SFTP-to-SFTP only for the
   features that affect it.
4. Run at least three paired before/after samples on the same idle host and
   corpus. Record commit, machine, filesystem, server, network RTT, warm/cold
   cache conditions, workload and range as well as median. Preserve scripts
   and raw results so another run can reproduce the claim.

**Gate:** establish baselines before tuning defaults. PLAN section 5.3's
500 MB agent limit at 1M tracked files, 10k-entry/s local cold scan, planning
and large-file transfer targets remain the qualification criteria. The local
FSEvents no-change target is not an SFTP stat-trust target. Keep existing
[Criterion budgets](../../perf/budgets.toml); do not loosen them to make an
optimization pass. Investigate the reviews' reported scan-budget misses on
an idle host rather than treating their busy-host samples as qualification.

## Accepted implementation work

Effort bands below are relative: S is one focused change, M is several
coordinated changes, and L needs an API or ownership design. They exclude
unpredictable hardware qualification time. Each item should be its own PR,
or split into the explicitly listed substeps.

### A1: Bound SFTP reads without skipping content verification

**Priority:** first wave. **Effort:** M. **Depends on:** A0 packet harness.

Owners: `foldover-net/src/sftp.rs::read_from` and
`foldover-endpoint/src/sftp.rs::{read_range_on,read_small_on,probe_access}`.

- Add an exact-length read operation for files whose expected size is known.
  Queue only useful chunks, handle short DATA from the first unconsumed
  offset, and retain an end probe so extra bytes are detected. Preserve the
  full digest, exact byte-count and post-read stat checks.
- Give small control-file reads a separate maximum-length contract with
  bounded speculation and an overflow probe. A limit is not an exact size;
  do not silently accept a truncated identity or ignore file. Retain the
  unknown-length API for callers that need it.
- Separately bound the startup access probe to enough READDIR work to prove
  listing access, then CLOSE; do not materialize an entire flat root twice.

**Acceptance:** correct bytes and digest for 0, 1, 1,024, 32,768, 32,769 and
524,288 bytes and resumed offsets. An ordinary sub-chunk exact read should
need one data request plus at most one end probe, not 16 or 32 READs. Test
growth, truncation, short/reordered replies, zero progress, cancellation,
timeouts and sink failure. All outstanding replies must be drained or the
session discarded before reuse. Re-run endpoint change-detection tests and
compare no-change SFTP observe time and packet counts.

### A2: Remove repeated journal scans, then empty resolution deletes

**Priority:** first wave, independent of A1. **Effort:** S–M.

Owners: `foldover-db/src/database/journal.rs::apply`,
`database.rs::change_base`, and the engine/state commit boundary.

- Inside the same revision-checked transaction, replace `run!=current`
  with separate `run<current` and `run>current` deletes, each constrained
  by `pair_id`. Both use primary-key ranges and together preserve cleanup
  semantics without new first-batch state. This is the simplest remedy from
  PR 256; confirm the plans with Rust's bundled SQLite too. An explicit
  retry-safe begin/append mode is a fallback if the measured result warrants
  a larger API change. Do not infer first batch from plan-row count.
- Preserve the previous undo journal until the first nonempty recording
  transaction commits. A no-write run must keep it. Preserve base/journal
  atomicity and `synchronous=FULL`.
- In a separate small change, check for resolutions once per transaction
  before issuing per-path deletes; ensure actual choices are still spent.

**Acceptance:** add `db_commit_batches_100k` exercising real base and journal
updates in batches of 256, with an old run present, plus 200k/400k scaling
runs. Check query plans and near-linear operation growth; one large base
transaction is insufficient. Cover continuation, empty first batch, rollback,
retry, stale revision, undo spending and replacement of the old run. Verify
lexically older/newer run IDs and other-pair isolation. Verify mixed
present/absent resolution paths as well as the empty-table fast path.

### A3: Shorten SFTP write chains while preserving final-state checks

**Priority:** second wave. **Effort:** M. **Depends on:** A1 measurements.

Owners: `sftp/transfer.rs::send_on`, `sftp.rs::{set_metadata_on,
staged_version_on,make_directories_on,place_on}`, `sftp/spelling.rs`.

1. Return authoritative attributes from metadata handling when it already
   read them and made no change; reuse those for staged version calculation.
   After a metadata mutation, still obtain the server's final stored state.
   Evaluate handle-based metadata operations separately; extend the wire API
   only if measured savings justify it and server compatibility is tested.
2. Add bounded, operation-scoped reuse of confirmed archive parents and
   spelling lookups. Cached listings are hints, not proof of continued
   existence or path identity. Respect dirty/evicted entries, invalidate on
   failure, and retry the safe directory walk where appropriate.
3. If wide cached folders show material CPU/lock cost, index exact names and
   NFC candidate names instead of repeatedly scanning/copying the whole
   cached vector in `listed_on` and `note_*`. Keep exact-spelling preference,
   ambiguous-candidate ordering, invalid UTF-8 and Fetching/Dirty semantics;
   bound cache bytes as well as folder count, with no lock held over I/O.

**Acceptance:** reduce counted requests on explicitly identified create and
replace paths; do not promise five requests universally. Preserve destination
checks, content-not-stored errors, final stored versions, metadata rounding,
executable-bit behavior, temp cleanup and cancellation. Test concurrent
destination creation/edit, removed or substituted parents, cache eviction,
NFC/NFD collisions and changed permissions. Keep verification rereads for
replacement, move and archive.
For the optional spelling index, compare 1k/10k/100k-name cached folders,
including mostly ASCII folders whose cache was activated by a Unicode name;
require equivalent lookup results and subquadratic mutation CPU.

### A4: Make local observation metadata work proportional to policy

**Priority:** second wave, independent of A3. **Effort:** M, then M–L.

Owners: `local.rs::entry_version`, `local/version.rs`,
`foldover-fs/src/root/{extended,content}.rs`.

1. Add an explicit observation capture mode selecting only version-relevant
   values and ACLs. Preserve resource forks as content, executable bits,
   quarantine rules, limits and detection of relevant concurrent changes.
   Keep full capture available for copy/archive/restore. Do not remove the
   final name-list check merely because the initial xattr list was empty:
   a relevant attribute can appear during the read.
2. After that comparison is green, fuse content hashing and extended metadata
   capture on one verified descriptor. Keep all existing identity, residency,
   materialization, cancellation and before/after consistency guarantees.

**Acceptance:** identical entry versions for the full policy matrix and
resource-fork, ACL, quarantine, system-attribute and dataless fixtures.
Inject metadata/content edits during capture. Count opens/stats/xattr calls;
improve cold observe on both plain and xattr-rich corpora without regressing
warm reuse. Native macOS driver tests are required, alongside Linux checks.

### A5: Remove pathological path searches and repeated name work

**Priority:** second wave; elevate for failure-heavy workloads. **Effort:** M.

Owners: `sftp.rs::unavailable`, `run.rs::{below_failure,align_spellings}`
and `plan.rs::{skip_branches,name_key,plan_resolved}`.

- Reuse the local component-prefix index design for SFTP unavailable roots
  and executor failures. Update it as new failures arrive. Apply the same
  boundary-safe approach to skipped subtrees; preserve forward/reverse rules.
- Add an ASCII borrowing fast path for name matching; lowercase only where
  required. Reuse normalized keys where measured, keeping distinct Unicode
  and case-matching policies distinct. Avoid a permanent full-tree key cache
  that cancels the memory savings.
- Separately use a sorted merge for exact membership in `align_spellings`.
  Preserve ambiguous composed-name handling and unavailable entries.
- Optimize parent/blocked-ancestor walks only after profiling. Use borrowed
  component prefixes or explicit parent indexes; preserve tree-role error
  precedence and outermost blocking semantics.

**Acceptance:** decision equivalence against existing planner properties and
simulation; fixtures containing `a`, `a-b`, `a.txt`, `a/x`, `a0`, non-UTF-8
paths, case clashes and composed/decomposed aliases. Add 100k/1M path benches
with thousands of skipped/denied roots, plus deep and Unicode variants.
Verify scaling and allocation counts, not busy-runner timing assertions.

### A6: Reduce full-tree copies and shorten avoidable lock holds

**Priority:** second wave; elevate if A0 exceeds 500 MB. **Effort:** M–L.

Owners: `Observation::versions`, `PairRun::{snapshot,preview,prepare_on}`,
`plan.rs::plan_resolved`, `foldover-service/src/sync.rs::perform`,
`foldover-state/src/pairs.rs` and `Engine::run_inputs`.

Split into small PRs in this order:

1. Lazily build the service's preview when needed for a held review, supplied
   token/approval or a response requesting approval. Preserve ADR 0013's
   exact fresh-plan binding. Test the complete held/token/approval matrix.
2. Compare the effective snapshot (observation plus excluded-entry cache)
   with the previous snapshot before encoding. Skip identical stores.
   Preserve the existing overlay precedence, additions/removals, spelling
   changes and revision checks; then stream/borrow changed snapshot rows to
   avoid successive full vectors and an intermediate map.
3. Let planning consume borrowed version views instead of cloning both
   `BTreeMap`s. Keep the abstraction within the existing layer graph.
4. Measure residual `run_inputs` load/decode under the engine mutex. If it
   blocks other pairs/status requests materially, copy the minimal consistent
   raw inputs under the single-writer boundary and decode outside it.
   Overlap base loading only with a coherent base/snapshot/revision design;
   starting another thread that holds the same lock does not solve blocking.

**Acceptance:** identical previews, plans, snapshots and approval decisions;
one changed snapshot entry still persists. Count preview/store calls and
allocations and measure peak RSS across prepare, preview and apply at 1M
tracked files, not just planner microbenchmarks. Exercise simultaneous status
requests, pair reconfiguration, stale revisions and other-pair activity.

## Experiments after the accepted fixes

Promote an experiment only when A0 shows its phase is material and the
benefit exceeds run-to-run variation on the affected workload. Avoid adding
pair settings until a real need for user control is established.

| ID | Experiment and implementation boundary | Acceptance and reason for sequencing |
|---|---|---|
| X1 | Overlap left/right observations, then bounded local hashing and cross-directory SFTP listing; tune effective workers from 4 toward 8 | Separate PRs; preserve single-thread recording/control, deterministic errors, cancellation, backpressure, FD limits and lease progress. Reuse ADR 0043/0046 patterns. Test low-capacity/refusing servers and shared HDD/SSD resources. Use the smaller endpoint limit and total per-run connection cost. Overlap can save up to the smaller scan duration when resources are independent; it can also increase contention and peak RSS. |
| X2 | Pipeline READDIR, batch request writes, pipeline CLOSE with trailing LSTAT, then tune large-file chunk/window and `copy_data` | Only pursue the substep whose measured cost remains. Cover EOF/drain/reordering, short I/O, nonblocking deadlines, flush-before-wait and close-error behavior. Keep bounded requests/memory; do not retry an oversized mutating write blindly on generic FAILURE. Retain a compatible small-packet path. Small documents will gain little from bulk-window tuning after A1. |
| X3 | Compare release defaults with thin LTO and fewer codegen units | Measure end-to-end CPU work, existing benches, binary size and clean build/link cost. Validate both agent/CLI and FFI/XCFramework builds. Keep unwinding; adopt only a repeatable gain worth its build cost. |
| X4 | Parallelize independent file/link archives | Update ADR 0043 first. Preserve all destination content verification, copy-before-archive and child-before-folder barriers; only the run thread records base/journal results. Test partial failure, cancellation, panic and undo. Replacement's archive step is already inside a copy worker; the separate Archive pass primarily benefits deletion-heavy runs. |

## Excluded and deferred proposals

| Proposal family | Decision |
|---|---|
| Reuse SFTP versions based only on stat; use stat-only destructive verification | Exclude from this plan. Periodic rehash bounds delay, not missed-edit risk. A future explicit product decision needs a separate ADR and user-visible semantics. A trustworthy server digest could preserve content comparison, but extension support and freshness must be proved separately. |
| Hash creates only during copy; resume SFTP partial writes; parallel SFTP-to-SFTP copies | Defer separate designs: these change preview/source verification or ADR 0034/0043 safety assumptions. A one-way pair still needs proof of acyclic session acquisition under refusal and cancellation, not just a raised worker constant. |
| Larger commit batches, less frequent root checks, SQLite NORMAL, panic abort | Keep current defaults. A2 and A1 first remove cost without enlarging crash redo, delaying root-loss detection, weakening durability or bypassing worker panic handling. Revisit batch/check policy only with measured residual cost and an explicit recovery/identity contract. |
| Post-write snapshots | Defer until A6: local fingerprint reuse may benefit if a write supplies authoritative post-state; SFTP must still reread content. Do not claim this eliminates next-run SFTP downloads. Cover partial runs, external edits and paths lacking final fingerprints before adopting. |
| Snapshot temp-table rewrite | Benchmark only if persistence remains material after A6. Existing diff writes are already efficient for unchanged rows; staging every incoming row adds its own O(N) writes and memory. No guaranteed bulk-SQL win. |
| Sorted-vector base, arena/streaming trees | Defer until A6's measured RSS remains over budget. These are broad representation changes with ordering/API costs. Consider one representation at a time, not a combined rewrite. |
| Wire buffer reuse, compact fingerprints, reader-result batching, ring-buffer request IDs, per-session multi-file pipelines, SSH multiplexing | Defer micro-optimizations until packet reduction and allocation profiles locate a bottleneck. Bound queues before batching. Preserve duplicate/unknown-ID detection, wraparound handling and session health. A modulo ring alone cannot distinguish colliding outstanding IDs. Multiplexing requires separate host-key/authentication, control-socket lifecycle and server-limit validation. |

## Delivery and stopping gates

1. Land A0 harnesses with A1 and A2 in parallel lanes. Publish baseline and
   after-results. Re-rank subsequent work using phase time and peak RSS.
2. Run A3 (wire/write), A4 (local driver) and A5 (path algorithms) independently
   where files do not overlap. Start A6 after the baseline; prioritize it
   immediately if the agent breaches the memory target.
3. Re-profile. Promote only justified X experiments. Run 100k comparisons
   before 1M qualification on representative NAS/SSH and local hardware.
4. Each code fix needs a failing regression before the fix, affected crate
   tests/lint, then `cargo xtask ci` and the applicable macOS native checks.
   Changes to driver consistency, concurrency, journaling or recovery also
   need simulation/fault-injection coverage and device-matrix checks for the
   affected endpoints. Re-run `cargo xtask bench-check` for affected budgets;
   register new benchmark IDs so they are actually enforced.
5. Ship each slice through PR, current-commit CI and review. Record measured
   outcomes in `perf/` or an ADR, including neutral/rejected experiments.
   Stop tuning a phase once it meets its target and is no longer material;
   reject a change if its apparent gain is noise or a correctness check fails.

## Source recommendation routing

Numbers refer to the numbered proposals in each original review. This table
keeps every review traceable without carrying forward its original ranking.
Items in the excluded/deferred table remain outside implementation scope.

| Review | Routing |
|---|---|
| [LAN end-to-end](lan-sftp-end-to-end-sync.md) | 1, 3 excluded stat-trust; 2 → X1; 4–5 → X2; 6 → A3; 7 deferred root policy; 8 → X3 |
| [Million-file hot paths](sftp-million-file-hot-paths.md) | 1 excluded stat-trust; 2 → X1; 3 → A3; 4 → X2; 5 → A1; 6 → X1/X2; 7 → A5; 8 deferred buffer work; 9 → X3 |
| [Run costs](sftp-million-file-run-costs.md) | 1–2, 4, 9 → X1; 3 → A1; 5 → A3; 6 deferred batch/root policy; 7–8 → A6/A5; 10 → X2 |
| [DB and apply](sftp-million-file-db-apply-hot-paths.md) | 1, 5 → A2; 2 → X2; 3 → X4; 4 → A6; 6–7 → A5; 8 → X3 |
| [Local scan and wire gaps](sftp-million-file-local-scan-and-wire-gaps.md) | 1, 4 → A4; 2 → A5; 3 → A1; 5 → A3; 6 deferred SFTP-to-SFTP; 7–8 deferred representation; 9 deferred buffers |
| [LAN pipeline](sftp-lan-million-doc-pipeline.md) | 1, 8 → A1; 2 → X1; 3 → A3; 4 → X2; 5–6 → A5; 7 deferred batching/fingerprints |
| [Engine and state](million-file-engine-state-hot-paths.md) | 1 → A2; 2 deferred post-write snapshot; 3 deferred temp tables; 4–5 → A6; 6 → A5 after profiling; 7 deferred request bookkeeping; 8 → X3 |
| [Per-file cost budget](per-file-cost-budget.md) | 1 → A1; 2 → A3 with destination-check correction; 3–4, 6 → X1; 5 → A4; 7 → A6; 8 → A5 |
| [Sync latency](sync-latency-hot-paths.md) | 1 excluded stat-trust; 2 → A3; 3 deferred lazy hashing; 4–5 → X1; 6 → A6; 7 → A5; 8 deferred batch policy; 9 → X3 |
| [Compare and copy](sftp-compare-and-copy-hot-paths.md) | 1 excluded stat-trust; 2 → A3; 3 deferred multi-file pipelines; 4 → X1; 5 deferred batch/root policy; 6 → A4; 7 → A5; 8 → A6; 9 → X3 |
| [LAN sync](sftp-lan-sync-hot-paths.md) | 1 excluded stat-trust; 2 → A3; 3, 5–6 → X1/X2; 4 → A4; 7 → A5; 8 → A5/A6; 9 → X3 |
| [Small-file review, PR 255](https://github.com/L-K-M/Foldover/blob/5b07a09fa399bce4fde0814b42302106bc94854a/docs/performance/lan-sftp-small-file-hot-paths.md) | 1, 8 → A1; 2, 4 → A3; 3 → X1; 5–6 → A6; 7 → A5 |
| [Million-document review, PR 256](https://github.com/L-K-M/Foldover/blob/055e72c157b415058f9cd291c4285179db30b642/docs/performance/million-document-sftp-hot-paths.md) | 1 → A1; 2 → A2 (preferred range-delete remedy); 3, 6 → X1; 4–5 → A6; 7 → A3; 8 → A5 |
