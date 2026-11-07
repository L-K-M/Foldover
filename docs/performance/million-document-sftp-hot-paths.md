# Million-document LAN SFTP performance proposals

Foldover is a preview-first folder synchronizer and backup tool: its CLI and native macOS UI send jobs to the Rust `foldoverd` agent, which prepares observations and a plan, applies checked copies through endpoint drivers, and commits recoverable state to SQLite. This report targets **end-to-end wall time for comparing and copying millions of documents to a LAN SFTP server**, with network requests/bytes, CPU time and peak memory as explanatory metrics. The eight proposals below are ranked by workload coverage and plausible time saved, discounted for implementation effort and correctness risk: remove demonstrated request amplification and repeated journal scans first, then improve enumeration, memory ownership and scheduling. All projected speedups are **unverified hypotheses**, not measured improvements or guarantees.

## Ranked proposals

1. Right-size SFTP read windows for small files
2. Make journal cleanup use indexed run ranges
3. List independent SFTP directories concurrently
4. Plan directly from observations without version-map copies
5. Stream snapshot encoding without rebuilding whole trees
6. Observe the local and SFTP roots concurrently
7. Index cached Unicode filename spellings
8. Index unavailable subtree prefixes in SFTP scans

## Scope, execution paths and evidence

Analysis date: 2026-11-07. Source references use commit `c997ae9d3231e862d74f74efb1e577d0fe07ff04`; the relevant code is unchanged in documentation-only main revision `d10bf29257bae6f20c9b707ae7669be18e2de159`. Line numbers refer to that source, not to this document. Only this new Markdown file is part of this change.

### Execution-path map

| Phase | Concrete source locations | Source-verified behavior |
| --- | --- | --- |
| Entry and dispatch | `crates/foldoverd/src/main.rs:48-69`; `crates/foldover-cli/src/main.rs:912-969`; `crates/foldover-service/src/sync.rs:844-1016` | The agent opens the service and serves requests. Both a preview and an execution job call `PairRun::prepare`; execution obtains a fresh plan rather than trusting a held preview. |
| Observe and plan | `crates/foldover-engine/src/run.rs:696-886` | `prepare_on` checks identities and filters, observes the left side and then the right side, aligns names, protects unread paths, and plans against the base. |
| Remote enumeration and comparison | `crates/foldover-endpoint/src/sftp.rs:482-705`; `crates/foldover-net/src/sftp.rs:267-300,339-412` | Directories are listed serially before content reads start. File comparison already uses up to four reader workers, but re-reads SFTP file contents even when attributes match. |
| Apply and verify | `crates/foldover-engine/src/run.rs:1174-1331`; `crates/foldover-engine/src/run/copies.rs:57-119`; `crates/foldover-endpoint/src/sftp/transfer.rs:130-226,428-531`; `crates/foldover-endpoint/src/sftp.rs:823-837` | Copies already run concurrently where endpoints allow it. Data is staged and checked; an existing destination is version-checked before replacement. Base and journal updates are committed in batches. |
| Persistence and finish | `crates/foldover-service/src/sync.rs:908-915`; `crates/foldover-db/src/database.rs:806-881,945-994`; `crates/foldover-db/src/database/journal.rs:79-107`; `crates/foldover-engine/src/run.rs:1297-1326` | Observations are cached before execution. Base changes and journal records share a transaction; completed runs also perform archive retention work. |

**Correctness boundary:** SFTP attributes are not unchanged-content proof. `SftpEndpoint::observe_entry` reads the version before considering reuse of the stored observation (`sftp.rs:631-653`); `docs/adr/0028-sftp-endpoints.md` explains the same-second replacement problem. None of these proposals removes content comparison, planned-version checks, no-replace creation, archives, undo journaling, identities, canaries, review gates, or leases. SFTP Full durability must continue to be rejected. No production server, SSH configuration, application installation or user data was changed for this analysis.

### Measurements actually run

Host: Apple M4 Max, 14 logical CPUs, 36 GiB RAM, macOS 27.0.1, Rust/Cargo 1.99.0, optimized benchmark profile. The host was busy: observed load averages were approximately 56/45/48. Timings below are **local diagnostic baselines only**, not capacity estimates for an idle host, a NAS or a million-file run. Existing cached Criterion comparisons were not treated as before/after optimization results.

The existing setup is documented in `perf/README.md:10-28`, with budgets in `perf/budgets.toml:6-14`; `xtask/src/bench.rs:14-20` selects five local benchmark targets. It does not measure an SFTP sync, per-256-write journal growth, or full preparation memory.

```sh
cargo bench --locked -p foldover-engine --bench plan -- --warm-up-time 1 --measurement-time 3
cargo bench --locked -p foldover-state --bench db -- --warm-up-time 1 --measurement-time 3
cargo test --locked -p foldover-net sftp::tests -- --nocapture
cargo test --locked -p foldover-endpoint --lib sftp:: -- --nocapture
cargo test --locked -p foldover-db journal::tests -- --nocapture
```

All five commands passed. A writable Cargo target directory and the installed Rust toolchain were supplied through the environment; no build configuration was edited. OpenSSH's `/usr/libexec/sftp-server` was present. Tests passed: 16 transport tests, 44 endpoint/session/reader tests, and two journal tests, including the real local SFTP-server cases. These are correctness tests, not LAN performance tests.

| Existing benchmark | Median point estimate, milliseconds |
| --- | ---: |
| `plan_100k_entries` | 63.64 |
| `db_commit_100k` | 100.64 |
| `db_read_100k` | 31.12 |
| `db_snapshot_unchanged_100k` | 22.35 |
| `db_snapshot_rewrite_100k` | 149.65 |

Medians were read from Criterion's `new/estimates.json`, not its reported mean interval. The plan fixture has 100,000 paths on the left and fewer on the right and in the base (`crates/foldover-engine/benches/plan.rs:17-104`). The DB commit fixture writes one large batch with `JournalUpdate::None` (`crates/foldover-state/benches/db.rs:77-95`), so it cannot expose proposal 2's growing journal cost.

Two additional, disposable probes supplied deterministic counters:

- **SFTP probe:** compiled unmodified copies of `foldover-net/src/sftp.rs` and `sftp/wire.rs` with `rustix` 1.1.5 in an external release-mode harness. It called `Session::open_read`, `read_all` and `close` against a scripted SFTP-v3 subprocess. The subprocess returned the requested bytes up to the configured file length, then EOF; all returned payload bytes were checked. This tests the actual transport implementation, but not SSH, storage or LAN timing.
- **Journal probe:** Python SQLite 3.54.1, in memory, using the journal table DDL extracted from `database.rs:376-390`. Before each 256-row insertion it ran the current cleanup DELETE, with a progress handler counting VM instructions only during DELETE. A separate run substituted two indexed range DELETEs and verified the final row count. This is a SQL micro-experiment, not a measurement of Rust's bundled SQLite or full sync time. A reproduction appears below.

| Scripted SFTP file bytes | READ requests | EOF replies |
| ---: | ---: | ---: |
| 0 | 16 | 16 |
| 1 | 32 | 31 |
| 4,096 | 32 | 31 |
| 32,768 | 16 | 15 |
| 32,769 | 32 | 30 |
| 524,288 | 32 | 16 |

| Journal rows inserted in 256-row batches | Current DELETE VM instructions | Two range DELETEs, VM instructions |
| ---: | ---: | ---: |
| 8,192 | 508,193 | 796 |
| 16,384 | 2,064,961 | 1,596 |
| 32,768 | 8,324,225 | 3,196 |

### Shared end-to-end acceptance protocol

**Not run:** a representative LAN test needs a dedicated disposable destination and server access. Use a fixed, recorded manifest of 1,000,000 files, initially 4 KiB each in 10,000 directories; separately test 32 KiB, 1 MiB, deep trees, wide directories and a sample matching the real document-size distribution. This is a proposed diagnostic workload, not a claim about the user's files. Use deterministic non-identical payloads and record NAS filesystem, RTT, link rate, SSH cipher, server version, durability and metadata policies.

For each revision measure: first population of an empty target, a no-change repeat, a 1% content-edit repeat, and a preview followed by its execution. Keep review/first-run gates enabled and record human approval wait separately. Time preview and execution separately and together, using the existing CLI's `sync ID --preview` and `sync ID --progress` commands on a matching release build. On macOS use the authorized app-bundled CLI, as required by `README.md:89-91`; do not bypass private IPC authentication. For the repeat-run case, an exact timing command is:

```sh
/usr/bin/time -l "$FOLDOVER_CLI" sync "$PAIR_ID" --progress > "$RESULTS/run.jsonl"
```

Set `FOLDOVER_CLI`, `PAIR_ID` and `RESULTS` explicitly for the disposable pair. A first-run approval must use the token of the freshly reviewed plan, not a hard-coded token. Run at least five repetitions per scenario and report median and range; distinguish warmed filesystem caches from a separately documented cold-start procedure. Count transport requests by opcode, transferred content bytes, files/second, directory-list time, hash/read time, plan time, snapshot time, journal/commit time, retention time, CPU seconds, peak RSS and failures. Retain request counts and correctness results even when noisy timing is inconclusive. All per-proposal benchmark additions below are **proposed**, not commands claimed to exist today. Gains overlap and must not be added together.

## Full proposals

### 1. Right-size SFTP read windows for small files

- **Category** — I/O and batching.
- **Evidence** — Source-verified: `crates/foldover-net/src/sftp.rs:30-35,339-412`, `Session::read_from`, always sends 16 requests of 32 KiB before consuming replies. After a short DATA response, later answers are consumed but ignored; a further window is required to observe EOF. `crates/foldover-endpoint/src/sftp.rs:750-803`, `read_range_on`, knows the expected length but does not pass it to the transport. Measured: a 4 KiB file required 32 READs and 31 EOF responses in the scripted probe above.
- **Hypothesis** — Millions of small-file comparisons pay for excess request encoding, pipe traffic, server dispatch, responses and an extra window. Reducing requests should shorten remote comparison even though file contents still cross the network. Thirty-two pipelined requests are not 32 serialized RTTs; a 16-fold request reduction would not imply a 16-fold sync speedup.
- **Proposed change** — Add a length-aware read entry point accepting the expected end offset, used by `read_range_on`. Size the in-flight window to the remaining chunks, refill continuously for large files, and perform a bounded EOF/growth probe at the expected end rather than sending an entire speculative window. Treat short non-EOF reads as partial progress, never as proof of EOF. Retain the byte-count limit, post-read stat check, cancellation, response-ID matching, common deadlines, maximum packet size and error draining. Keep a safe unknown-length path for callers without a size. Target two READs for a normal 4 KiB file: payload plus growth/EOF probe.
- **Expected impact** — Hypothesis: about 94% fewer READ requests for the measured 4 KiB case; 15-40% less end-to-end time on small-document, request-bound repeats, potentially under 5% when content bandwidth dominates. High confidence in the current amplification; medium confidence in a material phase improvement; low confidence in the wall-time range without LAN measurements.
- **Effort and risk** — Approximately 2-4 engineer-days; transport and SFTP endpoint only, medium correctness risk. Short reads, growing/shrinking files, resumed offsets and out-of-order replies can cause gaps or missed changes if implemented incorrectly.
- **Measurement plan** — Extend the existing `Fake` protocol fixture in `crates/foldover-net/src/sftp/tests.rs:189-264` with READ/EOF counters and lengths 0, 1, 4,096, 32,768, 32,769 and 524,288. Reproduce the baseline counts before changing code, then assert bounded requests and identical bytes after it. Run `cargo test --locked -p foldover-net sftp::tests -- --nocapture` and `cargo test --locked -p foldover-endpoint --lib sftp:: -- --nocapture`, especially short-read, wire-alignment and changed-length cases. Use the shared LAN protocol and reject the speedup hypothesis if reduced requests do not reduce comparison or total wall time.

### 2. Make journal cleanup use indexed run ranges

- **Category** — Algorithmic complexity.
- **Evidence** — Source-verified: `crates/foldover-db/src/database/journal.rs:79-107`, `journal::apply`, executes `DELETE FROM run_journal WHERE pair_id=?1 AND run!=?2` for every nonempty journal batch. The primary key is `(pair_id,run,seq)` in `crates/foldover-db/src/database.rs:376-390`; `crates/foldover-engine/src/run.rs:52,1256-1260` commits at approximately 256 accumulated updates. Measured in the SQL probe: the current query plan uses only `pair_id`, and doubling inserted rows approximately quadrupled cleanup VM work. The two range queries used `pair_id` and a `run` range.
- **Hypothesis** — After the previous journal is removed, the inequality repeatedly walks the growing current-run journal without deleting anything. For N successful writes in batches of B, examined current-run rows can grow as O(N²/B), approximately 1.95 billion row visits for one million writes and B=256. This is a source-derived scaling model, not a measured million-file run.
- **Proposed change** — Within the existing base/journal transaction, replace the inequality DELETE with two statements: `DELETE FROM run_journal WHERE pair_id=?1 AND run<?2` and the equivalent `run>?2`. Both are direct ranges in the current primary key and together preserve the existing semantics for non-null run names. Verify the plan with the bundled SQLite version. Do not disable journaling, enlarge the crash-recovery window or move cleanup outside the atomic commit.
- **Expected impact** — Hypothesis: remove over 99% of cleanup VM work once a same-run journal is large, as supported by the small SQL experiment. Approximately 5-25% less end-to-end time on million-file create/replace runs if journal CPU becomes material; nearly zero on no-change runs, and possibly under 5% on slow transfers. High confidence in the avoidable scan; medium confidence in the SQL remedy; low confidence in the total-time range.
- **Effort and risk** — Approximately 1-2 engineer-days including tests; small blast radius and low-to-medium risk. Preserve replacement of every other run's journal, pair isolation, empty-batch behavior and rollback on failure.
- **Measurement plan** — Reproduce the SQL counters below, then add a real `PairStore` benchmark that records 256-entry journal batches to totals of 8,192, 16,384, 32,768 and 1,000,000 rows, rather than using `JournalUpdate::None`. Compare cleanup VM steps/query plans, CPU and commit time; confirm near-linear total cleanup work. Run `cargo test --locked -p foldover-db journal::tests -- --nocapture` and `cargo test --locked -p foldover-engine --lib undo`. Verify first and subsequent batches, old/new lexicographic run names, interrupted commits and undo before the shared initial-population LAN test.

### 3. List independent SFTP directories concurrently

- **Category** — Concurrency and parallelism.
- **Evidence** — Source-verified: `crates/foldover-endpoint/src/sftp.rs:493-543`, `observe_reporting`, pops one pending directory and completes its `read_dir` before considering another. `crates/foldover-net/src/sftp.rs:267-300`, `Session::read_dir`, serializes OPENDIR, READDIR pages and CLOSE on that session. A bounded session pool already exists in `crates/foldover-endpoint/src/sftp/sessions.rs:55-163`; content reading already uses four workers in `sftp.rs:588-620`. Listing latency's share of LAN run time is unverified.
- **Hypothesis** — In a tree with many modest directories, one outstanding directory walk leaves the LAN and NAS waiting on metadata round trips. Sibling listings can overlap without changing content comparisons or the preview-before-write boundary.
- **Proposed change** — Use a bounded directory-work queue and up to the existing four sessions to list independent directories once their parent listing has admitted them. Let the observation's owner thread apply filters, enqueue child directories, collect errors and report progress. Keep all listing complete before the Checked/content phase, preserving the existing progress contract. Bound queued result bytes as well as worker count, sort issues deterministically and preserve the one-session fallback and cancellation while the pool is exhausted.
- **Expected impact** — Hypothesis: 2-3 times faster enumeration with sufficient sibling directories; 5-20% less end-to-end time when enumeration occupies 10-30% of the run. Little benefit for a single huge directory, deep single-child chains or content-bandwidth-bound comparisons. Medium confidence in enumeration overlap; low confidence in total benefit.
- **Effort and risk** — Approximately 4-7 engineer-days; endpoint enumeration and scheduler tests, medium risk. Concurrent failures must not turn unread subtrees into deletions; queue growth, server session refusal and cancellation can regress memory or responsiveness.
- **Measurement plan** — Add an enumeration phase timer and OPENDIR/READDIR counters; compare 1, 2 and 4 listing workers on the shared 10,000-directory fixture and a 100,000-directory variant. Hold content workers constant. Require identical admitted paths, issues and plans, bounded RSS and no extra listings. Run `cargo test --locked -p foldover-endpoint --lib sftp:: -- --nocapture`, including `the_whole_tree_is_listed_before_any_entry_is_checked`, `several_sessions_observe_what_one_session_does` and exhausted-pool cancellation cases, then measure total LAN wall time.

### 4. Plan directly from observations without version-map copies

- **Category** — Allocation and memory layout.
- **Evidence** — Source-verified: `crates/foldover-endpoint/src/local.rs:584-589`, `Observation::versions`, clones every path and materializes another `BTreeMap<SyncPath,EntryVersion>`. `crates/foldover-engine/src/run.rs:864-872` builds both complete projections for `plan_resolved` while the full observations and base remain live. `crates/foldover-engine/src/plan.rs:527-568` already performs a three-way ordered merge; proposing a new union set or sorting all paths again would duplicate an existing optimization. Peak allocation cost is unverified.
- **Hypothesis** — At million-path scale, two temporary trees add allocation, path copies and cache misses just before planning, and can trigger memory pressure. The comparison only needs borrowed paths and copyable versions, not owned duplicate trees.
- **Proposed change** — Add an internal typed read-only tree view supporting ordered iteration and version lookup over either an observation or an existing version map. Adapt planner validation, name-clash handling and move pairing to that view. Keep the public `plan_candidates`/`plan_resolved` entry points as compatible wrappers, retain the current ordered merge, and clone a path only when the returned plan must own it. Do not change path normalization, raw-byte ordering or reconciliation decisions.
- **Expected impact** — Hypothesis: avoid roughly 100-300 MiB of temporary storage per million paths per side-pair, depending on path lengths and layout, and reduce full preparation CPU by 10-25%. Approximately 1-5% less end-to-end time on a network-bound run, potentially more under memory pressure. Medium confidence that allocations fall; low confidence in the memory magnitude and total speedup without allocation/RSS profiles.
- **Effort and risk** — Approximately 4-6 engineer-days; planner internals and preparation call sites, medium risk. Generic accessors and borrowing can complicate code; validation, conflict suppression and deterministic move pairing must remain identical. No new unsafe code or dependency-layer changes are needed.
- **Measurement plan** — Run `cargo bench --locked -p foldover-engine --bench plan -- --warm-up-time 1 --measurement-time 3` as a planner regression guard, not proof of the improvement: that benchmark already starts from projected version maps. Add a preparation benchmark including `Observation::versions` at 100,000 and 1,000,000 paths, with allocation counts, CPU and peak RSS. Require byte-for-byte equivalent preview items/tokens and base updates across planner property tests: `cargo test --locked -p foldover-engine --lib plan`. Confirm the projected memory saving in the shared LAN run.

### 5. Stream snapshot encoding without rebuilding whole trees

- **Category** — Redundant work and caching.
- **Evidence** — Source-verified: `crates/foldover-engine/src/run.rs:925-945`, `PairRun::snapshot`, builds a temporary map of references, then clones paths and serializes fingerprints into a full vector. `crates/foldover-state/src/pairs.rs:275-293`, `PairStore::replace_snapshot`, clones paths and fingerprints again and allocates encoded versions. `crates/foldover-db/src/database.rs:829-879` then sorts references and merges against stored rows. Existing optimizations matter: unchanged SQL rows are already read as borrowed blobs and are not rewritten; SFTP keeps its old fingerprint when a fresh read proves the same version (`sftp.rs:644-652`). Measured unchanged-snapshot median: 22.35 ms per 100,000 rows, excluding `PairRun::snapshot`.
- **Hypothesis** — Repeated full-tree serialization and ownership conversions cost CPU and peak memory even when the database correctly writes no changed rows. Optimizing this boundary can reduce the pause between comparison and execution without weakening SFTP content freshness.
- **Proposed change** — Merge the already-ordered observation/cache iterators directly instead of collecting another map. Pass a sorted borrowed snapshot view through the state layer to a DB-owned merge operation; encode each fingerprint/version into bounded reusable storage as it is consumed. Keep the existing unsorted-input API as a compatibility wrapper and retain duplicate-path rejection, pair-revision validation, atomicity and exact exclusion semantics. Preserve changed-row-only writes; do not add a size/mtime-only SFTP digest cache or silently skip a required comparison.
- **Expected impact** — Hypothesis: 20-40% less snapshot-stage CPU and substantially fewer per-path temporary allocations; approximately 0-3% less total time when SFTP reads dominate, with larger memory-pressure benefits possible but unquantified. Medium confidence in removing conversions; low confidence in wall-time benefit. This ranks below wire and journal work because the measured local snapshot stage is relatively small.
- **Effort and risk** — Approximately 3-5 engineer-days; engine/state/DB interfaces, medium risk. Borrowing, iterator ordering, filtered paths and stale revisions can corrupt cache hints or cause unnecessary rereads if handled incorrectly. SQLite operations remain inside the DB driver.
- **Measurement plan** — Run `cargo bench --locked -p foldover-state --bench db -- --warm-up-time 1 --measurement-time 3`; retain both unchanged and all-fingerprints-changed cases. Add a full snapshot-export-plus-store benchmark at one million paths, counting allocations, bytes encoded, DB row changes and lock-hold time. Run `cargo test --locked -p foldover-db snapshot -- --nocapture` and verify that identical snapshots still change zero rows, while duplicate paths and stale revisions still fail atomically. Measure comparison-to-first-copy delay in the shared protocol.

### 6. Observe the local and SFTP roots concurrently

- **Category** — Concurrency and parallelism.
- **Evidence** — Source-verified: `crates/foldover-engine/src/run.rs:756-774`, `prepare_on`, finishes the left observation before starting the right. Local observation reuses settled stat fingerprints but otherwise hashes files (`crates/foldover-endpoint/src/local.rs:1199-1225`); SFTP content reading already has its own bounded workers. `crates/foldover-engine/src/run.rs:199-210` explicitly keeps `RunProgress` callbacks on the run thread. The degree of overlap available on the actual Mac/NAS is unverified.
- **Hypothesis** — A cold local scan/hash phase and a remote scan/read phase can use different resources concurrently. Their combined duration could move from approximately `Tlocal + Tremote` toward `max(Tlocal,Tremote)`, but gains are small if one side dominates or both contend for the same device.
- **Proposed change** — For local-to-SFTP pairs, use safe scoped observation workers with a shared atomic cancellation flag and bounded result/progress channels. Keep the original run thread as the only caller of external control and `RunProgress`; it polls while workers operate and joins both before name alignment/planning. Preserve deterministic errors, identity/filter setup, thread limits and the sequential fallback. Do not start writes during observation or reuse a stale preview.
- **Expected impact** — Hypothesis: 20-40% less observation time when phases are balanced; 5-20% less total time on cold comparisons, often under 5% on warm runs dominated by remote reads. Medium confidence in the overlap bound, low confidence that this workload is balanced enough to realize it.
- **Effort and risk** — Approximately 4-7 engineer-days; preparation orchestration, medium risk. Nested SFTP worker pools, extra live listings, contention, error ordering and cancellation can regress. Avoid sharing the mutable control callback directly across threads or changing the public callback threading contract.
- **Measurement plan** — Add per-side start/end timestamps and collect observed CPU/I/O overlap on cold and warmed versions of the shared fixture. Compare serial versus two-side observation with the SFTP reader count fixed at four; hold proposal 3's listing setting constant to attribute gains. Run `cargo test --locked -p foldover-engine --lib run` and the endpoint SFTP suite. Inject failure/cancellation on either side and require no writes, timely worker shutdown, identical plans and acceptable RSS before accepting any total-time improvement.

### 7. Index cached Unicode filename spellings

- **Category** — Data structures.
- **Evidence** — Source-verified: `crates/foldover-endpoint/src/sftp/spelling.rs:34-37,120-143` stores cached folder names in `Arc<Vec<Vec<u8>>>` and scans them for exact or NFC-equivalent matches. `note_added` uses a linear membership search at lines 201-221; `note_removed` retains over the whole vector at lines 226-243. Mutations update this cache even when the transferred name is ASCII, once that folder has a cached listing. The cache is already bounded to 512 folder entries (`spelling.rs:29-31,160-166`); it is not an absent cache. Wide-folder CPU cost is unverified.
- **Hypothesis** — After a Unicode lookup populates a wide folder's listing, repeated temporary creation, renames and removal can rescan many names per file. For M operations in a K-entry folder, this adds O(MK) name comparisons even without extra READDIR traffic.
- **Proposed change** — Replace the vector-only cached view with an indexed view: exact raw-name membership plus an NFC-key-to-candidates index. Compute normalized keys once when listing or adding a name; maintain both indexes on every existing `note_*` path. Preserve exact-byte preference, original candidate ordering for aliases, invalid-UTF-8 behavior, Fetching/Dirty invalidation and unknown-rename-outcome handling. Bound cache bytes as well as folder count so a few enormous directories cannot retain unlimited indexes.
- **Expected impact** — Hypothesis: 2-5 times less cache-maintenance/lookup CPU in wide cached folders, with 0-15% less total time in Unicode-heavy bulk replacement runs. Near zero if names never need listing-based respelling and the cache stays unpopulated. Medium confidence in the complexity reduction; low confidence in workload prevalence and total gain.
- **Effort and risk** — Approximately 3-5 engineer-days; SFTP spelling cache, medium risk. Incorrect alias choice or stale mutation tracking can target the wrong spelling; additional index memory and copy-on-write behavior must be measured.
- **Measurement plan** — Add a wide-directory replacement fixture with 1,000, 10,000 and 100,000 names, both NFC/NFD names and ASCII names after a Unicode lookup has populated the cache. Count normalized-key computations, names compared, cache bytes and READDIR calls. Run `cargo test --locked -p foldover-endpoint --lib sftp:: -- --nocapture`, including alternate-spelling, moved-folder and lost/replacing-rename invalidation cases. Compare cached-folder CPU scaling and total replacement wall time without attributing fewer network listings to a cache that already exists.

### 8. Index unavailable subtree prefixes in SFTP scans

- **Category** — Algorithmic complexity.
- **Evidence** — Source-verified: `crates/foldover-endpoint/src/sftp.rs:556-583,1713-1718` tests paths against every unavailable root, including a final full-map retain. In contrast, `crates/foldover-endpoint/src/local.rs:438-486`, `UnavailablePaths`, indexes roots and checks component prefixes; local observation uses it at lines 1166-1234. The engine also already indexes normalized unavailable roots in `crates/foldover-engine/src/run.rs:1944-1975`. The remaining candidate is the SFTP observer, not those optimized paths. Failure-heavy runtime cost is unverified.
- **Hypothesis** — With N readable entries and F unreadable subtrees/files, repeated vector scans can add O(NF) path comparisons and delay completion or cancellation. Healthy runs with F=0 do not meaningfully benefit, so this is a resilience-tail optimization rather than the first nominal-throughput change.
- **Proposed change** — Reuse the local observer's component-prefix index internally within the endpoint crate, including an empty-index fast path. Update it when recording SFTP issues, query it when admitting reads, and use its cancellation-aware final retain. Keep raw-byte component boundaries and original issues; do not substitute it for the engine's separate normalized-name protection. Continue suppressing descendants of a failed directory even if their results arrived earlier.
- **Expected impact** — Hypothesis: reduce protection checks toward O(N × depth × log F) rather than O(NF), with 10-50% less total time only when many failures make this CPU work substantial. Approximately zero on healthy runs. High confidence in the source-level complexity difference; low confidence in the failure frequency and wall-time range.
- **Effort and risk** — Approximately 1-3 engineer-days; internal observation helpers, low-to-medium risk. A prefix bug could expose failed subtrees to planning, so separator boundaries, late failures and normalized aliases need regression coverage. No public API or layer change is required.
- **Measurement plan** — Add an SFTP observation fixture with one million entries and 0, 100, 1,000 and 10,000 unavailable roots; use a scripted server to produce failures consistently without relying on superuser-sensitive permissions. Count root/prefix comparisons and final-retain CPU. Require the same issues, suppressed paths and zero destructive actions under failed roots; test `a`, `a/file` and `ab/file` explicitly. Run the endpoint SFTP suite and `cargo test --locked -p foldover-engine --lib unavailable`, then measure the failure-heavy shared LAN scenario and cancellation latency.

## Reproduce the SFTP request-count probe

Run this Python 3 script from the repository root with the pinned Rust toolchain on PATH. It copies the two transport modules unchanged into an automatically removed external directory, builds a minimal release harness, and runs a protocol fixture without SSH or user files. Its counts are also directly derivable from `Session::read_from`: a short nonempty DATA response suppresses later answers in the window, including EOF, so another 16-request window follows. The in-repository `Fake` regression additions in proposal 1 remain proposed work. The command uses cached dependencies; remove `--offline` if Rust's dependency cache is empty. This reproduces request counts, not LAN timing.

```python
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

server = r'''
import json, struct, sys
size, report = int(sys.argv[1]), sys.argv[2]
counts = dict(size=size, read_requests=0, eof_replies=0, data_bytes=0)
u32 = lambda n: struct.pack(">I", n)
field = lambda b: u32(len(b)) + b
def take(n):
    data = sys.stdin.buffer.read(n)
    if len(data) != n:
        raise EOFError
    return data
def send(body):
    sys.stdout.buffer.write(field(body))
    sys.stdout.buffer.flush()
def status(ident, code):
    send(bytes([101]) + ident + u32(code) + field(b"") + field(b""))
try:
    while True:
        packet = take(struct.unpack(">I", take(4))[0])
        kind, ident, fields = packet[0], packet[1:5], packet[5:]
        if kind == 1:
            send(bytes([2]) + u32(3))
        elif kind == 3:
            send(bytes([102]) + ident + field(b"h"))
        elif kind == 5:
            counts["read_requests"] += 1
            handle_size = struct.unpack(">I", fields[:4])[0]
            offset, wanted = struct.unpack(">QI", fields[4 + handle_size:])
            length = min(wanted, max(0, size - offset))
            if length:
                counts["data_bytes"] += length
                send(bytes([103]) + ident + field(b"x" * length))
            else:
                counts["eof_replies"] += 1
                status(ident, 1)
        elif kind == 4:
            with open(report, "a") as out:
                out.write(json.dumps(counts, sort_keys=True) + "\n")
            status(ident, 0)
        else:
            raise RuntimeError(kind)
except EOFError:
    pass
'''
client = r'''
#[allow(dead_code)] mod sftp;
use std::{env, ffi::OsStr, path::Path};
fn main() {
    let args: Vec<String> = env::args().collect();
    for size in [0usize, 1, 4096, 32768, 32769, 524288] {
        let value = size.to_string();
        let mut session = sftp::Session::spawn(Path::new(&args[1]), &[
            OsStr::new("-u"), OsStr::new(&args[2]),
            OsStr::new(&value), OsStr::new(&args[3]),
        ]).unwrap();
        let handle = session.open_read(b"fixture").unwrap();
        let mut taken = 0;
        session.read_all(&handle, &mut |chunk| {
            assert!(chunk.iter().all(|byte| *byte == b'x'));
            taken += chunk.len();
            Ok(())
        }).unwrap();
        session.close(handle).unwrap();
        assert_eq!(taken, size);
    }
}
'''
repo = Path.cwd()
with tempfile.TemporaryDirectory(prefix="foldover-sftp-read-probe-") as directory:
    work = Path(directory)
    (work / "src/sftp").mkdir(parents=True)
    for name in ("sftp.rs", "sftp/wire.rs"):
        shutil.copyfile(repo / "crates/foldover-net/src" / name,
                        work / "src" / name)
    (work / "Cargo.toml").write_text('''[package]
name = "foldover-sftp-read-probe"
version = "0.0.0"
edition = "2024"
[dependencies]
rustix = { version = "=1.1.5", features = ["event", "fs"] }
''')
    (work / "src/main.rs").write_text(client)
    (work / "server.py").write_text(server)
    subprocess.run([
        "cargo", "run", "--release", "--offline", "--manifest-path",
        str(work / "Cargo.toml"), "--", sys.executable,
        str(work / "server.py"), str(work / "counts.jsonl"),
    ], check=True, env={**os.environ, "CARGO_TARGET_DIR": str(work / "target")})
    print((work / "counts.jsonl").read_text())
```

## Reproduce the journal SQL micro-experiment

Run this from the repository root with Python 3. It extracts the actual final journal schema, measures cleanup only, and keeps the database in memory. VM counts can vary by SQLite version; the query plan and scaling trend are the relevant comparisons. It does not invoke Foldover or prove an end-to-end speedup.

```python
import re
import sqlite3
from pathlib import Path

source = Path("crates/foldover-db/src/database.rs").read_text()
ddl = re.search(
    r"CREATE TABLE run_journal_next \(.*?\) STRICT, WITHOUT ROWID;",
    source, re.S,
).group(0).replace("run_journal_next", "run_journal")
queries = {
    "current": ["DELETE FROM run_journal WHERE pair_id=?1 AND run!=?2"],
    "ranges": [
        "DELETE FROM run_journal WHERE pair_id=?1 AND run<?2",
        "DELETE FROM run_journal WHERE pair_id=?1 AND run>?2",
    ],
}
print("SQLite", sqlite3.sqlite_version)
for size in (8192, 16384, 32768):
    for label, deletes in queries.items():
        db = sqlite3.connect(":memory:")
        db.execute("CREATE TABLE pair (id BLOB PRIMARY KEY)")
        db.execute(ddl)
        db.execute("INSERT INTO pair VALUES (?)", (b"a",))
        steps = [0]
        def count():
            steps[0] += 1
            return 0
        for start in range(0, size, 256):
            db.set_progress_handler(count, 1)
            for query in deletes:
                db.execute(query, (b"a", "current"))
            db.set_progress_handler(None, 0)
            db.executemany(
                "INSERT INTO run_journal VALUES (?,?,?,?,?,?,?,?,?,?)",
                ((b"a", "current", i, f"p{i}".encode(), "right",
                  "create", b"v", None, None, None)
                 for i in range(start, start + 256)),
            )
        assert db.execute("SELECT count(*) FROM run_journal").fetchone()[0] == size
        print(size, label, steps[0])
        for query in deletes:
            print(db.execute("EXPLAIN QUERY PLAN " + query,
                             (b"a", "current")).fetchall())
        db.close()
```

## Deferred or rejected approaches

- **Reject a size/mtime-only remote content cache.** It misses content replacements with identical reported attributes, contrary to ADR 0028 and the passing `unchanged_sftp_attributes_cannot_hide_a_content_change` regression. A cooperative server with trustworthy content generations or compatible server-side digests would be a separate, approval-requiring capability design, not a transparent optimization for generic SFTP.
- **Reject removing validation, archives or undo to win a benchmark.** These change the product's safety behavior and are not needed for the ranked improvements.
- **Defer build/startup tuning.** `scripts/build.sh:51,147-151` already selects release builds by default, and the long-running agent amortizes startup. There is no CPU profile justifying LTO, codegen-unit or allocator changes ahead of wire, enumeration and data-structure work. This category was inspected but does not warrant one of the eight slots.
- **Do not blindly increase copy/session counts.** Four file workers, 16 outstanding chunks, pooled sessions, retry/clamping and separate lease traffic already exist (`crates/foldover-endpoint/src/sftp.rs:65-68,220-243`; `crates/foldover-endpoint/src/sftp/sessions.rs:105-163`; `crates/foldover-net/src/sftp.rs:33-35`). More workers may help, but NAS saturation and SSH limits must be measured before changing defaults.

## Open questions and unverified work

- No representative million-file LAN sync, SSH CPU profile, packet capture, Instruments/allocator profile, NAS I/O trace or cold-cache comparison was available. Only the local benchmarks, correctness suites and two counter probes above were run; none of the eight optimizations was implemented or benchmarked after a fix.
- What are the real document-size distribution, total bytes, directory width/depth, Unicode-name prevalence and changed-file fraction? These can materially reorder proposals 3-8.
- Is the source truly local storage or a mounted share backed by the same NAS? Shared-device contention could negate proposal 6.
- What RTT/link speed, SFTP implementation, SSH cipher, server session policy and NAS disk/cache limits apply? The scripted transport counter omits all of them; Python SQLite may also differ from Rust's bundled SQLite.
- How much total time is spent in remote content reads versus enumeration, local hashing, snapshot preparation, journal commits and retention? Existing progress rate starts at apply time (`crates/foldover-service/src/sync.rs:175-199`) and is not an end-to-end throughput measure.
- What fraction of runs explicitly preview before execution? Both prepare afresh today. Skipping the second observation without a trustworthy freshness proof is deliberately not proposed.
- Memory estimates and all percentage improvements remain hypotheses. Validate proposals independently on matching release revisions, then re-profile the combined implementation; request, CPU and allocation gains are not additive wall-time gains.
