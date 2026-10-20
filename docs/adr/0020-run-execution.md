# ADR 0020: Run execution

Status: Engine and identity gate implemented; agent API and app pending

## Decision

A run prepares, then executes, without holding the engine lock:

```text
prepare: open roots ─▶ observe both sides ─▶ plan against the base
execute: deletion gate ─▶ archive leftovers
         ─▶ forward pass (parents first): copies, metadata, base records
         ─▶ reverse pass (children first): archives
         ─▶ base commits in batches of 256, each bound to the pair revision
```

- **Identity:** the first run writes `.foldover/id` (a random 128-bit ID)
  into each root, reusing one that exists, and records both before any
  other write. Later runs stop before planning if either file is missing or
  different. Otherwise an unmounted disk that leaves an empty folder at its
  path would look like a mass deletion, and small trees fall below the
  deletion limits. Changing a root forgets that side's identity.
- **Free space:** before any write, each side's volume must have room for
  every file copied to it plus a 256 MiB reserve. Replaced versions stay in
  the archive on the same volume, so a replacement needs its full size.
  Otherwise the run stops with `InsufficientSpace` and writes nothing.
- **Gate:** per target, the plan's deletions are reviewed
  ([ADR 0013](0013-deletion-review-fuse.md)). Interactive runs above the
  limit need an `Approval` covering at least that many deletions; unattended
  runs above their limit always stop. A stopped run writes nothing and
  returns the preview.
- **Confirmation binds to the plan:** the agent keeps each pair's latest
  preview — its revision and every item's action and versions — with the
  `review_token` fingerprint the wire publishes. A run with a nonzero
  approval or a presented token executes only when that token names the
  held preview *and* its freshly planned preview still equals it;
  otherwise it stops with the fresh `NeedsApproval` preview, so an
  approval can never authorize deletions a person did not see, even when
  another client's preview replaced the held plan. A matching review also
  marks the run `RunContext::Reviewed`.
- **First run:** while either root lacks a recorded identity, every run
  stops with its preview unless it is `Reviewed` — the first run of every
  pair is previewed (PLAN.md §3.2, §3.6), which also confirms a non-empty
  target. A `Reviewed` retry of the unchanged plan proceeds.
- **Emptied side:** when one side holds no entries while the base
  remembers some, a plan that archives on the other side stops for review
  even below the fuse, so a wiped folder or a stale empty copy cannot
  empty its partner unseen (PLAN.md §3.6). Copies into an emptied side,
  such as a Backup target, need no review.
- **Roots during the run:** writes go through handles held on each root.
  Both roots' identity files are read again before the first write and
  every 256 planned entries; a missing, unreadable or different identity
  commits the work done so far and stops the run (`IdentityChanged` or
  `RootLost`), so an unplugged or swapped disk stops the run at once
  instead of failing entry by entry (PLAN.md §4.7).
- **Operations:** each planned change goes through the endpoint operations
  ([ADR 0019](0019-local-write-primitives.md)), which recheck both sides.
  A failure is reported per path and skips that path's descendants; other
  paths continue. Conflicts and blocked branches are reported, not written.
- **Base:** each successful change records the source's planned version and
  the target's stored version; archives and both-absent paths are forgotten.
  A commit fails if the pair's revision changed, which stops the run.

**Retention.** Archives live in `.foldover/archive/<run>/` on each side,
where `<run>` is the run's UTC start time
(`2026-10-18T09-30-00.123Z`). After a run completes without cancellation,
run folders older than 30 days are deleted permanently (owner decision,
PLAN.md §10). Only canonical run names count, so other folders in the
archive are kept. Deletion never follows links; a pruning failure is
reported, not fatal. This is the only code path that deletes user data.

**Recovery without a journal.** A crash loses at most one uncommitted
batch of base updates. The next run then sees equal content on both sides
and records it, or sees both absent and forgets it. A displaced original
left under a temporary name is reported by the next observation and
archived. No path is ever left without its old or new entry. This replaces
the planned operation journal for local pairs; remote endpoints without
atomic exchange may still need one. The run journal of
[ADR 0029](0029-undo.md) records writes for Undo only.

**Staying awake.** A prepared run holds a `PreventUserIdleSystemSleep`
power assertion, named "Foldover is syncing <pair>", from before its scan
until the run is dropped after the last write (PLAN.md §3.5). Sleep the
person chooses still happens; the run then stops like any interrupted
run. On macOS a failed assertion fails the run before it reads or writes
either folder. Systems without power assertions (Linux) run without one.

## Consequences

- A preview and its run are separate observations. The run re-plans and
  re-gates; approvals stay counts plus an opaque `review_token` on the
  wire, and the service binds them to the held preview by token and exact
  equality — a changed or superseded plan needs new review.
- Configuration changes during a run stop it at the next commit; up to one
  batch of writes may already have used the previous configuration's roots.
- Missing: free-space checks, progress events and cancellation from
  clients. Read-only roots cannot receive an identity file and cannot sync.
