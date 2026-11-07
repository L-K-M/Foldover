# Performance proposals: end-to-end sync time against an SFTP server on the LAN

Foldover is a macOS app over a portable Rust engine (`crates/`) that syncs folder
pairs between local disks, NAS shares, SFTP servers and other Macs. A run has three
phases over the wire for an SFTP side: **observe** (recursive `READDIR` plus a full
content read and BLAKE3 hash of every file, `crates/foldover-endpoint/src/sftp.rs`
`observe_reporting`/`observe_reads`/`version_on`/`read_range_on`), **plan**
(`crates/foldover-engine/src/plan.rs`), and **apply** (per-file staged upload,
metadata set, archive rename and place, driven by `crates/foldover-engine/src/run.rs`
with `PARALLEL_COPIES` copy workers on `SESSIONS` pooled SSH sessions, all speaking
the SFTP v3 wire protocol in `crates/foldover-net/src/sftp.rs`, one `ssh` subprocess
per session). The workload studied here is comparing and copying millions of
documents to an SFTP server on the local network, where the primary metric is
end-to-end wall-clock time of a sync/backup run. The dominant cost is not raw
bandwidth but **round trips and bytes moved per file**: with millions of documents,
even 5–10 sequential SFTP requests per file at LAN round-trip latency, times four
workers, dominates. The proposals below are ranked by expected impact on that
metric, weighted by effort and risk. Everything cited was read in the source at
commit `c997ae9`; nothing was profiled on a live server in this pass, so impact
magnitudes are hypotheses to be confirmed by the measurement plans, not measured
facts.

## Ranked proposals

1. Skip the full-file re-hash in SFTP scans when the stat fingerprint is unchanged
2. Raise the SFTP copy parallelism (`PARALLEL_COPIES`) for small-file workloads
3. Verify moves/replacements/removals by stat key instead of a full re-read
4. Pipeline `READDIR` requests when listing large directories
5. Raise the SFTP chunk size and in-flight window for streaming transfers
6. Cut per-file round trips in the place/archive path via the listings cache
7. Amortize the per-batch root identity re-reads during the apply loop
8. Tune the release build profile (LTO, codegen-units)

---

### 1. Skip the full-file re-hash in SFTP scans when the stat fingerprint is unchanged

- **Category:** redundant work and caching
- **Evidence:** `crates/foldover-endpoint/src/sftp.rs:631-659` — `observe_entry` computes `self.version(...)` at line 642 (which for files calls `version_on`, sftp.rs:681-712, which calls `read_range_on`, sftp.rs:750-806, reading the **entire file** over SFTP and hashing it, plus a trailing `lstat` at sftp.rs:797-803) and only *afterwards*, at line 647, consults `previous` to decide whether the stored observation can stand. The reader pool's own doc comment admits the consequence: `crates/foldover-endpoint/src/sftp/readers.rs:1-6` — "SFTP stat cannot prove a file unchanged, so an observation reads every file". The local endpoint does the opposite ordering: `crates/foldover-endpoint/src/local.rs:1198-1204` checks `previous.get(&entry.path)` with `fingerprint.proves_unchanged(&entry.metadata)` **before** hashing, and `Fingerprint::proves_unchanged_by` (local.rs:330-340) already implements the racy-window and re-read-staleness logic (`RACY_WINDOW` local.rs:283, `REREAD` local.rs:288) that makes stat-based reuse safe against the classic same-mtime-same-size race.
- **Hypothesis:** on every sync after the first, the SFTP-side scan re-reads and re-hashes 100% of file bytes over the network even though size+mtime(mode)+size stat keys are unchanged for the overwhelming majority of files in a document corpus. For millions of documents this is likely the single largest term in end-to-end time: it is pure network I/O that the local endpoint already avoids.
- **Proposed change:** in `SftpEndpoint::observe_entry`, check `previous.get(path)` with `known.fingerprint.same_stat(&stat_key(attributes), size)` (sftp.rs:647 already builds the key) **before** calling `version()`, returning `known.clone()` on a hit, mirroring local.rs:1198. To keep the same-second-same-size safety the current code buys by re-reading, reuse the local `Fingerprint` semantics: skip only when the fingerprint's `observed_at` is older than `RACY_WINDOW` relative to the listed mtime and within `reread_after()` — i.e. port `proves_unchanged_by` to the SFTP fingerprint (`Fingerprint::from_key`, local.rs:317-328, already stores exactly the stat key + size + observed_at that SFTP has). Optionally gate the behavior behind a pair option so conservative users can keep always-re-read.
- **Expected impact:** on steady-state runs over a mostly-unchanged tree, scan-phase network bytes drop from ~100% of corpus size to ~0%, so scan time falls from "read everything at LAN throughput" to "directory listings only" — plausibly a 30-60% reduction of total end-to-end sync time for a million unchanged documents, and near-zero cost on first sync. Confidence: **high** that the redundant read happens (verified in code), **medium** on the end-to-end magnitude (depends on the changed-file fraction).
- **Effort and risk:** medium — reorder a check and port the racy-window predicate; blast radius is `observe_from` for SFTP roots only. Regression risk: a file modified within the same whole second with identical size would be missed — the exact risk local roots already accept, but for SFTP it should be explicit and possibly configurable.
- **Measurement plan:** against a fixture SFTP server (the harness in `crates/foldover-net/src/sftp/tests.rs` spawns a real `sftp-server`), build a 100k-file tree, run `observe_from` once to populate `previous`, then time a second `observe_from` and count SFTP `READ` requests/bytes (instrument `Session::read_from` or read `target/criterion`-style counters) before and after the change. Secondary: end-to-end `foldover-cli` sync of the same tree twice, wall-clock of the second run.

### 2. Raise the SFTP copy parallelism (`PARALLEL_COPIES`) for small-file workloads

- **Category:** concurrency and parallelism
- **Evidence:** `crates/foldover-endpoint/src/sftp.rs:68` — `PARALLEL_COPIES = 4`, and `:70` `SESSIONS = PARALLEL_COPIES + 1`. `crates/foldover-engine/src/run.rs:1430-1444` — `workers()` is the min of both sides, and SFTP↔SFTP pairs are forced to 1. Each copy of a small file is a fixed chain of sequential round trips (open/write/close/fsetstat/lstat/rename(s) — see proposal 6), so throughput for millions of small documents scales almost linearly with worker count up to the server's `MaxSessions`. The session pool already clamps adaptively when a server refuses connections (`crates/foldover-endpoint/src/sftp/sessions.rs`, `REFUSALS`/`RETRY` logic, lines ~28-33 and `acquire`), so raising the constant is safe against servers that cap sessions.
- **Hypothesis:** with ~8-10 sequential round trips per small file and 4 workers, per-file latency is ~(8-10 × RTT)/4; doubling workers to 8 roughly halves the apply phase for a first sync of millions of small documents on a LAN, where RTT is small but request count is enormous.
- **Proposed change:** make `PARALLEL_COPIES` configurable per pair (default 8, PLAN.md §4.9 already sanctions "network 4–8 connections"), keeping the adaptive clamp in `Sessions::acquire` as the guard. For SFTP↔SFTP pairs, keep the forced single worker (the deadlock rationale in run.rs:1430-1436) or re-evaluate with per-direction sessions.
- **Expected impact:** up to ~2x on the apply phase for small-file-heavy first syncs if the server allows more sessions; proportionally less for large-file transfers that are bandwidth-bound. Confidence: **medium** (server `MaxSessions` and single-`ssh`-process contention may cap gains).
- **Effort and risk:** small — a constant plus a config field; risk is server-side load and more memory per session (each session is an `ssh` subprocess with a 1 MiB max-packet buffer, `crates/foldover-net/src/sftp.rs:39`).
- **Measurement plan:** fixture SFTP server with configurable `MaxSessions`, 100k small files, time `foldover-cli` first sync at `PARALLEL_COPIES` = 4 vs 8 vs 16; also record server-side session count and any refusal-driven clamping in `Sessions`.

### 3. Verify moves/replacements/removals by stat key instead of a full re-read

- **Category:** redundant work and caching (I/O reduction)
- **Evidence:** `verified_at_on` (`crates/foldover-endpoint/src/sftp.rs:823-837`) calls `version_on` (sftp.rs:681-712), which for files runs `read_range_on` (sftp.rs:750-806) — a **full read + hash of the file over SFTP** — just to check the entry still matches the expected version. It is called from `move_entry` (sftp.rs:1107-1165), `remove` (sftp.rs:1166+), and from `receiving_on` with `Expected::Present` (`crates/foldover-endpoint/src/sftp/transfer.rs:419-444`), i.e. before every replacement copy into an SFTP destination. Note `move_entry` then *also* does a stat-key `still` check after its directory creation (sftp.rs:1152-1157), showing stat-key comparison is already accepted in this file.
- **Hypothesis:** for every moved, removed, or replaced file, Foldover currently downloads and hashes the whole file over SFTP to answer "did it change since the scan?". The scan happened seconds earlier in the same run, so the expected version is fresh; a single `lstat` (one round trip) comparing `stat_key` detects size/mtime/mode changes at a fraction of the cost. For replace-heavy or move-heavy runs over large files this is a full extra copy of the affected bytes.
- **Proposed change:** in `verified_at_on`, replace the `version_on` call with a stat-key comparison: `lstat` the path once and compare `stat_key(&attributes) == stat_key_of(expected)` (the expected entry's fingerprint key is available via the observation), returning `DestinationChanged` on mismatch. Keep the content re-read behind an opt-in "paranoid verify" policy if desired. Symlinks still need `read_link` (cheap, one request).
- **Expected impact:** eliminates one full file download per moved/replaced/removed file; for a million small documents the saving is ~1-2 round trips per file (the read itself is small), for large replaced files it removes a full download each — potentially 10-30% of end-to-end time on change-heavy runs. Confidence: **high** that the full read happens (verified in code), **medium** on magnitude.
- **Effort and risk:** small-medium; blast radius is `verified_at_on` callers (move/remove/replace on SFTP). Regression risk: same-second same-size modifications between scan and apply go undetected — same accepted tradeoff as the local endpoint's `proves_unchanged` (local.rs:330-340); mitigate with the `RACY_WINDOW` guard.
- **Measurement plan:** same fixture server; count `READ` requests/bytes during the apply phase of a run that replaces N files and moves N files, before and after; compare end-to-end wall clock and the `DestinationChanged` error rate in the existing test suite (`crates/foldover-endpoint/src/sftp/tests.rs`).

### 4. Pipeline `READDIR` requests when listing large directories

- **Category:** I/O and batching
- **Evidence:** `Session::read_dir` (`crates/foldover-net/src/sftp.rs:267-301`) sends one `SSH_FXP_READDIR` and waits for its reply in a loop — strictly sequential round trips per directory batch. The same file already pipelines reads the same way writes do: `read_from` (sftp.rs:339-411) queues `WINDOW` (16, sftp.rs:35) requests before collecting replies, and the session stashes up to `MAX_STASHED = WINDOW` out-of-order replies (sftp.rs:46, `stashed` field sftp.rs:107-108).
- **Hypothesis:** for wide directories (hundreds of thousands of entries in one folder, common in document stores) each `READDIR` reply returns one server-chosen batch, so listing costs one RTT per batch; pipelining `WINDOW` requests cuts that by up to 16x. For millions of files spread over many directories, per-directory fixed latency also adds up.
- **Proposed change:** in `read_dir`, send up to `WINDOW` `READDIR` requests on the open handle before collecting replies in id order (mirroring `read_from`'s `pending`/`VecDeque` pattern), stopping early when an `EOF` reply is seen and draining the rest; keep the existing error/drain discipline.
- **Expected impact:** listing-phase latency reduction of up to ~10x for very wide directories; overall end-to-end effect is small-to-medium (listing is usually a minority of run time unless directories are huge). Confidence: **medium-high** that it helps wide directories, **low-medium** on total end-to-end impact.
- **Effort and risk:** small, self-contained in `read_dir`; risk is desync if EOF handling is wrong — but the session already bounds stashed replies and drains on error, and `read_from` proves the pattern.
- **Measurement plan:** point `read_dir` at a directory with 100k entries on the fixture server (count `READDIR` packets with a timing shim around `Session::request`, sftp.rs:689-694) before/after; end-to-end: scan time of a flat 100k-file tree via `scan_10k_files`-style bench scaled up.

### 5. Raise the SFTP chunk size and in-flight window for streaming transfers

- **Category:** I/O and batching
- **Evidence:** `crates/foldover-net/src/sftp.rs:32-35` — `CHUNK = 32 * 1024` ("every server accepts 32 KiB") and `WINDOW = 16`, so a stream keeps at most 16 × 32 KiB = 512 KiB in flight per direction. `MAX_PACKET` (sftp.rs:39) already tolerates 1 MiB replies. Streaming reads/writes are pipelined (`read_from` sftp.rs:339-411, `write_chunks`/`queue_write` sftp.rs:433-500), so throughput per session ≈ `WINDOW × CHUNK / RTT`. The local hashing buffer is a separate 64 KiB (`crates/foldover-fs/src/root/content.rs:12`).
- **Hypothesis:** on a LAN with small RTT the current window is adequate for bandwidth, but per-request CPU and packet count per byte are 4-8x higher than needed; with jitter or a loaded NAS, 512 KiB in flight caps throughput. OpenSSH's `sftp-server` accepts read/write requests up to 256 KiB, so `CHUNK` can rise well above 32 KiB.
- **Proposed change:** raise `CHUNK` to 128-256 KiB (negotiate down on `SSH_FX_FAILURE` to an oversized write, falling back to 32 KiB) and `WINDOW` to 32-64, keeping `MAX_PACKET` as the guard; optionally make both pair-configurable. Raise `HASH_BUFFER_BYTES` to 256 KiB for the local side in the same pass.
- **Expected impact:** fewer requests and syscalls per byte (CPU-side win on both ends), and up to ~4x higher per-session throughput under jitter; for a workload of millions of *small* documents the effect is minor (files are smaller than one chunk). Confidence: **medium**.
- **Effort and risk:** tiny (constants plus a fallback path); risk is a non-OpenSSH server rejecting large writes — handled by retrying smaller, and `copy_data`'s "server answered more than requested" check (sftp.rs:536-541) already guards reads.
- **Measurement plan:** fixture server, one 1 GiB file up and down at (32 KiB, 16), (128 KiB, 32), (256 KiB, 64); measure wall clock and request counts; also run the existing `hash_64_mib` bench with the new local buffer.

### 6. Cut per-file round trips in the place/archive path via the listings cache

- **Category:** I/O and batching (redundant round trips)
- **Evidence:** replacing one file on SFTP currently issues: `receiving_on` lstat (`transfer.rs:419-444`), `create_new` open + pipelined writes + close (`sftp.rs:888-920`, `write_new` sftp.rs:1672-1686), `set_metadata_on` (sftp.rs:940-980), `staged_version_on` lstat (sftp.rs:921-934), then `place_on` (sftp.rs:1034-1082): `lookup_on` lstat, `listed_on`, `set_aside_on` → `make_directories_on` (sftp.rs:1606-1641) which does **one `make_directory` + on failure one `lstat` per path component**, then the archive rename and the final rename. The endpoint already maintains a `listings` cache of folders it has seen (`sftp.rs:103-105`, `spelling.rs:201 note_added`, `listing_on`), but `make_directories_on` does not consult it, so every replaced file re-walks the archive directory chain over the network even when this run created those folders.
- **Hypothesis:** for millions of replacements, the per-component mkdir/lstat walk of the archive path (`.foldover/archive/<run>/…`) is several round trips per file that a "known to exist this run" cache would eliminate; `staged_version_on`'s lstat also duplicates the fsetstat reply's implied state.
- **Proposed change:** have `make_directories_on` skip components whose folder is present in the `listings` cache (and mark newly created ones via the existing `note_added`), so the walk costs zero round trips after the first file in a folder; reuse the attributes from `set_metadata_on`'s session state to avoid the extra lstat in `staged_version_on` where the protocol allows.
- **Expected impact:** several round trips saved per replaced file; for a million replaced small files at ~0.2-1 ms LAN RTT across 4-8 workers, plausibly 5-20% of apply-phase time. Confidence: **medium**.
- **Effort and risk:** medium — cache-invalidation correctness is subtle (the cache already has `Dirty` marking in spelling.rs:40-44 for exactly this); risk of writing into a wrong-spelling path if the cache lies, mitigated by the existing `Dirty` discipline.
- **Measurement plan:** count SFTP requests per replaced file (shim around `Session::request`, sftp.rs:689) for a run replacing 10k files in 100 folders, before/after; end-to-end wall clock of that run.

### 7. Amortize the per-batch root identity re-reads during the apply loop

- **Category:** redundant work
- **Evidence:** `crates/foldover-engine/src/run.rs:52` `COMMIT_BATCH = 256`, and run.rs:1233-1245: every 256 planned rows the run calls `check_roots` (run.rs:1378-1397), which for each recorded side calls `endpoint.identity()`; on SFTP that is `read_small(IDENTITY_FILE, …)` (sftp.rs:315-327 → `read_small_on` sftp.rs:1648-1670 = open + read + close, ~3-4 round trips per side).
- **Hypothesis:** for a run that changes 1M files, the loop performs ~3900 batches × ~8 SFTP round trips ≈ 31k requests just to re-read a 64-byte identity file — minutes of pure overhead on a WAN-ish link, and it also serializes the copy workers behind the run thread's session borrow.
- **Proposed change:** make the root-check interval adaptive (e.g. every `max(COMMIT_BATCH, rows/64)` rows, or a time-based check every N seconds) and/or check only the side being written; the lease heartbeat already guards liveness between checks.
- **Expected impact:** small in absolute terms on a LAN (seconds), larger over higher-latency links; removes a periodic stall of all workers. Confidence: **high** that the reads happen as described, **medium** on end-to-end magnitude.
- **Effort and risk:** small; risk is detecting an unmounted/replaced root later than today — bounded by keeping a time-based minimum frequency.
- **Measurement plan:** instrument `check_roots` call count and `read_small_on` request count for a 100k-change run; compare wall clock with interval 256 vs 4096, and confirm the unmounted-root tests in `crates/foldover-engine/src/run/tests.rs` still catch root loss.

### 8. Tune the release build profile (LTO, codegen-units)

- **Category:** build and startup configuration
- **Evidence:** the workspace `Cargo.toml` defines no `[profile.release]` section (verified: no `profile` key in the root manifest), so release builds use Cargo defaults: `opt-level = 3`, `codegen-units = 16`, no LTO; `scripts/build.sh` builds with plain `--release` (scripts/build.sh:150-152).
- **Hypothesis:** the scan/plan/hash hot paths (`plan_candidates` over 100k+ entries, BLAKE3 streaming, BTreeMap operations, SFTP wire encode/decode in `crates/foldover-net/src/sftp/wire.rs`) are CPU-bound enough that thin LTO + `codegen-units = 1` buys a few percent; it is cheap insurance for a workload that runs for hours.
- **Proposed change:** add to the workspace `Cargo.toml`:
  ```toml
  [profile.release]
  lto = "thin"
  codegen-units = 1
  ```
  and keep debug builds untouched via a separate dev profile if link time becomes painful.
- **Expected impact:** ~5-15% off the CPU-bound portions (hashing, planning, wire codec), i.e. a few percent of end-to-end time for large trees; not a game changer. Confidence: **medium**.
- **Effort and risk:** tiny; risk is longer release link times and slightly larger binaries; no behavioral change.
- **Measurement plan:** run `cargo xtask bench-check` (the criterion benches in `perf/README.md`: `scan_10k_files`, `hash_64_mib`, `plan_100k_entries`, `db_*_100k`) on the same host before and after, comparing medians.

---

## Open questions and unverified items

- No live profile was taken in this pass: all impact magnitudes are hypotheses derived from code reading at commit `c997ae9`, not from measurements. The request-count and wall-clock effects of proposals 1-7 should be confirmed with the per-proposal measurement plans against a real `sftp-server` before implementation.
- The deliberate design note in `crates/foldover-endpoint/src/sftp/readers.rs:1-6` ("SFTP stat cannot prove a file unchanged") and the comment at `crates/foldover-endpoint/src/sftp.rs:644-646` ("SFTP versions are never reused unread") indicate proposals 1 and 3 reverse an intentional correctness stance (SFTP mtimes have whole-second granularity). Whether the project accepts the local endpoint's racy-window tradeoff for SFTP is a product decision — proposals 1 and 3 should ship behind a pair option if not.
- `Session::copy_data` (`crates/foldover-net/src/sftp.rs:502-551`) copies server-side chunk-by-chunk with strict request/response pairing; I found no production caller in this pass (only the definition), so no proposal targets it — worth confirming whether peer-to-peer or remote paths use it.
- The real per-`READDIR` batch size of the target NAS's SFTP server (proposal 4) and its `MaxSessions` (proposal 2) are unknown; both change the expected gains.
- `foldover-state` snapshot/commit costs (`replace_snapshot`, `perf/budgets.toml` `db_*_100k` budgets) are already benched and budgeted; they were not profiled here and are assumed not to dominate the SFTP end-to-end time.
- Whether `check_roots`'s identity reads can be skipped entirely for local sides (they are cheap `stat`s there) while keeping them for SFTP was not analyzed per-side.
