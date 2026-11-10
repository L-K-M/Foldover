# ADR 0022: Scheduled and change-triggered runs

Status: Implemented

## Decision

Each pair has a schedule (state schema v5): `manual` (default), `every`
N minutes (1 minute to 1 week), or `changes`. Changing it keeps the base.

The agent checks every 5 seconds and starts unattended runs through the same
job manager as manual runs ([ADR 0021](0021-sync-job-api.md)); a busy pair
is skipped until its job ends. Gentle automatic syncs, the default, start
one per check and only while no other preview, sync or undo runs, the
longest-waiting pair first
([ADR 0054](0054-gentle-automatic-syncs.md)).

| Schedule | Due |
|---|---|
| `every` | One interval after the last scheduled start, or after the agent started, in awake time; or once the wall clock shows one interval since that start, since the last recorded run's start when the pair has not started under this agent yet, or since the agent's start when it has no recorded run |
| `changes` | 10 seconds after the last change in either folder; at most 5 minutes after the first change since the last run, when changes never stop; and an hour after the pair's last preview, sync or undo ended, or after its last scheduled start or the agent's start when no job has run since |

- **Change notifications** come from FSEvents on macOS and inotify on
  Linux (the `notify` crate, behind `foldover-watch`). They are hints: a
  change only starts a full observation, which reuses unchanged entries
  ([ADR 0018](0018-local-entry-versions.md)). Lost events and unwatchable
  folders, such as unmounted disks, are covered by the hourly run.
  Unwatchable folders are retried every check; one that becomes watchable
  counts as changed, since it may have changed while it was away.
- **Catch-up:** at the agent's first check each watchable folder's
  volume event history is replayed from the event recorded when the pair
  was last quiet. A change, or no usable record, counts as one change
  ([ADR 0002](0002-change-detection.md#implementation)).
- **Other computers' changes** reach no notification on shares and SFTP
  folders. Every 5 minutes the agent lists their root and up to 32
  recently active directories, one level each, and compares names,
  kinds, sizes and modification times with the previous check; names the
  pair's rules or the folder's `.foldoverignore` exclude, and Foldover's
  own, do not count. A directory whose entry changed
  is listed from then on, and when 32 are listed the least recently
  active one makes room. After any run the next check takes its listings
  as the new reference without comparing them, so the run's own writes
  are not changes. Edits further down
  wait for the hourly run. Pairs held by Low Power Mode or a costly
  network are not checked.
- **Missing folders:** when a due pair's folder path does not exist, as
  for an unmounted disk or share, no run starts. The pair's status becomes
  `waiting` for that side, without a notification or history entry, and the
  run starts at the first check after the folder is back. A manual run
  fails instead, naming the missing side.
- Changes inside `.foldover`, temporaries and `.DS_Store` are ignored. A
  run's own writes still mark the pair changed, so one quick follow-up run
  finds nothing to do.
- **First run:** new pairs become due on the configured cadence. The default
  `automatic` review policy permits unattended initial copies. The optional
  `required` policy stops every run with `NeedsApproval` until a person
  approves its unchanged plan. Folder identities are recorded only after a
  run completes; root checks apply on every run.
- **Unattended gate:** unattended runs never carry approvals. With the separate
  removal-protection setting enabled (the default), above-limit removals and
  suddenly empty folders stop with `NeedsApproval`, which the app shows
  ([ADR 0013](0013-deletion-review-fuse.md)). An optional `Previewed` result
  alone does not suspend scheduling; the next due run observes a fresh plan.
- **Pause** (state schema v11): `automation.pause` holds automatic runs
  for every pair, for 1 minute to 1 week or until `automation.resume`.
  It survives agent restarts. Changes noticed meanwhile stay recorded and
  run after the pause; manual syncs are unaffected. The menu bar, the CLI
  (`foldover pause [MINUTES]`, `foldover resume`) and Shortcuts set it.
- **Per-pair pause** (state schema v29): `pair.pause` holds one pair's
  automatic runs until `pair.resume`. It survives agent restarts, editing
  the pair keeps it, and removing the pair drops it; pausing does not change
  the pair's revision. A paused pair keeps being watched, and its changes
  run after the pause; manual Preview Changes and Sync Now still run and
  do not lift the pause. The app's sidebar and Pair menu, and the CLI
  (`foldover pairs pause ID`, `foldover pairs resume ID`), set it.
- **Intervals across sleep and restarts:** awake time is counted with
  `Instant`, which cannot be set back but stops while the Mac sleeps
  and starts over with the agent. Alone it made an "Every day" pair on
  a Mac that sleeps at night wait for a day of use, and wait forever on
  one that restarts daily. An interval pair is therefore also due once
  the wall clock shows a full interval since its last scheduled start
  or, before its first start under this agent, since the start of its
  last recorded run, manual or scheduled. A missed interval runs once
  when the Mac wakes or the agent starts, like a missed daily moment
  ([ADR 0039](0039-calendar-schedules.md)). A wall clock set back never
  holds a pair, since awake time still counts; one set forward costs a
  single early run.
- Other timing state lives in agent memory: a restart restarts the
  hourly check of `changes` pairs and the interval of a pair that has
  never run.

## Consequences

- An unattended run that stops for approval stays stopped until someone
  syncs manually; there are no notifications yet. A run that found a
  folder busy with another run is tried again when the schedule next
  comes due ([ADR 0047](0047-shared-one-way-sources.md)).
- Watching uses the configured root paths, resolved once; a folder moved
  while watched is noticed by the next hourly run, which then fails on its
  identity check or path.
