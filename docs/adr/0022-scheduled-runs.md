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
| `changes` | 5 seconds after the last change in either folder, and hourly |

- **Change notifications** come from FSEvents on macOS and inotify on
  Linux (the `notify` crate, behind `foldover-watch`). They are hints: a
  change only starts a full observation, which reuses unchanged entries
  ([ADR 0018](0018-local-entry-versions.md)). Lost events and unwatchable
  folders, such as unmounted disks, are covered by the hourly run.
- Changes inside `.foldover`, temporaries and `.DS_Store` are ignored. A
  run's own writes still mark the pair changed, so one quick follow-up run
  finds nothing to do.
- **Reviewed first:** a pair runs unattended only after a first sync has
  recorded its folder identities. The first run is always one a person
  started.
- **Unattended gate:** unattended runs never carry approvals. Above the
  unattended deletion limit they stop with `NeedsApproval`, which the app
  shows ([ADR 0013](0013-deletion-review-fuse.md)).
- Timing state lives in agent memory; a restart restarts every interval.

## Consequences

- An unattended run that stops for approval stays stopped until someone
  syncs manually; there are no notifications yet.
- Watching uses the configured root paths, resolved once; a folder moved
  while watched is noticed by the next hourly run, which then fails on its
  identity check or path.
