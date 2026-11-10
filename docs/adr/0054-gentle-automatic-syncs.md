# ADR 0054: Gentle automatic syncs

Status: Implemented

## Context

On 2026-11-09 macOS 27 warned that "foldoverd has been using more than
120% CPU for over 60 seconds" and offered to let it continue, limit its
CPU usage, or stop warning. No documentation of the alert was found, so
its exact rule is unknown.

The agent ran every sync at the priority macOS gives any process, with
every worker its folders allow: four readers per local folder on a
solid-state or network volume, eight SFTP readers, four copies, and every
due pair at once. Nothing set a launchd `ProcessType` or a thread's
quality-of-service class, and nothing told a sync a person waits for from
one the scheduler starts.

The CPU is real work. A read-only audit of the agent found no busy-wait
loops; every wait is a timer, a timeout or a blocking call. The archived
qualification samples of 2026-11-08 show million-file local scans and
fingerprint-expiry rereads at 170 to 300% CPU for 60 to 118 s, while
warm no-change runs stay near 55% and LAN SFTP runs near 1%. On the Mac
Studio (M4 Max, 14 cores, shared host), observing 100,000 local 8 KiB
files took 7.0 s at about 200%, 11.2 of its 12.9 CPU-seconds in the
kernel: metadata, extended attributes and reads, not hashing.

Much of that work starts unattended: interval and daily schedules,
changes, catch-up at start, and the hourly check of "When files change"
pairs ([ADR 0022](0022-scheduled-runs.md)). The hourly check counted from
the last scheduled start, so a run longer than an hour was followed at
once by the next.

Lowering priority alone does not keep the process under the alert. The
same 100,000-file scan under `taskpolicy -c utility` took 14.3 s at 110%
on average, and under `-c background` 23.4 s (6.7 to 34.8 s) at 98%, with
single samples still at 200 to 340% (medians of three, load 18 to 23).
`ProcessType` `Background` in the launchd plist would also slow the syncs
a person starts.

## Decision

- **A setting for automatic syncs:** gentle, the default, or full.
  Previews, syncs and undos a person starts always run at full effort.
  Schema v30 adds `automation_setting.automatic_effort`, default
  `gentle`. Older agents ignore the column and run at full effort, so the
  floor stays. An unreadable choice runs automatic syncs gently.
- **Gentle priority:** a gentle run's own thread, and each reader and
  copy thread it starts, enters the utility class for the run and returns
  to its previous class afterwards (`ThreadEffort` in `foldover-os`).
  macOS threads do not inherit their creator's class, which a native test
  checks, so every thread enters the effort itself.
- **Gentle parallelism where the processor limits the work:** a folder on
  this Mac's own disks is observed serially on the run's thread, and a run
  between two such folders copies one file at a time on that thread. Shares,
  SFTP servers and other Macs keep their readers and copies, since that
  work waits on the network (`Bottleneck` in `foldover-endpoint`). A
  gentle run therefore uses at most one core on local disks.
- **One gentle automatic sync at a time:** the scheduler, which looks
  every 5 seconds, starts at most one, and only while no other preview,
  sync or undo runs. The pair that waited longest since its last
  scheduled start goes first. It keeps its turn during the 5 seconds in
  which its folders' presence is checked; a slower folder check gives the
  turn up, so a dead share never holds the other pairs.
- **Full** keeps the earlier behavior.
- **The hourly check counts from the end:** for both settings, a "When
  files change" pair is due an hour after the last 5-second check that saw
  any of its jobs working, or after its last scheduled start when that is
  later.
- **Surface:** `automation.status` and `automation.settings` carry
  `effort` (`gentle` or `full`) beside `low_power` in local protocol 23.
  Each field `automation.settings` carries changes only that choice.
  Settings has "Run automatic syncs at full speed", and
  `foldover automation effort gentle|full` sets it.

## Verification and measurements

On the Mac Studio, `perf/run-qualification.py --prepare-only` observed
the same 100,000 files of 8 KiB from an empty catalog, three runs per
effort in alternating order, at a load average of 8 to 9:

| Effort | Median time | Median average CPU | Highest sample | Samples above 120% | Median CPU-seconds |
|---|---|---|---|---|---|
| Full | 5.6 s | 212% | 423% | 12 of 16 | 12.4 |
| Gentle | 8.1 s (6.2 to 17.7 s) | 94% | 100% | 0 of 33 | 7.6 |

The slowest gentle run averaged 49%: its utility threads waited while
other processes ran.

A second set ran while other processes kept the disk busy (load average
11 to 16, about 20,000 transfers per second from other programs). It
added one thread at normal priority, the gentle run's parallelism without
its priority, to tell the two apart. Medians of three alternating runs:

| Run | Full | One thread, normal priority | Gentle |
|---|---|---|---|
| Observe 100,000 files | 6.0 s | 17.4 s (17.3 to 19.4 s) | 56.6 s (21.0 to 104.5 s) |
| First copy of 20,000 files of 8 KiB | 4.9 s | 12.6 s | 17.9 s (15.8 to 22.1 s) |
| Samples above 120% CPU | 28 of 35 | 0 of 92 | 0 of 234 |

One thread is what keeps the process under one core. The utility class
adds the rest of the delay: macOS also caps a utility thread's disk
priority, so its reads and writes wait while other programs use the
disk. That is the class's intent, and Apple recommends it for work a
person sees progress on but does not wait for. A person who prefers
predictable speed chooses full effort.

Tests:

- `foldover-os`: lowering lasts until dropped and restores the previous
  class; a thread at utility or below is left alone; threads do not
  inherit their creator's class.
- `foldover-endpoint`'s
  `a_gentle_observation_reads_a_local_disk_on_one_thread` and
  `foldover-engine`'s
  `a_gentle_run_narrows_only_local_copies_and_syncs_the_same`, which
  covers local and SFTP targets.
- `foldover-service`: gentle automatic syncs start one at a time and take
  turns, wait for a sync a person started, and keep the turn while their
  folders are checked; full effort starts every due pair together; the
  hourly check counts from the end of a run longer than an hour; only
  automatic work follows the setting; the setting round-trips and changes
  alone. Each scheduling test fails with its change reverted.
- `foldover-db` and `foldover-state` keep both choices across reopening.

## Consequences

- Automatic syncs on this Mac's own disks use at most one core, at a
  priority the person's own work comes before, for the processor and the
  disk. They take longer: about half again as long for a 100,000-file
  scan on a lightly loaded Mac, and three to seventeen times as long while
  other programs keep the disk busy.
- Automatic syncs over the network keep their parallel reads and copies.
- Pairs due together sync one after another, and an automatic sync waits
  while a person's sync runs. A sync a person starts never waits for an
  automatic sync of another pair.
- A gentle run holds the engine lock at utility priority while it stores
  snapshots and commits. A request waiting for that lock can take longer
  while the Mac is busy.
- The setting does not limit syncs a person starts. A large manual sync
  can still trigger the macOS alert.
- Each automatic run that writes to a watched local folder is still
  followed by one run its own writes trigger (ADR 0022). That run now
  waits its turn like any other.
