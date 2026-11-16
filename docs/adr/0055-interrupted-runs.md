# ADR 0055: Interrupted runs

Status: Implemented ([plan](../resume-interrupted-runs.md), W3)

## Context

A run the agent stops during, for example at a restart, loses no
committed work: base and journal batches commit every 256 entries
(ADR 0020), and the next run finds finished work equal on both sides.
But the run itself was gone:

- Job state lives in memory, and only finished runs are recorded
  (ADR 0026). After a restart the pair read "Ready", with nothing saying
  a sync stopped halfway.
- A scheduled run's trigger survived by accident: it left no history
  row, so its interval or daily moment still looked missed. A person's
  sync waited for the person, who had no reason to know.
- The next run's first journal batch replaced the stopped run's journal
  (ADR 0029). Undo then covered only the second half of the work, and
  the first half could no longer be backed out.
- Nothing bounded a run that crashes the agent every time: crash-loop
  safe mode needs three unclean stops within five minutes, and launchd
  restarts the agent after 30 s, so a crash more than about two minutes
  into a run would repeat forever once runs resume by themselves.

Two designs were reviewed and rejected:

- **An unfinished history row as the marker.** `run_history.finished_ms`
  is `NOT NULL`, the outcome is an enumeration every reader decodes, and
  the scheduler reads the newest row as the last run. The row would need
  a schema rebuild, a raised minimum reader, a new outcome in every
  layer, and rules for closing rows a newer run supersedes.
- **Continuing the stopped run under its name.** The name is minted
  inside the run, after observing, so a run stopped while observing has
  none to continue. Reusing one needs the journal's sequence seeded,
  the bandwidth limit kept, and a policy for archive collisions: archive
  moves never overwrite, and a local replace places the new content
  before it archives the old.

## Decision

- **One row per pair** (`interrupted_run`, schema v31). A writing run, a
  person's sync or a scheduled one, is admitted when it starts: under
  the engine lock, in `Jobs::start`, with the same start time its history
  row gets. Admitting replaces the row of the run before, so no older
  run's row outlives a newer one. Previews and undos are not admitted.
- **Every result ends it.** When a job's result publishes, after any
  panic is caught, one that poisoned the engine's lock included: a
  finished run, or one a person cancelled, deletes the row; any other
  result (needs approval, failed, crashed) marks it `ended`, so the next
  run still continues its journal but nothing resumes it. Only a run the
  agent never saw end stays `running`.
- **A start finds it.** At an agent start, before the scheduler runs,
  every `running` row becomes `interrupted` and the pair's status is
  `interrupted` (local protocol 24), with when the run started and
  whether it continues by itself. The agent keeps its runtime lock until
  the process exits, after the runtime has waited for a sync still
  running, so no second agent can find that sync's row.
- **Continuing it.** Both kinds of run continue by themselves (the
  owner's decision of 2026-11-15 for a person's sync): the scheduler
  treats the pair as due at its first tick, manual pairs included,
  through the same holds as every automatic sync (safe mode, the global
  pause, pair pauses, the hold after an undo, Low Power Mode and costly
  networks). Cancelling an interrupted pair declines it: the row is
  deleted and nothing continues it. A preview or an undo of the pair
  takes the interrupted sync's place: its row is marked `ended`, so
  nothing continues it, in this agent or after the next start, and the
  next run still continues its journal.
  - A scheduled run continues as a scheduled one.
  - A person's sync continues as `Work::Resume`: at full effort, with
    the share remount and the manual history trigger, like the person's
    own sync, and with the interactive deletion check. It presents no
    reviewed plan, since that died with the old agent, so it stops for
    approval above 50 deletions and 1% of a side, and whenever the pair
    requires review: it starts unattended and halts at the fuse.
  - A person's sync continues only under the settings it started with:
    when the pair's revision changed, for example because a saved edit
    was activated at the start, it waits for the person.
  - "Skip in the next run" choices are stored (`pending_skip`, schema
    v32) instead of held in memory, so the continuing run honors them.
- **Undo covers the chain.** A run continuing one that wrote, or that
  itself continued one, keeps the journal: its first batch leaves the
  earlier runs' entries in place. Each run keeps its own name and archive
  folder. Undo reverts every run left in the journal, newest first, each
  entry from its own run's archive, and spends them all. Run names begin
  with their start time, so name order is start order.
- **Bounds.**
  - A journal whose oldest run started more than the archive retention
    minus a day ago (29 days) starts afresh: the continuing run prunes
    archive folders older than 30 days when it finishes, and Undo could
    no longer put back what the oldest one held.
  - Starts after an unclean stop count against the row, and a run
    continuing an interrupted one inherits the count. After two, the run
    no longer continues by itself and waits for a person. Its pair's
    schedule waits with it: a killed run leaves no history row, so the
    pair stays due and the schedule would start the same work at every
    agent start. A person's sync, preview or dismissal lifts the wait.
    Clean stops, including reboots, never count.
  - Activating a saved edit that changes roots or metadata categories
    deletes the row with the base and journal: nothing is left to
    continue.

## Consequences

- After a restart, a pair whose sync stopped says so, in the app (the
  sidebar menu offers Don't Resume or Dismiss), the CLI and
  `sync.status`.
- A backup continues at once instead of at its next interval or the
  person's next sync, and Undo after it reverts the whole logical sync.
- A killed run gets less than that. A crash or a power loss kills one,
  and so does a logout or a restart during a sync that needs longer
  than launchd's exit timeout: a stopping agent lets its sync run on
  until launchd kills it. The run continuing it first watches each
  lease it needs that the dead run left, for two minutes and however
  long ago that run died: a lease counts as dead only once its reader
  has seen it unchanged that long ([ADR 0032](0032-root-leases.md)).
  That is about two minutes for a Backup or a Mirror, which leases its
  target, and about four for a two-way Sync, which leases both folders
  one after the other. And what the killed run wrote after
  its last commit, up to a batch of 256 entries and the copies then in
  flight ([ADR 0020](0020-run-execution.md)), is in no journal: the
  continuing run finds those paths equal on both sides, and Undo leaves
  them as the killed run left them. A version it replaced or removed
  there stays in its archive folder.
- A resumed sync of a pair an undo holds does not start: the hold keeps
  every automatic start out until a person syncs.
- A path written by both an interrupted run and the run continuing it
  goes back only to the first run's version, and is listed as changed
  since the sync: Undo judges every entry against the observation it
  took before reverting anything.
- The continued run's history row counts only its own work; the stopped
  run's counts were in memory.
- Older agents ignore the table: they replace the journal as before and
  show no interrupted run.
- Still open, in the plan: the re-observation cost, which for SFTP
  folders means reading every remote file again (W1, W2).
