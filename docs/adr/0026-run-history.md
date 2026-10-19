# ADR 0026: Run history

Status: Implemented

## Decision

The agent records every finished run, manual or scheduled, in the pair's
history (state schema v9): start and end time, trigger, outcome, counts,
the run's archive folder and a failure message. Previews are not
recorded. Each pair keeps its newest 100 runs; the history survives
configuration edits and is removed with the pair.

Outcomes: **completed**, **cancelled** (including before anything was
written), **needs approval** (stopped before writing) and **failed**.

- The job records its run before publishing the final status, so a client
  that sees a finished job and then reads `history.list` finds it.
- Recording is best effort: a failed write loses one history row and never
  affects sync state or the job's result.
- The app shows the newest ten runs under the pair's sync controls and
  refreshes them when a job ends; `foldover history ID` prints all of them.
