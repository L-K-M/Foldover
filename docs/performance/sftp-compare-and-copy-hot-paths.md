# SFTP compare-and-copy hot paths

Foldover syncs folders between a Mac's disks, NAS shares, SFTP servers and
other Macs, previewing every change before it runs. This study targets the
goal of decreasing end-to-end time for the workload "comparing and copying
millions of documents to an SFTP server on the local network". The main
execution paths are: `LocalEndpoint::observe_reporting`
(`crates/foldover-endpoint/src/local.rs:1134`) and
`SftpEndpoint::observe_reporting` (`crates/foldover-endpoint/src/sftp.rs:482`)
which build both sides' observations (scan, hash, per-file version reads);
`plan_candidates` (`crates/foldover-engine/src/plan.rs:461`) which diffs the
two observations plus the base; and `PairRun::execute`
(`crates/foldover-engine/src/run.rs:997`) which applies the plan through
`transfer::copy` (`crates/foldover-endpoint/src/sftp/transfer.rs:125`) to the
SFTP upload path and commits base state in batches. Ranking rationale:
proposals are ordered by expected reduction of wall-clock time for this
workload, weighted by implementation effort and correctness risk. The two
dominant terms are (a) every comparison re-reads the entire remote corpus
over the wire (measured: warm and cold SFTP observe cost the same per file),
and (b) the upload path spends roughly eight serial round trips per small
file (measured: copy time per file falls 4.1x when going from one to four
sessions on a zero-latency server, i.e. it is latency-bound). The remaining
proposals cut state-commit, local-observe, planning and allocation costs,
which matter at millions of entries but are smaller terms.

## Method and evidence classes

**Measured facts** below come from release builds on the development Mac,
best-of-N on a shared host, produced in this study by:

1. `cargo test --release -p foldover-endpoint --test scratch_perf --
   --nocapture` (the untracked scratch harness at
   `crates/foldover-endpoint/tests/scratch_perf.rs`, `local_scan_breakdown`
   test): a temp tree of 10,000 4 KiB files in 100 folders, timings repeated
   3x, minimum reported.
2. A temporary probe harness (removed after measurement), the
   `sftp_scan_and_copy` recipe with its observe assertion fixed: 2,000
   8 KiB files in 20 folders copied via `SftpEndpoint::spawn` against
   `/usr/libexec/sftp-server -d /` (a local, zero-network-latency SFTP
   server), then observed cold (no previous snapshot) and warm (with one),
   and copied into a fresh server root with 1 and 4 threads, best-of-3.
3. `cargo bench -p foldover-engine --bench plan -- --quick`,
   `cargo bench -p foldover-fs --bench hash -- --quick`,
   `cargo bench -p foldover-state --bench db -- --quick` (criterion medians).

Everything not produced by those runs is labeled a hypothesis. Note the
zero-latency SFTP server understates network effects: on a LAN every
round trip adds roughly 0.2-0.5 ms, so latency-bound findings get worse,
not better, in production.

### Measured baseline (this study)

| Measurement | Result |
|---|---|
| Local cold observe, 10k x 4 KiB | 2.534 s = 253.4 us/file |
| Local warm observe (fingerprints reused) | 43.9 ms = 4.4 us/file |
| Local `scan_metadata` | 43.0 ms = 4.3 us/entry |
| Local `hash_file` (BLAKE3, 4 KiB) | 602 ms = 60.2 us/file |
| Local `read_extended_metadata` | 1.235 s = 123.5 us/file |
| SFTP observe cold, 2k x 8 KiB, local server | 174 ms = 87.0 us/file |
| SFTP observe warm (previous snapshot present) | 152 ms = 76.2 us/file |
| SFTP copy, 1 thread | 134.2 files/s = 7453 us/file |
| SFTP copy, 4 threads | 553.8 files/s = 1806 us/file |
| `plan_100k_entries` (criterion median) | 58.5 ms (budget 350 ms) |
| `hash_64_mib` (criterion median) | 34.3 ms (budget 60 ms) |
| `db_commit_100k` | 244.6 ms |
| `db_snapshot_unchanged_100k` | 22.8 ms |
| `db_snapshot_rewrite_100k` | 141.9 ms |

The SFTP warm-observe number is the key fact: supplying a previous
snapshot changes per-file cost by only 12 percent, because the version
computation reads the file's content before the snapshot is consulted
(proposal 1).

## Ranked proposals

1. Reuse stat-unchanged SFTP versions instead of re-reading every remote file
2. Collapse the upload path's per-file round trips
3. Pipeline several small files' requests per session
4. Parallelize the SFTP directory-listing walk
5. Batch base commits and stop per-batch identity re-reads
6. Skip extended-metadata work the metadata policy cannot observe
7. Linear-time plan ancestor resolution and one NFC pass per path
8. Stop cloning unchanged snapshot rows before the database diff
9. Release profile: LTO and single codegen unit for the agent and CLI

---

### 1. Reuse stat-unchanged SFTP versions instead of re-reading every remote file

- Category: redundant work and caching
- Evidence: `crates/foldover-endpoint/src/sftp.rs:631-658` (`observe_entry`)
  computes `self.version(...)` — for files a full content read and BLAKE3
  hash via `read_range_on` (`crates/foldover-endpoint/src/sftp.rs:750-805`) —
  before it consults `previous.get(path)`; the snapshot comparison at
  `sftp.rs:645-653` therefore only avoids rebuilding the entry, never the
  read. `crates/foldover-endpoint/src/sftp/readers.rs:1-5` states the
  current stance: "SFTP stat cannot prove a file unchanged, so an
  observation reads every file". Measured: SFTP observe costs 87.0 us/file
  cold and 76.2 us/file warm (2,000 x 8 KiB, local server) — warm reuse
  saves almost nothing. The machinery to do better already exists:
  `Fingerprint::from_key` / `same_stat` (`crates/foldover-endpoint/src/local.rs:303-328`)
  are built from the SFTP stat key (`stat_key`, `sftp.rs:1735-1745`), and the
  local side already skips reads for settled, stat-unchanged entries with a
  bounded reread window (`proves_unchanged_by`, `local.rs:328-336`,
  `RACY_WINDOW`/`REREAD`, `local.rs:274-283`).
- Hypothesis: for this workload most documents are unchanged between runs,
  so comparison time is currently O(corpus bytes) transferred over the wire
  instead of O(entries) stat calls. Reusing the stored version when the SFTP
  stat key (size, mtime, mode, owner) is identical to the fingerprint and the
  fingerprint is within its reread window would remove nearly all content
  reads from steady-state comparisons. The cost is a weaker change
  detection: without an inode in the stat key, a file swapped for different
  content with identical size, second-granularity mtime, mode and owner
  would go unnoticed until the reread window forces a read — the same
  trade-off the local side already accepts (PLAN.md §4.8) but the SFTP
  driver currently refuses.
- Proposed change: in `observe_entry` (`sftp.rs:631`), check
  `previous.get(path)` first: if
  `known.fingerprint.same_stat(&stat_key(attributes), size)` and the
  fingerprint's `observed_at` is within its `reread_after` window (mirroring
  `local.rs:328-336`, including the 3 s `RACY_WINDOW` for coarse timestamps),
  return `known.clone()` without computing `version`. Otherwise fall through
  to the existing full read. Gate the behavior behind a per-pair option
  (e.g. `CompareStyle::StatReuse` vs `CompareStyle::ReadAll`, defaulting to
  `ReadAll` until the semantics are ratified) so existing pairs keep today's
  guarantee. Persist fingerprints for SFTP observations exactly as local
  ones are persisted today (they already round-trip through
  `Fingerprint::to_bytes`/`from_bytes`, `local.rs:347-377`).
- Expected impact: comparisons of an unchanged corpus drop from
  transferring every byte (e.g. 1M documents averaging 50 KB = 50 GB, about
  8-10 minutes at 1 GbE, plus hashing) to stat calls only (the listing walk;
  minutes to seconds). Rough magnitude: 10x-100x faster steady-state
  comparison phase, which for backup-style reruns is most of end-to-end
  time. Confidence: high that the read cost disappears (measured warm/cold
  parity), medium that the stat-reuse semantics are acceptable.
- Effort and risk: medium (one decision point, fingerprint plumbing,
  tests); blast radius is the SFTP observation path only. Risk: weaker
  change detection as above. **Requires design sign-off**: it relaxes the
  documented "SFTP versions are never reused unread" stance
  (`crates/foldover-endpoint/src/sftp/readers.rs:3-4`); see the sign-off
  section below.
- Measurement plan: rerun the SFTP observe probe (2,000 files, then
  100,000) with a previous snapshot: before change warm ≈ cold (76-87
  us/file); after change warm should fall to roughly the listing-only cost
  (<10 us/file on the local server). Confirm on a real LAN with
  `time foldover-cli preview` over a 1M-document pair with no changes.

### 2. Collapse the upload path's per-file round trips

- Category: I/O and batching
- Evidence: for one small file create, `transfer::copy` to an SFTP target
  issues, in order: an absent-check `lstat` (`receiving_on`,
  `crates/foldover-endpoint/src/sftp/transfer.rs:419-443`), `create_new`,
  one `WRITE`, `close` (`write_new`, `crates/foldover-endpoint/src/sftp.rs:1672-1688`),
  a second `lstat` plus a `SETSTAT` (`set_metadata_on`, `sftp.rs:940-978`),
  a third `lstat` (`staged_version_on`, `sftp.rs:921-933`), and a `RENAME`
  (`place_on`, `sftp.rs:1034-1105`) — about 8 serial request/response
  exchanges per file. `Session::request` is strictly one-at-a-time
  (`crates/foldover-net/src/sftp.rs:689-696`). Measured: 7453 us/file with
  one session, 1806 us/file with four (8 KiB files, zero-latency server);
  the 4.1x speedup from 4 sessions shows the per-file cost is dominated by
  waiting on sequential exchanges, not by bytes. `set_handle_attributes`
  (`crates/foldover-net/src/sftp.rs:553-564`, `FSETSTAT`) is already
  implemented but used only by tests.
- Hypothesis: on a LAN each exchange costs at least one RTT (0.2-0.5 ms) on
  top of server work, so at least half of the 1.8 ms/file four-thread cost
  is round-trip waiting that folding requests would remove. The metadata
  round trips are also individually redundant: the mode is chosen locally,
  and one post-write read-back suffices for "record what the target stored"
  (PLAN.md §4.10).
- Proposed change: inside `write_new`/`upload_on`, set permissions and
  modification times with `FSETSTAT` on the open handle before `close`
  (merging the `set_metadata_on` lstat+SETSTAT pair into the write
  sequence); skip `set_metadata_on`'s leading `lstat` when the policy sets
  both permissions and times directly (both values are known from the plan);
  keep exactly one read-back `lstat` — reuse `staged_version_on`'s — for the
  recorded-reality version, and issue the trailing `RENAME` before waiting
  on that read-back where the protocol allows (the rename target is already
  computed). Target: 8 exchanges down to 5-6 for the create path; the
  replace path similarly folds its verification lstat into the archive
  rename sequence.
- Expected impact: 20-35 percent lower per-file copy latency at LAN RTT
  (measured local baseline 134 files/s single-threaded suggests roughly
  175-190 files/s after folding at zero latency, more on a LAN where RTT
  dominates). Confidence: medium.
- Effort and risk: medium; blast radius is `transfer::send_on` and
  `SftpEndpoint` staging helpers. Risk: a lost response must still be
  handled as today (`Self::answered` distinctions in `sftp.rs`); the
  recorded-reality version must come from a server read-back, not from what
  we asked for. Regression coverage: the existing SFTP endpoint tests
  (`crates/foldover-endpoint/src/sftp/tests.rs`) plus a request-counting
  harness (cf. the untracked `crates/foldover-endpoint/tests/sftp_trace.rs`
  recipe) asserting ≤6 requests per created small file.
- Measurement plan: extend `perf/budgets.toml` with a
  `sftp_copy_2k_small` criterion bench (2,000 x 8 KiB creates against
  `/usr/libexec/sftp-server`, 4 threads); compare median before/after;
  separately count wire requests per file and require a drop from ~8 to
  ≤6.

### 3. Pipeline several small files' requests per session

- Category: concurrency and parallelism
- Evidence: streaming reads and writes already keep a window in flight
  (`WINDOW = 16`, `CHUNK = 32 KiB`, `crates/foldover-net/src/sftp.rs:31-35`;
  `read_from`/`write_chunks`, `sftp.rs:339-500`) and the session can track
  out-of-order replies (`inflight`/`stashed`, `sftp.rs:100-107`,
  `send_for`/`receive_for`, `sftp.rs:696-766`), but every metadata request
  (OPEN, CLOSE, LSTAT, SETSTAT, RENAME) goes through the synchronous
  `request` (`sftp.rs:689-696`) and the whole run holds one exchange at a
  time per file. Measured latency-bound per-file cost (7453 us one thread
  vs 1806 us four threads, zero-latency server) confirms serial waiting
  dominates.
- Hypothesis: interleaving the request sequences of several small files on
  one session — send `OPEN`+`WRITE`+`CLOSE` for files A, B, C, then collect
  replies — moves the per-file cost from "8 x RTT" toward "8 x RTT / K"
  for K in flight, roughly halving to quartering copy time for small files
  at LAN RTT without opening more connections. The wire protocol and the
  session's stashing machinery already support it.
- Proposed change: add a batched variant of the staging sequence in
  `foldover-net::sftp::Session` (e.g. `pipeline(files: &[Prepared])`) that
  sends a fixed request script for K files and drains replies in order,
  reusing `send_for`/`receive_for`; drive it from `Copies`
  (`crates/foldover-engine/src/run/copies.rs`) workers so each worker hands
  the session a small batch (K = 4-8) instead of a single row. Keep the
  per-file verification semantics: any file whose reply sequence deviates
  (changed destination, short write) falls back to the serial path for that
  file. Ordering invariant "a copy is handed out only after the folder it
  goes into was made" (`run/copies.rs:1-9`) is preserved by batching only
  rows whose parent folders already exist.
- Expected impact: 30-50 percent lower per-file copy latency at LAN RTT on
  top of proposal 2 (low-medium confidence); for millions of small
  documents this is tens of minutes to hours saved per full copy.
  Confidence: low-medium (server scheduling behavior under pipelining
  unverified).
- Effort and risk: high; blast radius is the SFTP wire driver and the copy
  dispatcher. Risks: reply-order bookkeeping (already solved by `stashed`),
  error isolation per file, cancellation latency (keep the 100 ms control
  poll, `run/copies.rs:33`). Reject the change if the server under test
  (OpenSSH `sftp-server`, the device matrix's SFTP leg) shows throughput
  regression under pipelined metadata.
- Measurement plan: the `sftp_copy_2k_small` bench from proposal 2 at
  1 vs 4 workers, with and without K-wide pipelining, on both the local
  server and a LAN host; also measure cancellation latency during a large
  copy to confirm the control poll still fires within ~200 ms.

### 4. Parallelize the SFTP directory-listing walk

- Category: concurrency and parallelism
- Evidence: `SftpEndpoint::observe_reporting`
  (`crates/foldover-endpoint/src/sftp.rs:482-586`) walks directories
  strictly one at a time on one borrowed session:
  `while let Some(directory) = pending.pop() { ... self.read_dir(&remote) ... }`
  (`sftp.rs:494-510`). Each `read_dir` is OPENDIR + one-or-more READDIR +
  CLOSE (`crates/foldover-net/src/sftp.rs:267-301`). Only the later
  per-file read phase is parallelized (`observe_reads`,
  `sftp.rs:588-629`, over `workers()` sessions = 4,
  `sftp.rs:68-70`).
- Hypothesis: for millions of documents spread across many folders, the
  listing phase is O(folders) serial round trips — e.g. 100,000 folders x
  ~3 exchanges x 0.3 ms LAN RTT ≈ 90 s of pure waiting that a bounded
  parallel walk would cut by the pool width. Folder-heavy trees are the
  norm for document corpora.
- Proposed change: restructure the listing loop as a bounded parallel walk
  over the session pool, modeled on `readers::read_each`
  (`crates/foldover-endpoint/src/sftp/readers.rs:45-63`): a work queue of
  pending directories, up to `workers()` sessions each issuing `read_dir`,
  the calling thread still doing the filter/sidecar/progress accounting so
  output determinism is preserved (the issues sort at `sftp.rs:624-629`
  already normalizes ordering). Extend `readers` or add a sibling
  `listers` module with the same stop-on-panic and control semantics.
- Expected impact: 2-4x faster listing phase on folder-heavy trees
  (medium confidence); on the 2,000-file probe the listing share is small,
  but at millions of documents across deep trees it is a meaningful
  fraction of the compare phase.
- Effort and risk: medium; blast radius is the SFTP observe path only.
  Risks: nondeterministic entry arrival order (mitigated by sorting as
  today), cancellation behavior, and duplicate work when two queued
  directories share a parent that fails (deduplicate by path, mark failed
  subtrees `unavailable` exactly as `sftp.rs:521-540` does).
- Measurement plan: a bench that observes a synthetic remote tree of
  20,000 empty folders x 20 files over the local server (and on a LAN):
  before ≈ folders x 3 exchanges serial; after should scale with session
  count. Count `read_dir` wall time via `ObserveStep::Listed` progress
  timestamps.

### 5. Batch base commits and stop per-batch identity re-reads

- Category: I/O and batching
- Evidence: `PairRun::execute` commits base state every `COMMIT_BATCH = 256`
  rows (`crates/foldover-engine/src/run.rs:52`, batch check at
  `run.rs:1233-1244`) and calls `check_roots(identities)` on the same
  cadence (`run.rs:1236-1237`, defined `run.rs:1378-1399`), which re-reads
  each endpoint's identity file — over SFTP that is a small-file read
  (`identity` → `read_small`, `crates/foldover-endpoint/src/sftp.rs:315-327`,
  `sftp.rs:1642-1668`) = ~3 exchanges per side per batch. Each batch is its
  own `change_base` SQLite transaction with `synchronous = FULL`
  (`crates/foldover-db/src/database.rs:647`, `change_base`
  `database.rs:945-996`). Measured: `db_commit_100k` median 244 ms for a
  100k-entry bulk commit; per-transaction overhead at 256 rows is
  unverified but the transaction count is not: 1M copied files ⇒ ~3,900
  transactions and ~7,800 identity reads mid-run.
- Hypothesis: at millions of copied files the per-batch fsync'd transaction
  and the identity re-reads add fixed per-batch latency (hypothesis: tens of
  seconds to a couple of minutes per 1M files at LAN RTT) that a larger or
  time-based batch removes without weakening crash recovery, because every
  op is idempotent and the journal drains on restart (PLAN.md §5.1
  invariant 3), and the root-unmount detection does not need 256-row
  granularity.
- Proposed change: make the commit cadence time- or byte-based (e.g. commit
  when ≥256 rows *and* ≥250 ms have passed, or when the pending journal
  exceeds ~1 MiB), keeping the current mandatory final commit; move the
  `check_roots` identity verification to the lease-beat cadence (it already
  runs per lease beat through `leases.check`, `run.rs:1129-1135`) instead
  of every row batch, with an additional check every N rows (e.g. 16,384)
  to bound the unverified window. Keep the "landed copies journaled before
  failure" ordering (`run.rs:1253-1260`) exactly as is.
- Expected impact: for a 1M-file copy, removal of ~7,800 remote identity
  reads (≈30-60 s at 0.3 ms RTT each with 3 exchanges, hypothesis) and a
  ~15x reduction in transaction count; overall low single-digit percent of
  a long copy, larger for fast LAN copies of tiny files. Confidence:
  low-medium (per-transaction cost unmeasured).
- Effort and risk: small-medium; blast radius is `PairRun::execute` and
  `PairStore::change_base` call cadence. Risk: widening the window where a
  yanked volume is not noticed (mitigated by lease beats and the bounded
  row check); commit batching must not reorder journal application
  (`journal::apply` stays inside each transaction).
- Measurement plan: instrument a 1M-row simulated run
  (`cargo test -p foldover-engine --release -- simulation`) counting
  transactions and identity reads, before/after; plus the existing
  `db_commit_100k` budget unchanged (it measures the bulk path) and a new
  `db_commit_256x400` bench capturing the per-batch overhead that the
  change removes.

### 6. Skip extended-metadata work the metadata policy cannot observe

- Category: redundant work and caching
- Evidence: `entry_version` always reads extended metadata before hashing
  (`crates/foldover-endpoint/src/local.rs:1241`), and
  `read_extended_metadata` → `capture`
  (`crates/foldover-fs/src/root/extended.rs:273-299`) reads the attribute
  name list twice (`extended.rs:277`, `extended.rs:296`), probes every
  attribute value size twice (`extended.rs:281`, `extended.rs:285`), and
  always reads the ACL (`extended.rs:294`) — although the ACL only enters
  the version when the policy syncs permissions
  (`crates/foldover-endpoint/src/local/version.rs:108-115`) and attribute
  values only when the policy syncs extended attributes
  (`version.rs:136-139`); only the resource fork is unconditionally needed,
  because it is content (`version.rs:59-77`). Measured:
  `read_extended_metadata` costs 123.5 us/file — more than twice
  `hash_file` (60.2 us/file) — of the 253.4 us/file cold local observe.
- Hypothesis: for a default or content-focused policy, a large share of
  those syscalls produces data the version digest ignores; skipping them
  cuts the local comparison phase by roughly a fifth for cold entries and
  leaves the digest bit-identical for every policy setting.
- Proposed change: pass `MetadataPolicy` into `read_extended_metadata` and
  thread it to `capture`: (a) when `policy.permissions() == Ignore`, skip
  `access_control(file)`; (b) when `policy.extended_attributes() == Ignore`,
  read the name list once and fetch only `com.apple.ResourceFork`; (c)
  always drop the duplicate name-list read and the second size probe,
  validating change via the existing before/after `EntryMetadata` checks
  (`extended.rs:177-197`) instead. Keep limit and cancellation behavior
  (`MAX_ATTRIBUTE_*`, `extended.rs:12-16`) unchanged.
- Expected impact: 30-60 percent off the 123.5 us/file extended-metadata
  cost; cold local observe from 253 us/file to roughly 180-210 us/file on
  the measured tree (4-8 percent end-to-end for a compare-heavy run that
  must cold-scan the local side). Confidence: medium on the per-call
  savings, medium on the aggregate share.
- Effort and risk: small; blast radius is `foldover-fs` extended metadata
  and `entry_version`. Risk: a policy that syncs xattrs must behave
  exactly as today — cover with the existing metadata tests
  (`crates/foldover-fs/src/root/extended` tests) plus a test asserting the
  digest is identical before/after for each policy combination.
- Measurement plan: rerun the `local_scan_breakdown` recipe with
  `read_extended_metadata` timed separately under
  `MetadataPolicy::new(Sync, Ignore, Ignore)` vs the default; require the
  ignore-policy cost to fall below ~60 us/file and all `hash_64_mib` /
  metadata digest tests to stay green.

### 7. Linear-time plan ancestor resolution and one NFC pass per path

- Category: algorithmic complexity
- Evidence: `plan_resolved` walks each row's ancestor chain with per-level
  `BTreeMap` lookups in two passes — the deleted-versus-edited loop
  (`crates/foldover-engine/src/plan.rs:574-595`) and the blocked-by-ancestor
  loop (`plan.rs:603-621`) — giving O(n x depth x log n) for n rows despite
  the rows already being in path order ("Ancestors precede descendants",
  `plan.rs:603`). Separately, `align_spellings`
  (`crates/foldover-engine/src/run.rs:1511-1540`) recomputes an NFC-composed
  `String` per path (`composed`, `run.rs:1519`) in two full passes over the
  base and the observation. Measured: `plan_100k_entries` median 58.5 ms
  for 100k-entry sides (so roughly 0.6-1.2 s at 1M paths, extrapolated);
  the repo's own Unicode budget concedes up to 5 s for 50,000 respelled
  names (`perf/README.md`, `unicode_respelling_prepare_budget`).
- Hypothesis: at multi-million-path corpora the plan and alignment passes
  become noticeable fractions of the compare phase and allocate heavily;
  a single forward pass that maintains the nearest-ancestor state as rows
  are emitted (rows are sorted, ancestors first) removes the repeated
  parent walks, and computing each path's NFC key once into a reused map
  removes the repeated normalization allocations.
- Proposed change: in `plan_resolved`, replace both ancestor-walk loops
  with one forward pass keeping a small stack of (directory path, row
  index, conflict/blocked state); a row inherits its parent's blocked root
  from the stack top instead of re-walking parents. In `align_spellings`,
  compute `composed(path)` once per path into
  `BTreeMap<&SyncPath, String>` shared by both passes (or key the `missing`
  map by the composed form while building it in one pass over base and one
  over observation entries). Keep the exact semantics of
  `CandidateDecision::BlockedByAncestor` and the ambiguous-respelling rule
  (`run.rs:1553-1560`).
- Expected impact: 30-50 percent lower plan-phase CPU at 1M+ paths
  (low-medium confidence), plus a measurable allocation drop; end-to-end
  this is single-digit percent except for very large comparisons where
  planning currently takes seconds on the run thread.
- Effort and risk: medium; blast radius is `plan.rs` and one function in
  `run.rs`, both covered by extensive tests
  (`crates/foldover-engine/src/plan.rs:757+`,
  `unicode_respelling_prepare_budget`). Risk: subtle change to
  conflict-suppression propagation — mitigate by keeping the existing test
  scenarios and adding a differential test (old vs new decisions on seeded
  trees).
- Measurement plan: `cargo bench -p foldover-engine --bench plan -- --quick`
  plus a new `plan_2m_entries` bench; compare medians and peak allocation
  (e.g. `dhat` or `heaptrack` on the bench binary). The stored budget
  `plan_100k_entries = 350` must not regress.

### 8. Stop cloning unchanged snapshot rows before the database diff

- Category: allocation and memory layout
- Evidence: `PairStore::replace_snapshot`
  (`crates/foldover-state/src/pairs.rs:276-296`) builds a
  `Vec<SnapshotRow>` that clones every entry's path, fingerprint and
  encoded version (`pairs.rs:279-287`) before handing it to
  `Database::replace_snapshot`, whose merge explicitly borrows unchanged
  rows to keep them allocation-free ("Borrowed blobs keep an unchanged row
  allocation-free", `crates/foldover-db/src/database.rs:822-880`). The
  upstream cloning defeats that design for every entry, not just changed
  ones. Measured: `db_snapshot_unchanged_100k` median 22.8 ms per 100k
  entries per side, i.e. the no-change path already costs real time and
  allocation at 1M entries x 2 sides each run.
- Hypothesis: at millions of entries the pre-clone churn (hundreds of MB of
  short-lived `Vec<u8>`s per run end) dominates the unchanged-path cost and
  pressures peak memory (PLAN.md §5.3 budgets 500 MB at 1M tracked files);
  borrowing the caller's `SnapshotEntry` slice end to end cuts both.
- Proposed change: change `Database::replace_snapshot` to accept rows as
  borrowed structs (path/fingerprint/version as `&[u8]`) — the merge loop
  already only needs borrowed blobs — and have `PairStore::replace_snapshot`
  map `&SnapshotEntry` into those borrowed rows (encoding versions into one
  reusable scratch buffer, or encoding lazily only for added/changed rows
  as the merge identifies them). No schema or semantic change.
- Expected impact: 30-50 percent lower time and a large allocation drop on
  the unchanged-snapshot path (low confidence on time, high on allocations);
  tens of seconds saved per run end at multi-million entries. Confidence:
  low-medium.
- Effort and risk: small-medium; blast radius is `foldover-state` and
  `foldover-db` snapshot APIs (internal crates). Risk: borrow lifetimes in
  the merge loop; keep `db_snapshot_unchanged_100k` and
  `db_snapshot_rewrite_100k` budgets green.
- Measurement plan: `cargo bench -p foldover-state --bench db -- --quick`
  comparing `db_snapshot_unchanged_100k` before/after, plus a 1M-entry
  variant and a peak-allocation measurement of `replace_snapshot`.

### 9. Release profile: LTO and single codegen unit for the agent and CLI

- Category: build and startup configuration
- Evidence: the workspace manifest (`Cargo.toml`) defines no
  `[profile.release]` section, so release builds use the defaults
  (`codegen-units = 16`, no LTO); the hot CPU paths (BLAKE3 hashing,
  planning, version encoding) are compiled under those defaults. Measured
  medians under the current profile: `hash_64_mib` 34.3 ms,
  `plan_100k_entries` 58.5 ms (both within budget).
- Hypothesis: enabling `lto = "thin"` (or `"fat"`) and
  `codegen-units = 1` for the release profile yields 5-15 percent on
  CPU-bound phases (hashing loops, plan iteration, snapshot encode/decode)
  and slightly faster startup for `foldoverd`/`foldover-cli`, at the cost
  of longer builds. The magnitude is a hypothesis; BLAKE3 uses SIMD
  assembly so hashing gains may be at the low end.
- Proposed change: add to the workspace `Cargo.toml`:

  ```toml
  [profile.release]
  lto = "thin"
  codegen-units = 1
  ```

  Verify `cargo xtask ci` and `scripts/build.sh core` still pass and the
  benchmarks move as expected before keeping it; drop it again if the
  median changes are under ~3 percent, since build-time cost would then not
  pay off.
- Expected impact: 5-15 percent on CPU-bound benchmarks and small
  end-to-end gains (low confidence); this is the lowest-ranked proposal
  because the workload is dominated by network and syscall latency, not
  compiled-code speed.
- Effort and risk: trivial; blast radius is build times (CI wall clock) and
  binary size. No runtime behavior change expected.
- Measurement plan: `cargo xtask bench-check` before and after on an idle
  host; keep the change only if at least two of `hash_64_mib`,
  `plan_100k_entries`, `db_commit_100k` improve by ≥3 percent with none
  regressing.

---

## Requires design sign-off

- **Proposal 1 (stat-unchanged version reuse for SFTP)** relaxes the
  documented stance that "SFTP stat cannot prove a file unchanged, so an
  observation reads every file"
  (`crates/foldover-endpoint/src/sftp/readers.rs:3-4`) and the exactness
  implied by PLAN.md §4.8 ("same size, same mtime, same file ID means
  unchanged; anything else is hashed" — SFTP stat keys carry no file ID,
  `crates/foldover-endpoint/src/sftp.rs:8`). The proposal mitigates this
  with the local side's existing bounded-reread policy
  (`crates/foldover-endpoint/src/local.rs:274-283`) and an opt-in default,
  but the semantic change is the owner's call. No proposal in this
  document breaks a stated constraint (the constraints block was empty).

## Open questions and unverified items

- Per-file request counts (~8 per small create) are derived from code
  reading (`transfer.rs:446-531`, `sftp.rs:940-978`, `sftp.rs:921-933`,
  `sftp.rs:1034-1105`), not from a wire trace; confirming them with a
  packet counter (the `sftp_trace.rs` recipe) is the first step of
  proposal 2.
- All SFTP timings used a zero-latency local `sftp-server`; real LAN RTT
  and real server scheduling (OpenSSH on Linux, NAS-embedded servers) are
  unverified and could change the ranking between proposals 2 and 3.
- The warm SFTP observe probe confirmed warm ≈ cold per-file cost on a
  2,000-file tree; whether some other layer caches content for larger
  corpora was not tested (no such cache exists in the code as read).
- Per-transaction cost of `change_base` at 256 rows (proposal 5) was not
  isolated; `db_commit_100k` measures the bulk path only.
- Proposal 3's pipelining behavior against servers other than OpenSSH
  `sftp-server` (device matrix SFTP leg, NAS appliances) is unverified;
  it must not ship without that matrix run.
- The local probe measured 8 KiB-class and 4 KiB-class files; very large
  documents (tens of MB) are dominated by bandwidth and the existing
  32 KiB x 16 window (`crates/foldover-net/src/sftp.rs:31-35`), which was
  not tuned here — chunk-size tuning may deserve a separate study for
  large-file-heavy corpora.
- Peak memory at multi-million entries (proposals 7-8) was not profiled;
  PLAN.md §5.3's 500 MB budget at 1M files was used as the reference point.
- Proposal 9's LTO gains are entirely hypothetical until measured; BLAKE3
  assembly may make them negligible.
