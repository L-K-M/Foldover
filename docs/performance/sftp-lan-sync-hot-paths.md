# SFTP LAN sync hot paths: comparing and copying millions of documents

Foldover syncs folder pairs through one Rust agent (`foldoverd`) with a
preview-first engine: `PairRun::prepare_on` observes both sides
(`crates/foldover-engine/src/run.rs:696`), reconciles them in
`plan_resolved` (`crates/foldover-engine/src/plan.rs:507`), and
`PairRun::execute` (`crates/foldover-engine/src/run.rs:997`) applies the
plan through `transfer::copy` (`crates/foldover-endpoint/src/sftp/transfer.rs:125`)
over a blocking SFTP v3 client (`crates/foldover-net/src/sftp.rs`). This
document targets one workload: comparing and copying millions of documents
to an SFTP server on the local network, where end-to-end run time is the
primary metric. The compare phase is dominated by the SFTP side's decision
to re-read every file's content on every scan, and the copy phase by the
number of serialized request/response round trips each small file costs;
the ranking therefore puts content re-reads first, per-file round trips
second, and single-threaded local hashing third, weighted down by
implementation effort and semantic risk. Three claims below are anchored
by benchmarks run on this repository (release/bench profile, short
criterion windows, macOS 27 on Apple silicon); everything else is a code
reading and is labeled a hypothesis.

## Ranked proposals

1. Reuse stat fingerprints instead of re-reading unchanged SFTP content on every compare
2. Cut serialized per-file round trips in the SFTP upload path
3. Parallelize the local scan's hashing pass
4. Skip extended-attribute value reads the entry version discards
5. Raise SFTP session parallelism toward PLAN's 4–8 band and enlarge streaming chunks
6. Parallelize SFTP directory listings during observe
7. Replace linear unavailable/failed path scans with prefix indexes
8. Reduce allocation churn in planning, snapshots and name matching
9. Ship the agent with an optimized release profile

Proposal 1 changes documented verification semantics and needs owner
sign-off; see "Requires approval" below. If it is not approved, proposal 2
is the first implementable item.

## What was measured

Short criterion runs against the repository's own benches plus a scratch
harness driving `foldover_net::sftp::Session` against
`/usr/libexec/sftp-server` over stdio pipes (protocol and server cost
without network latency). Host: macOS 27.0.1, Apple silicon, release
profile, scratch build directory outside the repo.

| Measurement | Result |
|---|---|
| `scan_10k_files` (full `LocalEndpoint::observe`, 10,525 entries) | 2.29 s median (2.13–2.45 s), ≈ 229 µs/entry |
| `hash_64_mib` (`Root::hash_file`, 64 MiB) | 30.1 ms median (29.9–30.3 ms), ≈ 2.1 GiB/s |
| `plan_100k_entries` (two 100k sides + base) | 54.9 ms median (53.7–56.0 ms) |
| SFTP `lstat` (scratch harness, 4 KiB files) | 0.013 ms/op |
| SFTP create + write + close | 0.085 ms/file |
| SFTP open + read + close + lstat | 0.087 ms/file |

The scan number is above `perf/budgets.toml`'s 1000 ms budget for
`scan_10k_files`, but that budget was calibrated on a Linux machine
(`perf/README.md`); this is a cross-host difference, not evidence of a
regression.

---

### 1. Reuse stat fingerprints instead of re-reading unchanged SFTP content on every compare

- Category: redundant work and caching
- Evidence: `crates/foldover-endpoint/src/sftp.rs:468-470` states "SFTP
  stat cannot prove they stayed unchanged, even when size and modification
  time match"; `observe_entry` (`crates/foldover-endpoint/src/sftp.rs:631-659`)
  computes `self.version(...)` at line 640 — which for files calls
  `version_on` → `read_range_on(session, path, attributes, 0, …)`
  (`crates/foldover-endpoint/src/sftp.rs:681-693`) — and only then compares
  against the previous snapshot at line 647. The comparison therefore
  cannot prevent the read; it only avoids rewriting the snapshot. The local
  side does the opposite: `crates/foldover-endpoint/src/local.rs:1198-1199`
  reuses the stored version when `fingerprint.proves_unchanged` holds, with
  a racy window and a 3.5–7 day spread re-read
  (`crates/foldover-endpoint/src/local.rs:283-291`, `:341-348`).
- Hypothesis: for a compare run over an unchanged corpus, the SFTP observe
  downloads every byte of every remote file just to re-derive digests the
  snapshot already holds. For 1M documents averaging 20 KB (20 GB) on a
  1 Gbps LAN this is ≈ 200 s of transfer plus ≈ 3 round trips per file
  amortized over 4 reader threads (≈ 230 s round-trip time at 0.5 ms RTT) —
  on the order of 7–8 minutes per compare, all of it redundant when nothing
  changed. Unverified magnitude; the mechanism is verified in code.
- Proposed change: give SFTP entries the same fingerprint treatment local
  entries get. In `observe_entry`, before calling `version`, compare
  `stat_key(attributes)` and size against `previous[path]` and reuse
  `previous[path].version` when (a) the stat key matches, (b) the entry's
  `observed_at` is outside the `RACY_WINDOW` (SFTP mtimes have 1-second
  resolution per PLAN.md §4.7), and (c) the entry is inside its
  `reread_after` window (reuse the existing `Fingerprint` spread logic in
  `crates/foldover-endpoint/src/local.rs:341-348` so a large corpus is
  re-verified in slices). Files whose stat changed, or whose window
  expired, are read and hashed exactly as today. Keep the full-content
  verification path unchanged for `verified_at_on`
  (`crates/foldover-endpoint/src/sftp.rs:823-837`) so apply-time checks
  remain strict.
- Expected impact: compare phase over a mostly-unchanged remote corpus
  drops from O(corpus bytes) network reads to O(corpus entries) metadata
  listings — estimated 50–90% wall-clock reduction of the compare phase for
  the stated workload (medium confidence on magnitude, high confidence on
  direction).
- Effort and risk: medium (one observe path plus snapshot plumbing);
  the semantic risk is the point — a same-second, same-size edit on the
  server is discovered at the next re-read slice, not the next run. This
  amends ADR 0028's recorded "contents and link targets are re-read"
  stance and PLAN.md §4.8's weekly content-hash cadence becomes the backstop.
  Blast radius: change detection on SFTP and Mac-to-Mac-over-SFTP pairs.
- Measurement plan: add an SFTP integration bench that runs
  `SftpEndpoint::observe_from` twice over 100k small files with a populated
  `previous` map (second run has zero changes); record wall time and bytes
  read (count in the `sink` of `read_range_on`) before and after. A passing
  check: second-run bytes read fall to ~0 while a planted same-size edit
  outside the racy window is still detected.

### 2. Cut serialized per-file round trips in the SFTP upload path

- Category: I/O and batching
- Evidence: the small-file upload chain in
  `crates/foldover-endpoint/src/sftp/transfer.rs:446-530` (`send_on`) and
  `crates/foldover-endpoint/src/sftp.rs`: `receiving_on`
  (`transfer.rs:419-443`, one lstat via `attributes_on`), `write_new`
  (`sftp.rs:1673-1685`: create + N writes + close), `set_metadata_on`
  (`sftp.rs:940-979`: lstat, then fsetstat), `staged_version_on`
  (`sftp.rs:921-933`: a second lstat), `place_on`
  (`sftp.rs:1034-1075`: rename, plus a lookup when replacing). Every one of
  these is a synchronous `Session::request` — send one packet, wait for its
  reply (`crates/foldover-net/src/sftp.rs:689-694`). PLAN.md's capability
  matrix promises "pipelined requests" for SFTP (PLAN.md §4.7), but
  pipelining exists only inside one file's data stream (`read_from` /
  `write_chunks`, `crates/foldover-net/src/sftp.rs:339`, `:433`).
  Additionally `PairRun::execute` re-reads both root identities every 256
  rows (`COMMIT_BATCH`, `crates/foldover-engine/src/run.rs:52`,
  `:1236-1240`), each identity read costing an open+read+close SFTP
  sequence.
- Hypothesis: a small document costs ≈ 7–9 serialized round trips beyond
  its data writes. At 0.5 ms LAN RTT that is ≈ 4 ms of latency per file per
  worker; with 4 workers (`PARALLEL_COPIES`,
  `crates/foldover-endpoint/src/sftp.rs:68`) copying 1M small files spends
  ≈ 15–20 minutes in round trips alone. The scratch measurement (0.085 ms
  for create+write+close with zero network latency) confirms the cost is
  per-operation, not per-byte. Round-trip arithmetic is a hypothesis; the
  request count is verified from code.
- Proposed change: (a) merge `set_metadata_on`'s trailing lstat and
  `staged_version_on`'s lstat into one call that returns the attributes the
  version needs (one lstat saved per file); (b) for `Expected::Absent`,
  fold the absence pre-check into the `OPEN … CREATE|EXCL` of
  `create_new` (`crates/foldover-net/src/sftp.rs:313-328`) — the exclusive
  create is already the authoritative absence proof, and `place_on`'s
  rename still refuses to replace, so safety is preserved and one lookup
  per new file disappears; (c) extend `Session` with a small cross-file
  pipeline for metadata ops (the wire already demultiplexes replies by
  request id via `inflight`/`stashed`, `crates/foldover-net/src/sftp.rs:100-105`)
  so several small files' open/write/close/rename interleave on one
  session; (d) throttle `check_roots` to wall-clock (e.g. every 5 s)
  instead of every 256 rows.
- Expected impact: 30–60% wall-clock reduction on small-file-heavy copies
  (medium confidence); (a)+(b) alone remove 2 of ≈ 8 round trips (≈ 25%,
  high confidence on the count, medium on wall time).
- Effort and risk: (a) and (b) are small and low risk; (c) is medium-large
  with real desynchronization risk on error paths — the existing `drain`
  discipline (`crates/foldover-net/src/sftp.rs:782-800`) must be preserved.
  What could regress: replace-vs-create race reporting (an occupied
  destination must still fail without replacing), and cancellation
  latency while a pipeline is in flight.
- Measurement plan: extend `crates/foldover-endpoint/src/sftp/tests.rs` with
  a timing harness (or a criterion bench behind the existing
  `FOLDOVER_SFTP_SERVER` mechanism) copying 20k 4 KiB files to a local
  `sftp-server`; compare wall time and request counts (count `request`
  calls with a test counter) before and after; verify the race tests in
  `sftp/tests.rs` and `sftp/transfer.rs` stay green.

### 3. Parallelize the local scan's hashing pass

- Category: concurrency and parallelism
- Evidence: `LocalEndpoint::observe_reporting` processes entries in one
  serial loop (`crates/foldover-endpoint/src/local.rs:1165-1226`), calling
  `entry_version` → `hash_file` (`crates/foldover-endpoint/src/local.rs:854-871`,
  `crates/foldover-fs/src/root/content.rs:32-45`) one file at a time on the
  run's thread. The SFTP side already fans file reads out across sessions
  (`observe_reads`, `crates/foldover-endpoint/src/sftp.rs:588-628`, via
  `readers::read_each`, `crates/foldover-endpoint/src/sftp/readers.rs:46`).
  Measured: `scan_10k_files` = 2.29 s median for 10,525 entries
  (≈ 229 µs/entry) on this host.
- Hypothesis: per-entry work (two lstats, an open/read/close, xattr
  probes) is I/O- and syscall-bound and interleaves well across files;
  hashing 1M documents serially at 229 µs/entry is ≈ 4 minutes, and a
  bounded worker pool (PLAN.md §4.9 allows 4–8 workers on SSD/network)
  would cut it to 1–2 minutes. Unverified scaling factor.
- Proposed change: run the per-entry `entry_version` work in a bounded
  pool, mirroring `readers::read_each`'s design: the run's thread keeps
  recording results and asking `control` (preserving the ADR 0043
  single-recorder rule), workers only compute versions, and results land
  in path order or are sorted after collection. Keep the fingerprint
  fast path (`crates/foldover-endpoint/src/local.rs:1198-1203`) on the
  cheap serial walk so unchanged files never queue.
- Expected impact: 2–4× faster local observe on hash-heavy runs (first
  syncs, weekly verification passes, fingerprint-window expirations);
  low effect on stat-only re-scans (medium confidence).
- Effort and risk: medium; blast radius is the local observe path only.
  Risks: cancellation latency must stay bounded (the readers pattern's
  `stop` flag handles this), and the change-detection re-stat ordering
  (`crates/foldover-endpoint/src/local.rs:1073-1079`) must keep running on
  the scanning thread's observations.
- Measurement plan: rerun `cargo bench -p foldover-endpoint --bench scan`
  (and the 100k-file variant in the bench fixture) before/after on the same
  host; target is approaching the PLAN.md §5.3 budget of ≥ 10k
  entries/s cold (≈ 100 µs/entry) from today's ≈ 4.4k entries/s.

### 4. Skip extended-attribute value reads the entry version discards

- Category: redundant work and caching
- Evidence: `entry_version` always calls `optional_extended_metadata`
  (`crates/foldover-endpoint/src/local.rs:1241`, `:1258-1270`), which
  `flistxattr`s and then `fgetxattr`s and digests every attribute value
  (`crates/foldover-fs/src/root/extended.rs:170-…`, value reads at
  `extended.rs:407`). But the version digest returns early when the pair
  ignores extended attributes (`crates/foldover-endpoint/src/local/version.rs:136-138`)
  and, when it does not, keeps only `Synced` and (if quarantine travels)
  `Quarantine` roles (`crates/foldover-endpoint/src/local/version.rs:151-157`)
  plus the resource fork as content (`local/version.rs:59-67`). Values
  with ignored roles — ownership-adjacent system attributes, lock flags,
  creation dates (PLAN.md §4.10) — are read and hashed and then dropped.
- Hypothesis: on macOS volumes with several xattrs per file (Finder info,
  tags, quarantine, `com.apple.provenance`), value I/O and BLAKE3 digesting
  of attributes that never enter the version is a measurable share of the
  ≈ 229 µs/entry scan cost; for 1M documents with 3–5 xattrs each this is
  hundreds of thousands of superfluous `getxattr` syscalls per scan.
  Unverified share.
- Proposed change: in `read_extended_metadata`
  (`crates/foldover-fs/src/root/extended.rs:170`), accept a role filter (or
  a policy) and read values only for names whose `AttributeRole`
  (`metadata.attribute_role(name)`, used at `local.rs:1310-1335`) is
  `ResourceFork`, `Synced`, or `Quarantine`-when-it-travels. When the
  policy ignores extended attributes entirely
  (`MetadataPolicy::extended_attributes() == Ignore`,
  `crates/foldover-types/src/metadata_policy.rs:57-59`), skip every value
  read except `com.apple.ResourceFork`. Keep the listing
  (`flistxattr`) so unknown names still surface.
- Expected impact: 10–30% faster local scans on xattr-rich macOS volumes
  (low-medium confidence); zero effect on Linux or xattr-free corpora.
- Effort and risk: small; blast radius is the extended-metadata reader and
  its digest tests (`crates/foldover-fs/src/root/extended/tests.rs`). What
  could regress: a role classification change would silently stop tracking
  an attribute — add a test asserting every listed name is either read or
  explicitly role-ignored.
- Measurement plan: fixture of 10k files × 4 xattrs (two synced, one
  quarantine, two system); run `scan_10k_files`-style bench with
  `MetadataPolicy` variants before/after; also rerun
  `unicode…`/metadata digest regression tests in `local/version.rs`.

### 5. Raise SFTP session parallelism toward PLAN's 4–8 band and enlarge streaming chunks

- Category: concurrency and parallelism
- Evidence: `PARALLEL_COPIES = 4` and `SESSIONS = 5`
  (`crates/foldover-endpoint/src/sftp.rs:68-70`), while PLAN.md §4.9
  specifies "network 4–8 connections with pipelined requests"
  (PLAN.md:570-571). The streaming parameters are `CHUNK = 32 KiB` and
  `WINDOW = 16` (512 KiB in flight per session),
  `crates/foldover-net/src/sftp.rs:32-35`; `copy_data` (used for
  server-side staging) is strictly one request at a time
  (`crates/foldover-net/src/sftp.rs:502-550`).
- Hypothesis: small-file work is latency-bound (proposal 2's round-trip
  arithmetic), so going from 4 to 8 worker sessions is worth close to 2×
  on the round-trip portion of both observe and copy; larger chunks
  (up to 256 KiB, the common OpenSSH per-request maximum) reduce packet
  and syscall count per byte for the large-document minority and help on
  ≥ 2.5 GbE where 512 KiB in flight caps throughput at window/RTT.
  Unverified.
- Proposed change: make the session/worker count a negotiated or
  configured value defaulting to 8 for SFTP endpoints (the `Sessions` pool
  already clamps to what the server accepts,
  `crates/foldover-endpoint/src/sftp/sessions.rs:118-160`), and probe the
  read/write request size up from 32 KiB with fallback to 32 KiB on
  `SSH_FX_FAILURE`/`SSH_FX_OP_UNSUPPORTED`. Keep `WINDOW` ≥ 16 so in-flight
  bytes grow with the chunk.
- Expected impact: up to ~1.8× on the latency-bound portion of observe and
  copy for small files, plus 10–30% on large-file throughput at high link
  speeds (low-medium confidence).
- Effort and risk: low (constants, a probe, and pool tests in
  `sftp/sessions.rs`); risk is server-side connection and request-size
  limits, already mitigated by the refusal clamping and the fallback.
- Measurement plan: the proposal-2 harness at 4 vs 8 sessions, plus a
  1 GiB single-file transfer bench measuring throughput at 32 KiB vs
  probed chunk sizes against `sftp-server` and one real NAS.

### 6. Parallelize SFTP directory listings during observe

- Category: concurrency and parallelism
- Evidence: the observe walk lists directories strictly one at a time on
  one session — `while let Some(directory) = pending.pop()` then
  `self.session_unless_cancelled(control)?.read_dir(&remote)`
  (`crates/foldover-endpoint/src/sftp.rs:495-508`) — while only the
  subsequent file-content reads fan out (`observe_reads`,
  `crates/foldover-endpoint/src/sftp.rs:588-628`). Each `read_dir` is an
  OPENDIR + N×READDIR + CLOSE sequence on one wire
  (`crates/foldover-net/src/sftp.rs:267-301`).
- Hypothesis: for corpora laid out as many directories (the typical shape
  of "millions of documents"), serial listing round trips delay the start
  of the parallel read phase and cap listing throughput at one directory
  per ≈ (2 + pages) RTTs; parallel listing across the pooled sessions
  would shorten the listing phase proportionally to the session count.
  Unverified.
- Proposed change: run directory listings through the same
  `readers::read_each` pattern as file reads, or pipeline OPENDIR/READDIR
  across directories on one session (proposal 2's cross-file pipeline),
  while keeping the run's thread as the only recorder and preserving the
  "unavailable folder rules out its subtree" ordering
  (`crates/foldover-endpoint/src/sftp.rs:552-560`).
- Expected impact: listing phase of observe roughly 2–4× faster on
  wide trees (medium-low confidence); negligible on single-directory
  corpora.
- Effort and risk: medium; the subtree-suppression ordering and the
  deterministic listing-vs-checked progress reporting
  (`ObserveStep::Listed`/`Checked`) must not change.
- Measurement plan: fixture of 100k files across 10k directories on a
  local `sftp-server`; time `observe_from` before/after; assert identical
  `Observation` contents (existing proptest suites in
  `crates/foldover-endpoint/src/sftp/tests.rs`).

### 7. Replace linear unavailable/failed path scans with prefix indexes

- Category: algorithmic complexity (data structures)
- Evidence: `unavailable()` does `observation.unavailable.iter().any(…)`
  per call (`crates/foldover-endpoint/src/sftp.rs:1713-1719`) and is called
  per read (`crates/foldover-endpoint/src/sftp.rs:562`) and per entry in
  the final `retain` (`crates/foldover-endpoint/src/sftp.rs:578`).
  `Executor::below_failure` scans `self.failed` linearly per row
  (`crates/foldover-engine/src/run.rs:1707-1708`) and is called from both
  `copies` and `apply` for every row (`run.rs:1704`, `:1740`, also `:1802`).
  The local side already shows the fix: `UnavailablePaths` indexes roots in
  a `BTreeSet` and checks prefixes in O(depth)
  (`crates/foldover-endpoint/src/local.rs:438-464`).
- Hypothesis: with F unavailable roots or failed paths and N rows this is
  O(N×F) byte-prefix comparisons per run. With one broken subtree it is
  negligible; with thousands of permission-denied directories over
  millions of entries it becomes a real share of prepare/execute time
  (e.g. N = 3M, F = 5k → ≈ 1.5×10^10 comparisons). Unverified magnitude,
  verified complexity class.
- Proposed change: adopt the `UnavailablePaths` prefix-set pattern for the
  SFTP `unavailable` helper and for `Executor::failed` (store roots in a
  sorted set keyed by `name_key`, check each ancestor prefix of a path).
  Keep the same matching semantics (case/Unicode matching via `name_key`,
  `crates/foldover-engine/src/plan.rs:421-438`).
- Expected impact: removes a quadratic-ish term that only bites on
  error-heavy runs; near-zero cost otherwise (medium confidence).
- Effort and risk: small; behavior-preserving refactor with existing
  proptest coverage (`crates/foldover-engine/src/plan.rs` unavailable/
  uncertain tests).
- Measurement plan: synthesize an observation with 5k unavailable roots
  and 500k entries, time `observe_reads`'s retain and an execute pass with
  5k failures before/after (criterion microbench in
  `crates/foldover-engine/benches/plan.rs`).

### 8. Reduce allocation churn in planning, snapshots and name matching

- Category: allocation and memory layout
- Evidence: `PairRun::snapshot` clones every path and fingerprint into a
  fresh `Vec<SnapshotEntry>` (`crates/foldover-engine/src/run.rs:925-941`)
  and `replace_snapshot` re-encodes them into rows
  (`crates/foldover-state/src/pairs.rs:276-296`,
  `crates/foldover-db/src/database.rs:806-881`).
  `align_spellings` NFC-normalizes every base key and every entry key into
  new `String`s whenever names match normalized
  (`crates/foldover-engine/src/run.rs:1511-1576`) — which is always for
  SFTP sides (`crates/foldover-endpoint/src/endpoint.rs:240`).
  `name_key` allocates per call
  (`crates/foldover-engine/src/plan.rs:421-438`) and is invoked per row
  from the `protect_unavailable` / `protect_uncertain_absences` closures
  (`crates/foldover-engine/src/plan.rs:115-226`).
  `preview()` clones versions per visible row
  (`crates/foldover-engine/src/run.rs:944-987`).
- Hypothesis: at 1M+ entries these O(n) clone/normalize passes add
  seconds per run and hundreds of MB of peak RSS (the plan merge itself
  takes 55 ms per 200k rows measured; the surrounding clones roughly
  double or triple the entry-touching work). Unverified share.
- Proposed change: intern paths once per run (e.g. `Arc<[u8]>` or a
  per-run interner behind `SyncPath` handles), short-circuit `name_key`
  with an `is_nfc` check before the `nfc().collect()`, pass borrowed rows
  into `replace_snapshot` instead of re-encoding `Vec<SnapshotRow>`, and
  precompute one name key per path for the `protect_*` closures instead of
  one per row check.
- Expected impact: 10–30% of prepare-plus-commit time and a materially
  lower RSS peak at multi-million entries (low-medium confidence).
- Effort and risk: medium; touches shared vocabulary types, so blast
  radius is wide — keep `SyncPath`'s public API stable and change
  internals only. What could regress: equality/order semantics must stay
  byte-exact (plan.rs:1294 pins raw byte order).
- Measurement plan: extend `plan_100k_entries` and `db_snapshot_unchanged_100k`
  to a 1M-entry variant; track wall time and peak RSS (via
  `malloc_size`/`/usr/bin/time -l`) before/after; `cargo xtask bench-check`
  must stay within `perf/budgets.toml`.

### 9. Ship the agent with an optimized release profile

- Category: build and startup configuration
- Evidence: the workspace `Cargo.toml` defines no `[profile.release]`
  section (`Cargo.toml:1-30`), so the shipped agent builds with default
  codegen units and no LTO. Hot CPU surfaces that would benefit: BLAKE3
  hashing (measured 2.1 GiB/s in `hash_64_mib`), the plan merge
  (`plan_100k_entries`, 55 ms/200k rows), and the version/digest mixing in
  `crates/foldover-endpoint/src/local/version.rs`.
- Hypothesis: `lto = "thin"` (or `"fat"`) plus `codegen-units = 1` for the
  release profile yields single-digit to low-double-digit CPU savings on
  these loops; BLAKE3's assembly core already runs near memory speed, so
  the win concentrates in planning, serialization and hash-adjacent code.
  Unverified.
- Proposed change: add `[profile.release] lto = "thin", codegen-units = 1`
  (keep `panic = "unwind"` — the copy workers catch panics via
  `catch_unwind`, `crates/foldover-engine/src/run/copies.rs:205-215`) and
  measure; also confirm `scripts/build.sh` and `xtask` builds pick it up
  without exceeding CI build-time budgets.
- Expected impact: 3–10% end-to-end on CPU-bound phases (low confidence),
  mostly visible in scan/plan, not in network-bound copies.
- Effort and risk: trivial change; risks are longer release build times
  and, rarely, LTO-induced build issues with the UniFFI/`unsafe`-audited
  crates (`crates/foldover-ffi`, `crates/foldover-fs` bindings). The
  `cargo xtask ci` gate and `cargo deny check` must stay green.
- Measurement plan: run `cargo xtask bench-check` and the macOS Apple-target
  test job with and without the profile; compare the four hot benches
  (`hash_64_mib`, `plan_100k_entries`, `scan_10k_files`, `db_*`).

---

## Requires approval

- Proposal 1 (stat-fingerprint reuse for SFTP content) weakens the
  documented "contents and link targets are re-read" verification stance
  recorded in `docs/adr/0028-sftp-endpoints.md` and tightens PLAN.md §4.8's
  weekly content-hash pass into the sole backstop for same-size,
  same-second remote edits. The local side already accepts exactly this
  tradeoff (`crates/foldover-endpoint/src/local.rs:283-353`), and the
  reread-window spread keeps detection bounded, but it is a change to
  what "compared" guarantees and needs an owner decision (and an ADR
  update) before implementation.

## Open questions and unverified items

- All magnitudes except the four measured numbers above are hypotheses:
  no end-to-end profile of a million-document sync against a real LAN SFTP
  server was run. The round-trip arithmetic (RTT ≈ 0.3–0.5 ms, 4 workers,
  ≈ 8 serialized ops per small file) is an estimate and should be
  replaced with numbers from one real run before committing to proposal 2's
  scope.
- `scan_10k_files` measured 2.29 s on this macOS host versus the 1000 ms
  budget in `perf/budgets.toml` (calibrated on Linux, `perf/README.md`).
  Whether macOS is inherently slower here (xattr probes, stat density) or
  the budget needs re-calibration is unverified; proposal 3's target
  should be pinned after re-measuring on the budget host.
- The scratch SFTP harness ran `sftp-server` over stdio pipes with no ssh
  and no network; it bounds protocol and server overhead, not LAN
  behavior. Cipher and ssh-channel overhead on the real path
  (`ssh -s sftp`, `crates/foldover-net/src/sftp.rs:114-146`) is unmeasured.
- Unknown: how much of a real corpus is small files versus large
  documents. Proposals 2/5/6 dominate for small files; proposals 1 and 5's
  chunk sizing dominate for large ones; the ranking assumes "millions of
  documents" skews small.
- Unknown: whether the SFTP servers in scope accept more than 4–5
  concurrent connections and larger request sizes; proposal 5's probe and
  fallback handle refusal, but per-server defaults were not surveyed.
- Proposal 8's RSS claims are unmeasured; peak memory during
  `prepare_on` at several million entries has not been profiled
  (instruments/xcode-gcc not run).
- The `copy_data` server-side staging path
  (`crates/foldover-net/src/sftp.rs:502-550`) is serialized per chunk; it
  matters only for `keep_copy`/archive flows on the SFTP side and was not
  ranked for this workload. If conflict-keep copies are common in the
  corpus, revisit.
