# ADR 0023: Conflict resolution and per-item overrides

Status: Implemented for files, links and folders present on one side

## Decision

A person resolves a conflict from a preview by keeping one side. The agent
stores the choice with the versions both sides held in that preview (state
schema v6). The planner applies a choice only while both sides still hold
exactly those versions; otherwise the conflict is reported again.

| Kept side | Other side | Next run |
|---|---|---|
| Present | Present, other content | Copy the kept version; the other is archived |
| Present | Present, same content | Copy the kept metadata |
| Present | Absent | Copy the kept version |
| Absent | Present | Archive the other version |

- Nothing is deleted: a replaced or removed version goes to the run's
  archive ([ADR 0020](0020-run-execution.md)).
- A choice for a folder conflict (a folder deleted in one folder while
  something inside it changed in the other) covers the whole folder: the
  kept side wins every entry inside it, as a mirror from that side would.
  Keeping the folder copies it back with its contents; keeping the
  deletion archives it with its contents. The choice applies while the
  folder itself is unchanged; a later edit inside the folder follows the
  choice and is archived, never lost. Type mismatches between a file and
  a folder are still resolved by hand. Limit: if copying a kept folder back
  fails partway, an unchanged file not yet copied looks deleted on the
  next run and is archived after that run's preview; the run journal
  (PLAN.md §4.7) is meant to close this.
- `sync.resolve` (`id`, `path`, `keep`, `review_token`) needs the pair's
  latest preview to contain the conflict, and the token must name that
  preview — a choice cannot follow a plan another client published after
  it, and a pair update makes the held preview obsolete before keep-both
  copies anything. Otherwise it fails with `PREVIEW_REQUIRED` (-32008).
  Applying a choice spends it, and changing the pair's roots or metadata
  categories discards all choices.
- The app shows Keep A and Keep B on conflicts and previews again
  after a choice. The CLI uses `--keep-left PATH` and `--keep-right PATH`.

**Keep both** (`keep: "both"`, for files changed in both folders): the
agent first copies B's version, after checking it is still the previewed
one, to a free name beside it in B, then records "keep A".

```text
docs/report.txt  →  docs/report (from Folder B, 2026-10-18).txt
```

The date is the copied version's modification day (UTC); `2`, `3` and so
on are appended when the name is taken. The next run replaces B's
version with A's, archiving it as usual, and copies the new file to A.
The app shows Keep Both on such conflicts; the CLI uses `--keep-both PATH`.

## Per-item overrides

PLAN.md §3.8 offers, on every previewed change, "copy the other way, skip
once, always skip" (Always Skip adds a filter rule; ADR 0024).

- **Copy the other way** is a side choice on any change, not only a
  conflict: keeping the folder a change would have overwritten copies its
  version back instead. An archive becomes a restore into the folder the
  item went missing from; a copy of a new item archives it where it was
  created. The app labels these "Restore to Folder A", "Archive in Folder
  B" or "Copy the Other Way"; the CLI uses `--keep-left PATH` or `--keep-right PATH`. Blocked
  items wait for the conflict above them.
- A folder present on one side only is settled as a branch, as for folder
  conflicts. A choice for a folder present on both sides copies only its
  metadata; a file/folder mismatch is still resolved by hand.
- **Skip once** (`keep: "skip"`, `--skip PATH`) leaves a previewed change
  and everything inside it as it is — no write, no base change — in the
  pair's next run that executes, scheduled or not, while both sides still
  hold what the person saw. The agent keeps skips in memory: a restart
  forgets them, and the next preview shows the change again.

## Consequences

- A choice made from an old preview cannot overwrite a newer edit.
- Keep both always keeps A's version at the original path.
- Keep both writes before the next run: the copy stays even if the choice
  is later discarded.
