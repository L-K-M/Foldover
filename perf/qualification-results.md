# Performance qualification results

The worthwhile changes in the [action plan](../docs/performance/action-plan.md)
are implemented. These measurements separate completed runs, failed runs and
unqualified cases. They do not establish release readiness.

The [evidence bundle](results/qualification-2026-11-08.tar.gz) contains 114 cases:
sanitized JSON summaries, traces, resource reports, samples and provenance.
The [index](results/index.csv) records revisions, binary hashes, return codes,
per-iteration timings and kernel peak RSS; the [manifest](results/manifest.json)
records artifact hashes. Private paths, the LAN address and synthetic review
tokens are redacted. No binaries or corpus contents are included. Extract it
with `tar -xzf perf/results/qualification-2026-11-08.tar.gz` into a separate
working directory.

Index schema 2 preserves original build descriptions in `source_label` and
records a full `source_base_commit` when the explicit commit can be resolved.
Interpret patched builds using their labels, base commits and saved source.
Process-wide `harness_elapsed_s` and kernel RSS repeat across that process's
iteration rows; the `total_s` duration belongs to the individual iteration.

## Conditions and limits

Runs used an Apple M4 Max (Mac16,9, 14 CPU cores, 36 GiB RAM), macOS 27.0.1,
and Rust 1.99.0. The host was shared, with background work and uncontrolled
OS caches. No idle-host, cold-cache or network RTT claim is made. The NAS
hardware was later identified through the read-only API as an eight-core
i7-9700K running TrueNAS 25.10.7, reporting 67,265,757,184 memory bytes. Its
observed load averages were about 11 on eight cores. Disk/media/layout and
the SSH disconnect cause were not established. A later preflight
recorded Linux `6.12.105-production+truenas`, OpenSSH 9.2p1 on the server and
OpenSSH 10.3 on the client. These are environment observations, not a cause
for the earlier disconnects. LAN fixtures used
8 KiB unique deterministic SHAKE documents, 1,000 files per directory.

Memory uses decimal MB and a strict 500,000,000-byte comparison. Kernel peak
RSS is authoritative for the measured process and includes transient peaks;
it excludes SSH children and the server. Samples may miss short peaks. Raw
`ps` CPU values are OS reports, not per-phase or instantaneous utilization.
A zero process return code does not imply that a memory budget passed.

Some cases record failed `ps` samples; their sampled child/session/CPU
observations are incomplete. Kernel peak RSS and runner phase durations are
collected independently of those samples. Earlier runners did not all emit
`samples.jsonl`; absent files are not reconstructed. Filter failed process
return codes before comparing complete-run timings. Partial pairs remain as
failure evidence, with no invented counterpart.

The bundle records historical source gaps in `provenance/legacy-source-gaps.json`.
The exact intermediate `service1m-expired` diagnostic source and
`service1m-tight` planner source were not retained. The tight trial records
its original planner hash, and its two Service probe files match the saved
terminal source. The included `state-load.rs` is the terminal capture/decode
probe; earlier synthetic input-only variants predate that API and are not
claimed to be byte-identical. These historical rows retain their measured
values and binary hashes. The final signed-agent cases have verified source
patches and build hashes.

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

The table selects diagnostic and qualification cases; the index retains all
runs, including additional intermediate memory-budget misses. These are
distinct cases, not one continuous speed comparison. Local fixtures
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
| Isolated signed agent, real IPC, persistent expired-cache fixture | 502.8 | Complete zero-change preview, over budget |
| Same agent workload, first comparison released before the second scan | 494.9 | Complete zero-change preview, below budget |

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

The agent case runs the real `foldoverd`, authenticated CLI/IPC and scheduler
in a private uninstalled signed bundle. Its app identity stub is never
launched. The diagnostic patch selects an explicit fixture catalog; startup,
crash accounting, authentication and engine behavior remain active. There is
one manual local pair and no paired devices. Each preview covers 1,001,000
paths per side, proposes zero changes and remains held after its client exits.
Status samples include CLI startup and IPC; the maximum after the change was
83.934 ms, which is not comparable to the direct-Service handler measurement.

The first side's exact comparison can finish after its own spelling alignment.
Hints are captured first at their original raw spellings. Cross-side exclusions
later change planning visibility only, preserving verified snapshot values.
Releasing its prior entries and eight-byte timestamp column before the second
observation removes about 8 MB of overlapping storage. Functional tests cover
equal, refreshed, replaced and missing rows with exclusions before and after
comparison. The same closed seed and byte-exact 21-day cache-age mutation were
used for both agent cases. Whole-process durations were 100.4 and 167.0 seconds
on the shared host; no wall-clock improvement is claimed from these samples.

An earlier agent trial failed its completeness assertion: the old aged
temporary trees contained only their 1,000 directories per side, while the
catalog still held the million-file snapshots. Its 554.6 MB peak is retained
as a failed fixture trial, not a normal million-file preview result. The cause
of the missing physical files is unknown. A new persistent owned corpus was
cloned from the intact original: exactly one million actual 8 KiB files per
side, aged 30 days, then freshly seeded with zero copies, updates or archives. That seed
peaked at 449.4 MB. Before aging its cache, independent streaming checks verified
all 2,002,000 snapshot rows, the unchanged stat bytes/nanoseconds/versions, all
other tables and the source catalog hash. File counts were checked again before
each agent run. These isolated agent cases do not qualify the installed app,
multiple previews or peer discovery.

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

The earlier attempted three-pair 100k comparison stopped in its first baseline
when SSH broke after roughly 70k remote files and 1,173 seconds. No after sample
or remaining pairs completed. A separate million-file prepare-only LAN run
failed after 3,712.8 seconds, around 470k remote files, with a broken SSH
transport. Its peak was 513.9 MB on the earlier representation. Neither is
a successful qualification. The final-code retry also failed after 5,567.5
seconds near 780k remote entries, with the same SSH broken pipe. Its 314.7 MB
peak covers that incomplete run, not a completed million-file qualification.
All failed artifacts are retained. The isolated retry with per-connection SSH
client logging then completed: local observation 54.627 s, remote observation
6,530.469 s, apply 335.538 s and total 6,927.075 s. Kernel peak RSS was
416.7 MB, with zero copies, updates or archives and 3,912 recording commits.
This uses a fresh catalog over two already populated matching million-file
folders; it establishes their baseline, rather than qualifying warm tracked
input loading. Its binary has the earlier final production core plus logging
only. The subsequent comparison-lifetime change is qualified by the agent cases.

The new 100k comparison completed all three baseline/current pairs with
zero copies, updates or archives. Every sample cloned the same closed
tracked catalog and used the same unchanged corpus. Pair order was
before/after, after/before, before/after. The original baseline is
`6c15c8e`; the optimized production build is `b48dc43`. This compares the
combined changes, rather than isolating the final lifetime adjustment.

| Phase | Before samples, seconds | After samples, seconds | Median before → after |
|---|---|---|---|
| Remote observation | 1365.221, 1396.490, 1392.555 | 554.243, 549.650, 559.371 | 1392.555 → 554.243 |
| Complete run | 1388.364, 1413.605, 1410.236 | 570.989, 567.723, 574.902 | 1410.236 → 570.989 |

These are shared-host runs with uncontrolled OS caches. All six complete
samples, their kernel RSS, binary hashes and final comparison checks are
included in the bundle. Earlier failed comparisons remain in the index.

The owner reports no known SSH session limits or network interruptions.
Read-only checks found no corresponding Mac sleep event. The API-provided
SSH log records session closures without a cause; the kernel window contains
container bridge changes nearby, which do not establish causation. Direct
journal access was denied, but finite read-only API log jobs succeeded and
completed. No client/server configuration or power setting was changed.

Three real SSH packet-count pairs each ran 100 reads of the same owned 8 KiB
file. Every bounded run used 200 READ requests; every unknown-length run used
3,200. Both delivered 819,200 DATA payload bytes in 100 DATA replies. Maximum
outstanding requests were one and sixteen respectively, with no undrained
replies. These are framed protocol counts, excluding SSH overhead; the relay
adds overhead, so this is not a latency or RTT comparison.

A reverse SFTP-to-local copy completed 10,000 files and ten directories,
then completed a no-change run. File path/content SHA-256 maps matched the
owned original corpus. A separate local-to-SFTP case copied sixteen unique
16 MiB files (256 MiB), followed by a full remote reread with zero changes.
Initial apply was 33.362 s; the following remote observation was 3.934 s.
Source inspection found no configured bandwidth limit and an existing write
window maintained across producer chunks. The local medium reported SSD in
under one millisecond. The write/read timing gap is not explained by these
checks; no further transfer default was promoted from this single shared-host
observation. Raw scripts, counts and results are retained.

## Rejected and deferred experiments

- The two-write read-finish variant had repeated long outliers. Keep the
  repeatable one-write implementation; the neutral and regressive samples
  remain in the bundle.
- Thin LTO with one codegen unit produced a 15.2% smaller example binary
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
focused tests passed locally. At this revision, the full local CI command
reported three native filesystem failures also reproduced on unchanged main: changing
resource-fork work bounds, quarantine preservation and archive-restoration
mode expectations. They are not reported as passing. CI/review for the final
commit is recorded on the [integration](https://github.com/L-K-M/Foldover/pull/268)
and [agent follow-up](https://github.com/L-K-M/Foldover/pull/269) PRs.

The follow-up in [PR 271](https://github.com/L-K-M/Foldover/pull/271) fixed the
quarantine defect and corrected the two native test contracts. All 91 native
filesystem tests and the full CI gate then passed. The historical failures
above remain part of this experiment's evidence. See the subsequent
[installed-app qualification](release-readiness-results.md) for the new
NAS, metadata, cancellation, memory and peer results and their limits.

At the end of this experiment, warm tracked million-file LAN qualification,
true cold-cache runs, installed-app qualification, the full physical-device
and overnight matrix, and release/beta qualification remained. Wide flat directories, deeper paths, more metadata,
multiple active previews, HDDs and restrictive SSH servers can change time
and memory. Follow [qualification.md](qualification.md) on isolated data;
keep the failures and limits alongside any new results.
