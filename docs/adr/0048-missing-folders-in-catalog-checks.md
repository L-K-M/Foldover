# ADR 0048: Missing folders in catalog checks

Status: Implemented

## Context

Saving a pair compares its folders with every configured pair's
([ADR 0011](0011-pair-configuration-api.md), [ADR 0047](0047-shared-one-way-sources.md)).
A configured local folder that would not open stopped every change:

```text
Agent error -32005: Reconnect the roots of pair 1317ee49c3c945038eec11598eb116ea
before changing the catalog: The folder is missing; connect its disk or share
```

A backup whose target disk sits in a drawer therefore blocked adding any
other pair, and the error named the pair by its ID. The pair editor's
check left such pairs out, so only Save said so. ADR 0011 failed closed
"until persistent volume and endpoint identities are implemented". They
are now: every synced folder carries `.foldover/id`, and every run checks
it before writing ([ADR 0015](0015-held-volume-uuid.md)).

## Decision

A configured local folder is compared in one of three ways:

| Folder | Compared | When it cannot be compared |
|---|---|---|
| There | By held identity and ancestry, as before | · |
| Missing: nothing at its path, e.g. its disk not mounted | By where it would be, and by its identity file | · |
| There but unreadable, e.g. access refused | Not at all | Saving refuses; the editor says so |

- **Where it would be.** `Root::relation_to_missing` opens the deepest
  ancestor of the missing path that exists and compares it by held
  identity, so no spelling of that ancestor evades the check. A chosen
  folder that is that ancestor, or above it, would hold the missing one
  once it is back, and overlaps. Anything else is beside it: the first
  missing name is in no folder, so nothing chosen can be inside it.
- **Its identity file.** If the pair synced, it recorded the folder's
  identity. A chosen folder carrying that identity is the same folder,
  mounted or moved elsewhere, and overlaps.
- **Unreadable folders** still fail closed. The error names the pair and
  the folder:

  ```text
  Can't check the chosen folders against “Photos backup”, whose folder
  /Volumes/Backup/Photos can't be read: …
  ```

  `pair.check` (local protocol 19) reports such a pair as
  `{ "kind": "unverified", "side", "pairName" }` for each side that
  cannot be checked, and the editor blocks Save, naming the pair once.

Roles still decide first: a folder both pairs only read as Mirror or
Backup sources is never compared.

## Consequences

- A pair whose disk or share is away no longer blocks other changes.
- A chosen folder that holds the missing folder at another path, say a
  parent of the disk mounted elsewhere, is not caught: only the chosen
  folder's own identity file is read, not every folder below it. Its
  runs would still check their own identities, but two pairs could both
  write the missing folder once it is back. The same applies to a
  missing folder that never synced, which has no identity file to match.
- A chosen folder inside the missing folder at another path is not
  caught either, for the same reason.
