# ADR 0053: Background pair removal

Status: Implemented

## Context

Removing a pair deleted its row in one transaction, and the foreign keys
cascaded to every row the pair owned: two snapshot rows and one base row
per tracked file, plus its journal, history and conflict choices. The
engine lock and the single writer were held for the whole delete.

On 2026-11-08 a person removed a pair whose Folder A was their home
folder. The app reported "Agent request timed out", then "The agent did
not answer in time" for the pair list, and greyed out Edit Pair and Remove
Pair. Requests wait five seconds (ADR 0021). The pair list waits for the
same engine lock as the removal, so it timed out too.

A copy of the million-file qualification catalog reproduced the cost on
the Mac Studio (M4 Max, idle, load average 4): deleting the "Qualification
local million" pair (2,002,000 snapshot rows, 1,001,000 base rows) took
3.55 s with SQLite's cache warm and 4.02 s cold, with the agent's
`synchronous=FULL`, WAL and foreign keys. The time grows with the file
count, so a home folder on a laptop exceeds five seconds.

## Decision

- Removal only marks the pair: `pair.remove` checks the revision, drops a
  saved edit, and inserts the pair's ID into `pair_removal` (schema v28).
  Its cost no longer depends on the pair's size.
- Every lookup a caller starts from treats a marked pair as gone: the
  listing, `pair.get`, revision checks, the active configuration a job
  claims, and run history. So the pair disappears from the app, the CLI,
  the scheduler, catalog overlap checks and job claims at once. Writers are
  reached only through those lookups, and removal still requires an idle
  pair, so no job writes rows for a marked pair.
- The agent deletes marked pairs' rows in the background. Each step
  deletes up to 10,000 rows of one pair in one transaction, from the
  snapshot, base, journal, conflict-choice and history tables in turn.
  When a step finds fewer rows than its limit, it also deletes the pair
  row, which cascades to the small tables and the mark. The task takes
  the engine lock per step and waits 5 ms between steps, so queued
  requests go first.
- The task starts with the agent, which finishes removals an earlier run
  left, and wakes after every removal. A step that fails stops the pass;
  the rows stay hidden, and the task tries again five minutes later or at
  the next removal, whichever comes first.
- v28 raises the minimum reader to 28: an older agent would show, and
  could sync, a marked pair again.

## Verification and measurements

- `foldover-db`'s `removal_hides_the_pair_and_leaves_its_rows_for_the_purge`
  checks that removal leaves the rows, that every reader treats the pair as
  gone, and that the purge deletes the rows in limited steps without
  touching another pair.
- `foldover-service`'s `removal_leaves_the_rows_to_the_purge` checks the
  request path and the task the agent starts.
- The opt-in `million_file_service_removal` test removes a pair from a
  copied catalog and polls `pair.list` during the purge, as the app does.
  On the Mac Studio at a load average of 14, against a copy of the same
  catalog: `pair.remove` answered in 1.4 ms, and the purge took 5.9 s.
  During the purge, 235 `pair.list` calls were answered, the slowest in
  18 ms.

## Consequences

- Removal answers at once, and the pair list keeps answering during the
  purge.
- Deleting rows in steps writes the same pages as one statement and
  commits more often, so the purge takes longer in total. It runs while
  nothing waits for it.
- The state database does not shrink after a purge. SQLite reuses the
  free pages, as it did before.
- An agent stopped during a purge resumes it at its next start. A step is
  one transaction, so a stop never leaves a half-deleted step.
