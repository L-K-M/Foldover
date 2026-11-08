# Performance qualification results

The worthwhile changes in the [action plan](../docs/performance/action-plan.md)
are implemented. These measurements separate completed runs, failed runs and
unqualified cases. They do not establish release readiness.

The [evidence bundle](results/qualification-2026-11-08.tar.gz) contains 94 cases:
sanitized JSON summaries, traces, resource reports, samples and provenance.
The [index](results/index.csv) records revisions, binary hashes, return codes,
per-iteration timings and kernel peak RSS; the [manifest](results/manifest.json)
records artifact hashes. Private paths, the LAN address and synthetic review
tokens are redacted. No binaries or corpus contents are included. Extract it
with `tar -xzf perf/results/qualification-2026-11-08.tar.gz` into a separate
working directory.

## Conditions and limits

Runs used an Apple M4 Max (Mac16,9, 14 CPU cores, 36 GiB RAM), macOS 27.0.1,
and Rust 1.99.0. The host was shared, with background work and uncontrolled
OS caches. No idle-host, cold-cache or network RTT claim is made. The NAS
hardware and SSH disconnect cause were not established. A later preflight
recorded Linux `6.12.105-production+truenas`, OpenSSH 9.2p1 on the server and
OpenSSH 10.3 on the client. These are environment observations, not a cause
for the earlier disconnects. LAN fixtures used
8 KiB unique deterministic SHAKE documents, 1,000 files per directory.

Memory uses decimal MB and a strict 500,000,000-byte comparison. Kernel peak
RSS is authoritative for the measured process and includes transient peaks;
it excludes SSH children and the server. Samples may miss short peaks. Raw
`ps` CPU values are OS reports, not per-phase or instantaneous utilization.
A zero process return code does not imply that a memory budget passed.

Each paired case uses matched fixtures and saved binaries. Separate experiment
sets have separate baselines: their speedups must not be added or multiplied.
The Service measurement calls the real dispatcher and holds its preview, but
does not start an installed daemon, IPC, scheduler or peer discovery.

## Promoted changes

| Workload and phase | Before samples, seconds | After samples, seconds | Median before → after |
|---|---|---|---|
| LAN 10k, remote observation, four → eight readers | 130.501, 134.597, 133.013 | 69.623, 70.548, 76.648 | 133.013 → 70.548 |
| LAN 10k, remote observation, serial → one-write CLOSE/LSTAT | 68.678, 67.586, 86.364 | 56.158, 56.135, 54.777 | 68.678 → 56.135 |
| Local 100k, full source observation, one → four readers | 19.900, 6.258, 5.703 | 4.623, 3.930, 3.495 | 6.258 → 3.930 |
| LAN 1k deletions among 10k tracked files, apply | 93.550, 93.014, 92.517 | 32.588, 28.363, 29.644 | 93.014 → 29.644 |
| Same deletion workload, complete run | 148.258, 148.613, 148.290 | 88.901, 85.907, 87.825 | 148.290 → 87.825 |

All six archive samples completed with exactly 1,000 archives, zero copies
or metadata updates, and no failures. Pair order alternated before/after,
after/before, before/after. Each catalog came from the same closed seed;
only the owned NAS fixture files were restored between samples. Copy and
folder barriers, single-worker behavior, failure, cancellation, worker panic
and native undo have regressions. See [ADR 0043](../docs/adr/0043-parallel-copies.md).

The local reader comparison retains the slow first baseline sample. It is
not a cold-cache qualification. [Protocol measurements](sftp.md) record
packet budgets, spelling-index timings, the 1k reader comparison, source
revisions and transport details. The real 256-row DB commit benchmark also
improved from about 2.34 s to about 0.60 s during A2; the current budget run
and scaling regression cover that production transaction shape.

## Million-file memory

These are distinct cases, not one continuous speed comparison. Local fixtures
contain one million actual 8 KiB files and 1,000 directories on each populated
side. The initial matching-folder case executes baseline commits with zero
file writes. First-copy preparation includes full hashing, snapshot storage
and a preview, but does not transfer all files.

| Case | Kernel peak MB | Result and scope |
|---|---:|---|
| Synthetic catalog input load, initial dense representation | 840.2 | Over budget before scanning; input only |
| Synthetic native/native input load, compact representation | 452.3 | Input only; not a filesystem qualification |
| Synthetic native/SFTP input load, compact representation | 420.3 | Input only; not a LAN qualification |
| Local first-copy preparation, raw keys, consumed snapshots | 486.4 | Completed prepare-only run |
| Two populated local folders, initial baseline before sharing versions | 559.6 | Completed, over memory budget |
| Same populated local workload after sharing verified versions | 473.9 | Completed below budget |
| Warm tracked local no-change run | 481.0 | Completed below budget |
| Periodic local rehash before compact original timestamps | 912.8 | Completed, over memory budget |
| Rehash with the first timestamp column | 509.7 | Completed, still over budget |
| Rehash with exact compact nanoseconds | 498.3 | Completed below budget |
| Real Service preview over expired local caches, initial plan allocation | 500.2 | Completed, narrowly over budget |
| Service preview with initial reservation experiment | 498.6 | Intermediate passing sample |
| Service preview with exact union reservation | 489.9 | Completed below budget |

For expiry cases, regular file mtimes were aged 30 days before seeding.
Copies of the closed warm catalog had only fingerprint observation times
aged 21 days, preserving stat keys, versions and file contents. Later samples
subtract seconds per row and preserve full nanoseconds. This forces ordinary
periodic rehashes outside the local racy window. The final service preview
reported zero proposed changes for 1,001,000 paths per side. During that run,
maximum in-process status-handler time was 0.143 ms and maximum polling
lateness was 13.532 ms; these are not IPC round-trip measurements.

The exact plan reservation counts the immutable path union before allocating
rows. It avoids geometric growth for matching trees and for mixed additions
and deletions; an extra cancellable read-only pass is the tradeoff. See
[ADR 0052](../docs/adr/0052-compact-observation-storage.md) for ownership,
version lifetimes and exact cache comparison. These results cover one active
preview and these path/metadata distributions, not arbitrary concurrent plans
or an installed-agent memory guarantee.

## Additional workload and service checks

Eight complete local cases cover 10,000 flat 1 KiB files and 10,000 files
under eight Unicode directory levels at 32 KiB each. Both fixtures passed
initial copy, no-change, exactly 100 content edits, and then 100 different
atomic replacements plus 100 different deletions. The initial deep copy
also created 4,254 directories. Each edit case copied 100 files; each final
case copied 100 and archived 100. No-change cases proposed no writes.
Per-case verifiers compared counters, file/directory path sets and content
hashes. A separate parent check hashed all 9,900 final files on each side.
These are single shared-host observations, not paired speedup measurements.

The real million-file Service input probe measured 0.593 ms for capture and
mount lookup under the engine mutex, 0.610 ms waiting by a contending caller,
and 3.613 s decoding after release. The barrier handshake is excluded from
capture time. A separate service scan cancelled after 10,228 checked entries
and settled in 37.673 ms, with completion polled every 5 ms. These focused
probes do not measure installed-daemon RPCs, input-decode cancellation or
stalled remote I/O. Their peaks of 454.0 and 456.6 MB cover input loading and
a cancelled scan, not a completed preview or sync.

A known 12,288-byte local copy and its following no-change run verified the
new logical-byte counters: 12,288 and zero respectively. Counter values are
retained separately from duration nanoseconds; logical bytes do not describe
SSH traffic or resumed-transfer payloads.

Two new budgets cover one million prefix queries against 10,000 roots,
with shallow and deeper Unicode paths. Each fixture asserts 500,000 hits
and 500,000 boundary-sibling misses before timing. The full gate measured
123.8 and 387.8 ms against new 369 and 1,147 ms limits, calibrated at about
three times the earlier medians. Existing limits are unchanged. These time
`contains_ancestor`; setup and allocation counts are excluded. Raw Criterion
samples and calibration provenance are in the evidence bundle.

## Completed and failed LAN qualification

The clean 100k no-change run completed with zero copies, updates or archives:
local observation 1.809 s, remote observation 575.466 s, apply 13.670 s and
total 591.719 s. The apply cost still includes the unchanged root-check,
lease, history and retention policies.

The attempted three-pair 100k comparison stopped in its first baseline when
SSH broke after roughly 70k remote files and 1,173 seconds. No after sample
or remaining pairs completed. A separate million-file prepare-only LAN run
failed after 3,712.8 seconds, around 470k remote files, with a broken SSH
transport. Its peak was 513.9 MB on the earlier representation. Neither is
a successful qualification, and the final implementation has no completed
million-file LAN run. The failed artifacts are retained. A new full run of
the final implementation is in progress; its result is not included yet.
The NAS owner reports no known SSH session limits or network interruptions,
so the earlier disconnects remain unexplained transport failures.

## Rejected and deferred experiments

- The two-write read-finish variant had repeated long outliers. Keep the
  repeatable one-write implementation; the neutral and regressive samples
  remain in the bundle.
- Thin LTO with one codegen unit produced a 15.3% smaller example binary
  (10,135,184 → 8,590,368 bytes), but three paired local 10k runs showed no
  repeatable runtime gain. Median total was 318.72 → 325.18 ms, with overlapping
  ranges. Keep the release defaults. The default build reused registry
  dependencies while the thin build rebuilt them all: 7.87 and 36.55 seconds
  are not a fair clean-build comparison. No FFI/profile promotion is claimed.
- Separate left/right overlap, additional READDIR pipelining and bulk-window
  changes were not promoted without a material measured benefit. The local
  phase is small relative to remote observation in the clean LAN sample.
- Stat-only SFTP trust, larger commit batches, weaker SQLite durability,
  less frequent root checks, panic abort and speculative post-write caches
  remain excluded or require their separate designs.

## Verification and remaining gaps

All nineteen benchmark medians passed with the final exact-union
reservation, including the two new prefix-scaling cases. The seventeen
existing limits are unchanged. The evidence bundle includes the final gate table.
The device matrix passed on case-sensitive APFS, HFS+, exFAT and FAT32.
FFI binding drift/type resolution, layer checks, agent lifecycle smoke and
focused tests passed locally. The full local CI command still reports three
native filesystem failures also reproduced on unchanged main: changing
resource-fork work bounds, quarantine preservation and archive-restoration
mode expectations. They are not reported as passing. CI/review for the final
commit is recorded on the integration PR.

Completed million-file LAN qualification, true cold-cache runs, installed-daemon
qualification, the full physical-device/overnight matrix and release/beta
qualification remain. Wide flat directories, deeper paths, more metadata,
multiple active previews, HDDs and restrictive SSH servers can change time
and memory. Follow [qualification.md](qualification.md) on isolated data;
keep the failures and limits alongside any new results.
