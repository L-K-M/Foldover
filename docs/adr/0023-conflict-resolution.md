# ADR 0023: Conflict resolution

Status: Implemented for files and links

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
- Choices involving a folder are not applied: a folder conflict is
  resolved through the entries inside it, or by hand.
- `sync.resolve` (`id`, `path`, `keep`) needs the pair's latest preview to
  contain the conflict; otherwise it fails with `PREVIEW_REQUIRED`
  (-32008). Applying a choice spends it, and changing the pair's roots or
  metadata categories discards all choices.
- The app shows Keep A and Keep B on file conflicts and previews again
  after a choice. The CLI uses `--keep-left PATH` and `--keep-right PATH`.

## Consequences

- No "keep both" yet; it needs a naming scheme for the second copy.
- A choice made from an old preview cannot overwrite a newer edit.
