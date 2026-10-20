# ADR 0022: Scheduled and change-triggered runs

Status: Implemented

## Decision

Each pair has a schedule (state schema v5): `manual` (default), `every`
N minutes (1 minute to 1 week), or `changes`. Changing it keeps the base.

The agent checks every 5 seconds and starts unattended runs through the same
job manager as manual runs ([ADR 0021](0021-sync-job-api.md)); a busy pair
is skipped until its job ends.

| Schedule | Due |
|---|---|
| `every` | One interval after the last scheduled start, or after the agent started |
| `changes` | 10 seconds after the last change in either folder; at most 5 minutes after the first change since the last run, when changes never stop; and hourly |

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
  kinds, sizes and modification times with the previous check; excluded
  names and Foldover's own do not count. A directory whose entry changed
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
- **Reviewed first:** a pair runs unattended only after a first sync has
  recorded its folder identities. The first run is always one a person
  started.
- **Unattended gate:** unattended runs never carry approvals. Above the
  unattended deletion limit they stop with `NeedsApproval`, which the app
  shows ([ADR 0013](0013-deletion-review-fuse.md)).
- **Pause** (state schema v11): `automation.pause` holds automatic runs
  for every pair, for 1 minute to 1 week or until `automation.resume`.
  It survives agent restarts. Changes noticed meanwhile stay recorded and
  run after the pause; manual syncs are unaffected. The menu bar, the CLI
  (`foldover pause [MINUTES]`, `foldover resume`) and Shortcuts set it.
- Timing state lives in agent memory; a restart restarts every interval.

## Consequences

- An unattended run that stops for approval stays stopped until someone
  syncs manually; there are no notifications yet.
- Watching uses the configured root paths, resolved once; a folder moved
  while watched is noticed by the next hourly run, which then fails on its
  identity check or path.
