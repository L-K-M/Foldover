# Performance proposals: database and apply-phase costs of million-file SFTP runs

Foldover is a macOS app over a portable Rust workspace (`crates/`) that syncs folder pairs between local disks, shares, SFTP servers and other Macs. A run observes both sides, plans against a SQLite-backed base, then applies copies, metadata updates, moves and archives on a small worker pool while the run thread journals every write and commits base/journal rows in batches of 256 (`COMMIT_BATCH`, `crates/foldover-engine/src/run.rs:52`). The workload studied here is comparing and copying millions of documents to an SFTP server on the local network; the primary metric is end-to-end wall-clock time of a sync/backup run. This document complements the three existing analyses in `docs/performance` by concentrating on the database commit path, the apply phase's serial archive pass, and per-entry CPU costs that those passes did not cover. Proposals are ranked by expected reduction in end-to-end time weighted by effort and risk. The code-structure facts and line numbers were verified by reading the tree at commit `d10bf29`; no Rust toolchain was available in this session, so no benchmark or profile was run, and every impact magnitude below is a hypothesis to be confirmed by the stated measurement plan. The only live numbers cited are ADR 0046's published measurements (5 ms simulated SFTP latency; 100k-entry snapshot, poll, archive and SFTP-read benchmarks).

## Ranked proposals

1. Stop the per-batch journal delete from scanning the run's own journal
2. Pipeline `CLOSE` with the trailing `LSTAT` in SFTP reads
3. Parallelize file and link archives across the copy worker pool
4. Skip snapshot persistence when the new observation equals the stored one
5. Do not spend a `DELETE` per base row on an empty resolution table
6. Make `skip_branches` linear in the number of rows
7. Make Unicode name keys cheap for ASCII paths and avoid per-row allocation
8. Add a release profile with thin LTO and one codegen unit

---

### 1. Stop the per-batch journal delete from scanning the run's own journal

- **Category:** I/O and batching (database)
- **Evidence:** `crates/foldover-db/src/database/journal.rs:84-86` executes `DELETE FROM run_journal WHERE pair_id=?1 AND run!=?2` on every `JournalChange::Record`. `change_base` is called once per `COMMIT_BATCH` (`crates/foldover-engine/src/run.rs:52`, `:1177-1187`, `:1256-1258`, `:1275`), and `crates/foldover-state/src/pairs/journal.rs:85-104` maps every `JournalUpdate::Record` to `JournalChange::Record`. The table's primary key is `(pair_id, run, seq)` (`crates/foldover-db/src/database.rs:203-214`) and there is no index on `run`, so the `pair_id=? AND run!=?` predicate range-scans every journal row of the pair.
- **Hypothesis:** the first non-empty batch of a run deletes the previous run's journal; every later batch re-scans the current run's already-inserted rows to delete nothing. For N writes in batches of 256 the scan cost is O(N²/256); at 1M writes that is on the order of 2 billion row visits, tens of seconds to minutes of pure SQLite work on the run thread while copy workers wait for the engine lock.
- **Proposed change:** in `journal::apply`, before the delete, check whether this run already has journal rows (`SELECT EXISTS(SELECT 1 FROM run_journal WHERE pair_id=?1 AND run=?2)`); only run the `DELETE ... run!=?2` when it does not. The first non-empty batch still removes the previous run's entries; later batches skip the scan. No schema change is needed.
- **Expected impact:** removes the quadratic term; for a 1M-write run, DB scan work drops from ~2 billion row visits to one delete plus one O(log n) existence check per batch. Estimated 5-20% off apply-phase wall time on large change-heavy runs; near-zero on small runs. Confidence: **high** that the delete and scan happen as described (code and schema), **medium** on the wall-clock magnitude (depends on SQLite's plan and disk).
- **Effort and risk:** small; one query plus a branch. Blast radius is journal replacement semantics. The check must not skip the delete when a run's first non-empty batch arrives after base-only batches; the existing `rows.is_empty()` early return (`journal.rs:80-82`) already handles that case. Existing tests in `crates/foldover-db/src/database/journal.rs` cover replacement and spending.
- **Measurement plan:** add a criterion bench `db_commit_batches_100k` that commits 100k base updates in batches of 256 and times the whole loop; compare before/after. Confirm the query plan with `EXPLAIN QUERY PLAN DELETE FROM run_journal WHERE pair_id=?1 AND run!=?2` on a populated database. End-to-end: run a 100k-file sync against the fixture `sftp-server` and compare wall clock.

### 2. Pipeline `CLOSE` with the trailing `LSTAT` in SFTP reads

- **Category:** I/O and batching
- **Evidence:** `crates/foldover-endpoint/src/sftp.rs:785` closes the handle and then `:793-800` issues a separate `lstat` on the same path. Both go through `Session::request` (`crates/foldover-net/src/sftp.rs:689-694`), which sends one request and waits for its reply. The session already supports keeping multiple requests in flight: `send_for`/`receive_for` (`crates/foldover-net/src/sftp.rs:696-767`) and the `pending: VecDeque` window in `read_from` (`:339-415`). Every observed file and every resumed copy read pays this serial close-then-lstat sequence.
- **Hypothesis:** the post-read `lstat` is a change check that does not depend on the close's result; issuing both requests back-to-back removes one full round trip per file read. At 1M files and 4 read workers that is ~250k saved round trips; at the 5 ms simulated latency of ADR 0046 it is on the order of 20 minutes, and at a 0.3 ms LAN round trip it is roughly 75 seconds.
- **Proposed change:** add `Session::close_and_lstat(handle, path)` (or a small `request_pair` helper) that sends `CLOSE` and `LSTAT` with `send_for`, then collects both replies in id order and returns the attributes. Use it in `read_range_on` on the success path. Keep the existing error and drain discipline (`drain`, `crates/foldover-net/src/sftp.rs:782-801`) so a failed close or lstat still leaves the wire aligned, and still check the close result before reporting success.
- **Expected impact:** one fewer round trip per observed or copied file; estimated 5-15% off the SFTP observe phase for small-file-heavy scans and a smaller gain on copies. Confidence: **high** that the two requests are sequential today, **medium** on the wall-clock magnitude.
- **Effort and risk:** small-medium; one new session method plus one call-site change. Risk is error ordering and drain semantics: if `CLOSE` fails, the `LSTAT` reply is still owed and must be consumed or drained. Existing wire tests and `crates/foldover-endpoint/src/sftp/tests.rs` cover close/stat behavior.
- **Measurement plan:** shim `Session::send_for`/`request` to count round trips during `observe_from` over a 10k-file fixture tree; compare requests per file and observe-phase wall time before/after. End-to-end: run `foldover-cli` sync twice against the fixture and compare the second run's observe time.

### 3. Parallelize file and link archives across the copy worker pool

- **Category:** concurrency and parallelism
- **Evidence:** `crates/foldover-engine/src/run.rs:1220-1245` runs the reverse archive pass on the run thread. `Copies::takes` (`crates/foldover-engine/src/run/copies.rs:96-98`) only takes `Propagate` rows, and `Executor::copies` (`crates/foldover-engine/src/run.rs:1698-1705`) rejects everything else. The archive branch of `Executor::apply` (`run.rs:1770-1787`) calls `SftpEndpoint::remove` (`crates/foldover-endpoint/src/sftp.rs:1166-1208`), which verifies the entry and then renames it into the archive. ADR 0043 explicitly keeps archives on the run thread.
- **Hypothesis:** in a replace- or delete-heavy run, every archived file is a serial chain on the run thread while the copy workers are idle during the reverse pass. Parallelizing file and link archives (not folder removals, which depend on their children being gone) across the same worker pool removes that serialization; the run thread keeps journaling and base recording, as it already does for copies.
- **Proposed change:** extend the `Copies` worker pool to accept `EntryDecision::Archive { on }` rows whose entry is a file or symlink. The worker performs the endpoint `remove` and returns the result; the run thread records the journal and base update in `landed`, exactly as for copies. Keep directory removals on the run thread and call `copies.finish` before each, so a folder is never removed while a child archive is still outstanding. Do not parallelize `Move` rows yet: a move can be followed by a metadata update that depends on it.
- **Expected impact:** for a run replacing or deleting N files, archive-phase wall time falls by up to the worker count; estimated 10-40% off apply-phase time on replace-heavy million-file runs, near-zero on a first sync (no archives). Confidence: **high** that archives are serial today, **medium** on the magnitude (depends on the archive fraction).
- **Effort and risk:** medium; it reuses the existing `Copies` channel and worker machinery, but the archive branch's journaling and failure handling must stay on the run thread. Risks: a worker panic mid-archive must stop the run as today; folder-removal ordering must be preserved; `verified_at_on` failures must still surface as `DestinationChanged`. Existing run and SFTP tests cover archive semantics.
- **Measurement plan:** run a 10k-file replace workload against the fixture `sftp-server` with 1 and with 4/8 workers; measure archive-phase wall time and requests per file. Confirm the folder-removal and archive-ordering tests in `crates/foldover-engine/src/run/tests.rs` still pass.

### 4. Skip snapshot persistence when the new observation equals the stored one

- **Category:** redundant work and caching
- **Evidence:** `crates/foldover-service/src/sync.rs:909-915` calls `run.snapshot(side)` and `store_snapshot` for both sides after every `PairRun::prepare`, including previews and no-change runs. `PairRun::snapshot` (`crates/foldover-engine/src/run.rs:925-942`) clones every path, fingerprint and version into a `Vec<SnapshotEntry>`; `PairStore::replace_snapshot` (`crates/foldover-state/src/pairs.rs:276-296`) re-encodes them, and `Database::replace_snapshot` (`crates/foldover-db/src/database.rs:806-883`) reads the whole stored snapshot and merges. `ObservedEntry` is `Eq` (`crates/foldover-endpoint/src/local.rs:381-384`), and unchanged observations are reused verbatim (`known.clone()` at `crates/foldover-endpoint/src/sftp.rs:646-651` and `crates/foldover-endpoint/src/local.rs:1198-1204`).
- **Hypothesis:** on a no-change run the new observation equals the previous snapshot entry for entry, so the entire encode/read/merge/write path is wasted. At 1M entries per side, ADR 0046 measures 46 ms per 100k unchanged snapshot at the DB layer; the full path also includes `snapshot()` allocation and encoding plus the DB read, plausibly around a second per side per run.
- **Proposed change:** have `PairRun::prepare_on` compare the new observation (plus the excluded-entry cache) with the previous snapshot map in memory before dropping the map, and return a `[bool; 2]` "changed" flag. `perform` stores a snapshot only for sides whose flag is true. The comparison is O(n) with no allocation: check that the lengths agree, then confirm every previous entry is present and equal in either the cache or the new observation. No schema change.
- **Expected impact:** saves one snapshot encode plus one DB read/merge per unchanged side per run; estimated 0.5-1.5 s per 1M-entry no-change run, and a large fraction of preview cost on unchanged trees. Confidence: **high** that the work is redundant when unchanged, **medium** on the magnitude.
- **Effort and risk:** small-medium. The comparison must treat the cache and the observation as disjoint and must not skip a store when an entry moved between them; the equality check catches version changes. A bug that skips a needed store would make the next run reread more data but would not lose any. Existing snapshot tests (`malformed_snapshot_versions_are_cache_misses`, `restoring_an_identical_snapshot_writes_nothing`) cover the store semantics.
- **Measurement plan:** instrument `store_snapshot` call counts and time them during a no-change second sync of a 100k-file tree; compare before/after. Add a unit test where exactly one entry changes and confirm that side is still stored.

### 5. Do not spend a `DELETE` per base row on an empty resolution table

- **Category:** redundant work and caching (database)
- **Evidence:** `crates/foldover-db/src/database.rs:965-990` prepares `DELETE FROM resolution WHERE pair_id=?1 AND path=?2` and executes it for every `BaseChange::Put` and `BaseChange::Remove`. The resolution table holds a person's unresolved conflict choices (`SCHEMA_V6`, `crates/foldover-db/src/database.rs:103-113`) and is empty for the overwhelming majority of pairs and runs. `change_base` is called once per `COMMIT_BATCH` (256) with up to 256 changes.
- **Hypothesis:** a run that writes 1M base rows issues 1M indexed `DELETE` statements against an empty table; each is a B-tree lookup that finds nothing. That is pure overhead on the commit path while copy workers wait for the engine lock.
- **Proposed change:** before the loop, check once per transaction whether the pair has any resolutions (`SELECT EXISTS(SELECT 1 FROM resolution WHERE pair_id=?1)`); only execute the per-row delete when it does. A more precise follow-up is to pass the set of resolution paths the engine actually applied (it already loads `resolutions` in `run_inputs`, `crates/foldover-engine/src/configuration.rs:242-263`) and delete only those.
- **Expected impact:** removes up to 1M pointless `DELETE` statements per large run; estimated 1-3 s of CPU at 1M changes, a few percent of apply-phase time. Confidence: **high** that the deletes happen, **medium** on the magnitude.
- **Effort and risk:** tiny; the `EXISTS` check is stable within the transaction because the pair revision guard prevents concurrent resolution changes. Risk: a resolution inserted between the check and the loop would be missed, but the revision check and single-writer lock make that impossible; existing resolution tests cover spending.
- **Measurement plan:** add a criterion bench `db_commit_no_resolutions_100k` that commits 100k base changes with an empty resolution table; compare statement counts and wall time before/after. End-to-end: time a 100k-file sync against the fixture server.

### 6. Make `skip_branches` linear in the number of rows

- **Category:** algorithmic complexity
- **Evidence:** `crates/foldover-engine/src/plan.rs:307-333` — for every row, `skipped.iter().any(|root| root.is_ancestor_of(&row.path))` scans the entire list of active skipped roots, and `skipped` grows with every matching skip. `skip_branches` is called from `plan_resolved` (`plan.rs:600`) with the run's `Choices.skips`. Rows are sorted by raw path bytes, so a skipped root's descendants form a contiguous span. The same linear-scan pattern appears in `Executor::below_failure` (`crates/foldover-engine/src/run.rs:1707-1709`), which is O(N·F) for F failures.
- **Hypothesis:** with K skipped roots and N rows the pass is O(N·K); a preview where a person skips a large folder (or many paths) turns planning into a quadratic scan. For N=1M and K=10k this is 10^10 byte comparisons. The failure scan is the same shape on error-heavy runs.
- **Proposed change:** replace the linear scan with a stack of active skipped roots. Because rows are sorted, once a row is not below a root no later row can be; pop roots that cannot contain the current row, then check only the innermost remaining root. Apply the same stack technique to `below_failure` during the forward pass; for the reverse pass, keep a `BTreeSet` of failed roots and check the row's ancestors, or accept the linear scan there because failures are normally rare.
- **Expected impact:** removes the quadratic term; planning stays O(N·depth) worst case. Runs without skips already return early, so the win is confined to skip-heavy or failure-heavy runs. Confidence: **high** on the complexity, **medium** on how often the case occurs in this workload.
- **Effort and risk:** small; the stack invariant must be proven against sorted byte order (a root's descendants are contiguous because `/` sorts before any byte that can follow a prefix). Risk: a subtle ordering bug could leave a skipped subtree unskipped or skip too much. `plan.rs` has unit tests and `crates/foldover-engine/src/plan_properties.rs` has property tests.
- **Measurement plan:** add a criterion bench `plan_skipped_100k_entries` with 10k skips over 100k rows; compare before/after. Run `PROPTEST_CASES=500000` on the plan properties to confirm identical decisions.

### 7. Make Unicode name keys cheap for ASCII paths and avoid per-row allocation

- **Category:** allocation and memory layout / data structures
- **Evidence:** `crates/foldover-engine/src/plan.rs:421-439` (`name_key`) allocates a `String` via `text.nfc().collect()` for every path whenever either side is normalized (any local↔SFTP pair), and `mark_name_clashes` (`plan.rs:351-410`) stores those `Vec<u8>` keys in a `HashMap` for every row. `align_spellings` (`crates/foldover-engine/src/run.rs:1519-1522`) composes every base and observed path with another `nfc().collect()`. `SftpEndpoint::compose` (`crates/foldover-endpoint/src/sftp/spelling.rs:503-509`) does the same for every name written. ASCII paths are the common case and need no normalization.
- **Hypothesis:** for a million-document tree, the planner performs millions of needless NFC collections and `Vec`/`String` allocations per run; these are pure CPU and allocator costs on the run thread. A fast path that returns the input bytes for ASCII, plus a borrowed or `Cow` key for the hash map, removes them.
- **Proposed change:** in `name_key`, return the path bytes unchanged when `text.is_ascii()` and the matching mode is `Exact`, or when `text.is_ascii()` and matching is case-sensitive (NFC is the identity for ASCII); case-insensitive ASCII still needs `to_ascii_lowercase`. Change the key type to `Cow<[u8]>` (or a small enum) so `mark_name_clashes` does not allocate for ASCII. Add the same `is_ascii()` fast path to `align_spellings`'s `composed` closure and to `SftpEndpoint::compose`.
- **Expected impact:** single-digit to ~10% off plan-phase CPU for million-entry trees; small in end-to-end terms next to SFTP I/O, but free and safe. Confidence: **medium** (allocation counts verified by reading; wall-clock unverified).
- **Effort and risk:** small-medium; the `Cow` key change touches `mark_name_clashes` and every other `name_key` caller (`unavailable`, `uncertain_absences`). Risk: a caller that relies on an owned `Vec` must clone; behavior is unchanged because ASCII NFC is the identity.
- **Measurement plan:** run `cargo xtask bench-check` (`plan_100k_entries`) before/after; add a 1M-entry plan variant and an allocation counter in a bench to count keys per row.

### 8. Add a release profile with thin LTO and one codegen unit

- **Category:** build and startup configuration
- **Evidence:** the workspace `Cargo.toml` has no `[profile.release]` section, so release builds use Cargo defaults (`codegen-units = 16`, no LTO); `scripts/build.sh:150-152` builds with `--release`. Hot CPU code includes BLAKE3 streaming (`crates/foldover-fs/src/root/content.rs:12,103`), planning (`crates/foldover-engine/src/plan.rs`), and the SFTP wire codec (`crates/foldover-net/src/sftp/wire.rs`).
- **Hypothesis:** the CPU-bound portions of a large run (hashing, planning, packet encode/decode) benefit from cross-crate inlining and a single codegen unit; the effect is a few percent, but it is cheap insurance for a workload that runs for hours.
- **Proposed change:** add to the workspace `Cargo.toml`:
  ```toml
  [profile.release]
  lto = "thin"
  codegen-units = 1
  ```
  Keep the dev profile unchanged.
- **Expected impact:** ~5-15% off CPU-bound portions, a few percent of end-to-end time for large trees. Confidence: **medium**.
- **Effort and risk:** trivial; risk is longer release link times and slightly larger binaries; no behavioral change.
- **Measurement plan:** run `cargo xtask bench-check` on an idle host before/after and compare medians for `hash_64_mib`, `plan_100k_entries` and `scan_10k_files`; record the build-time delta.

---

## Requires approval (changes a correctness contract)

These ideas are not ranked because they weaken documented guarantees. They are listed so the trade-off is explicit.

- **Stat-trust SFTP observation.** Skipping the content re-read when `(size, mtime, mode, owner)` matches the stored fingerprint would remove the largest remaining cost for steady-state runs, but it reverses the stated invariant that "SFTP stat cannot prove a file unchanged" (`crates/foldover-endpoint/src/sftp/readers.rs:1-6`, `crates/foldover-endpoint/src/sftp.rs:469-472`; ADR 0046). It needs a product decision, an opt-in or a bounded re-hash schedule, and a same-size/same-mtime safety net.
- **Stat-key verification for moves, archives and replacements.** `verified_at_on` (`crates/foldover-endpoint/src/sftp.rs:823-837`) re-reads and re-hashes a file before every move, archive and replacement. Comparing the stat key instead would save a full download per affected file, but it carries the same missed-edit risk as the previous item.
- **`synchronous = NORMAL` for the state database.** This would cut the fsync cost of every commit batch. WAL remains crash-safe for application crashes, but a power loss could drop committed batches; the run would re-plan already-written files and repair itself, at the cost of subtler base/journal reasoning.

## Open questions and unverified items

- **No code was executed.** This session had no `cargo` or `rustc` on `PATH`, so no benchmark, test or profile was run. Every impact magnitude above is a hypothesis. The code-structure facts and line numbers were read at commit `d10bf29`; ADR 0046's numbers are the only live measurements cited.
- **The SQLite query plan for the journal delete was inferred, not observed.** The schema (`PRIMARY KEY (pair_id, run, seq)`, no index on `run`) implies a range scan over every journal row of the pair; `EXPLAIN QUERY PLAN` on a populated database would confirm it.
- **The workload shape is unknown.** The directory/file ratio, the changed fraction per run, and the archive fraction determine whether the observe-side proposal (2), the archive proposal (3), or the database proposals (1, 5) dominate. A `find | wc -l` per top-level directory on the target tree would settle it.
- **Server facts are unverified.** The target SFTP server's `MaxSessions`, READDIR batch size, and maximum accepted request size affect the size of the round-trip wins and the viability of more sessions.
- **The snapshot comparison assumes cache and observation are disjoint.** `cache` is built from `observation.entries()` and then `exclude` removes those paths, so they should be; a debug assertion or a merge-length check would make the invariant explicit.
- **The journal-delete fix assumes no other writer inserts a different run's journal between batches.** The pair revision guard and single-writer lock make this likely, but the DB tests should confirm it under the existing concurrency tests.
- **The untracked `crates/foldover-endpoint/tests/scratch_perf.rs` present in the working tree was not created by this pass and was left untouched.**
