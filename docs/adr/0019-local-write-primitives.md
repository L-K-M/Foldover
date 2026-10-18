# ADR 0019: Local write primitives

Status: Driver implemented; endpoint verification, journaling and execution pending

## Context

Execution needs writes that never lose data, keep every path holding either
its old or its complete new entry (PLAN.md §5.1 invariants 1, 2 and 5), and
apply only the metadata a pair syncs ([ADR 0018](0018-local-entry-versions.md)).

## Decision

All writes are relative to held directories and never follow links.

```text
create:   .foldover-tmp-X ──rename(no-replace)──▶ name
replace:  .foldover-tmp-X ◀──rename(exchange)──▶ name
          (the displaced old entry waits at .foldover-tmp-X for archiving)
archive:  name ──rename(no-replace)──▶ .foldover/archive/…/name
```

- **Staging:** a file's data streams from the held source into an exclusive
  temporary beside its destination, hashed as it is written. The source must
  match its observation before, during and after the copy. Scans skip
  temporaries, and an unplaced one is removed when dropped.
- **Placement:** `renameat2`/`renameatx_np` with no-replace for creates and
  exchange for replacements. The staged parent must still be the
  destination's parent. An exchange returns the displaced entry; it holds
  user data and is never removed automatically.
- **Archiving:** a no-replace rename into private (0700) directories under
  `.foldover/archive/`, only while the entry still matches its observation.
- **Directories:** created with `mkdirat`; removed only when empty.
- **Nothing is unlinked** except empty directories and unplaced temporaries.

**Metadata** follows the pair's policy:

| Category | Sync | Ignore |
|---|---|---|
| Resource fork (content) | Always copied to new files, in 1 MiB positional chunks | Same |
| Extended attributes | Copy synced xattrs and hidden/no-dump flags; on updates, remove stale ones first | Leave the target's alone |
| Permissions | Copy mode bits (never setuid, setgid or sticky) and the macOS ACL | New entries get umask defaults; a file's executable bit follows the source |
| Modification times | Copy file mtime last | New files keep their write time |

Lock and append-only flags, ownership and system-managed attributes are never
written. Which xattrs count as fork, compression storage, system-managed or
user metadata is decided in one place, `EntryMetadata::attribute_role`. On
Linux only `user.` xattrs are synced; ACLs and labels need privileges.

**Durability:** `Standard` uses `fsync` for files and directories. `Full`
uses `F_FULLFSYNC` on macOS, which also flushes the drive's cache; Linux
`fsync` already does.

## Consequences

- A crash leaves either the old or the new entry at each path. A displaced
  entry may remain under a temporary name; recovery must archive it, so the
  journal has to record those names.
- The parent check before placement narrows, but does not close, the window
  in which a moved parent directory could receive the entry.
- Removing an ACL on the source may not clear the target's; `fcopyfile`'s
  behavior there is unverified.
- Untested: physical devices, network volumes and exFAT targets.
