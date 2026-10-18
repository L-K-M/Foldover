# ADR 0022: Scheduled runs

Status: Implemented (interval schedules); change-triggered runs pending

## Decision

Each pair has a schedule: `Manual` (default) or an interval from 1 minute
to 1 week, stored as `schedule_minutes` (state schema v5). Changing it keeps
the base.

The agent checks every 30 seconds. A pair is due one interval after its
last scheduled start, or after the agent started. Due pairs get an
unattended run through the same job manager as manual runs
([ADR 0021](0021-sync-job-api.md)); a busy pair is skipped until its job
ends.

- **Reviewed first:** a pair runs unattended only after a first sync has
  recorded its folder identities. The first run is always one a person
  started.
- **Unattended gate:** unattended runs never carry approvals. Above the
  unattended deletion limit they stop with `NeedsApproval`, which the app
  shows ([ADR 0013](0013-deletion-review-fuse.md)).
- Schedules are in agent memory only; a restart restarts every interval.

## Consequences

- An unattended run that stops for approval stays stopped until someone
  syncs manually; there are no notifications yet.
- File-change triggers (FSEvents, [ADR 0002](0002-change-detection.md))
  will start the same unattended runs.
