# ADR 0019: Local write primitives

Status: Driver and endpoint operations implemented; execution pending

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
- **Volumes without the rename flags:** HFS+ and exFAT answer `ENOTSUP`
  to both, and macOS's FAT driver takes a swap as a plain rename over
  the destination, losing what it held (found by the device matrix, ADR
  0038). On macOS the flags are used only where the volume declares them
  (`VOL_CAP_INT_RENAME_EXCL`, `VOL_CAP_INT_RENAME_SWAP`). The same FAT
  driver lists names decomposed and finds files by either spelling, but
  renames only from the composed one; a rename a decomposed name missed
  is tried again composed, on macOS only. A no-replace
  rename becomes an exclusive placeholder of the entry's kind at the
  destination, which a plain rename then replaces; the refusal holds. An
  exchange becomes PLAN.md §4.7's two steps: the old entry moves aside
  under a temporary name, then the staged one moves in, and a failure in
  between moves the old one back. An agent that dies between the steps
  leaves both under temporary names, which the next run archives: nothing
  is lost, but the path stays empty until a run fills it. One that dies
  between a placeholder and its rename leaves the placeholder, empty,
  under the destination's name: a new empty file for the next run to
  sync, or an identity file that counts as missing and is written again.
  `EINVAL` answers take the same fallbacks.
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

## Endpoint operations

`LocalEndpoint::copy_from`, `update_metadata_from` and `remove` execute one
planned change. Each first proves both sides still hold what the plan
observed, then stages, checks that the staged content equals the source's
planned content, rechecks the destination's stat, places and archives.
Changes since the preview fail with `SourceChanged` or `DestinationChanged`
and write nothing; content a target cannot store (such as a dropped resource
fork) fails with `ContentNotStored`. Each operation returns the version
actually stored, for the base.

Observations keep each entry's stat as a fingerprint. An identical stat
proves the version unchanged without rereading data, unless the entry was
modified within three seconds of the observation: coarse FAT/exFAT
timestamps cannot show an edit within one tick. Otherwise the version is
recomputed.

Archives go to `.foldover/archive/<run>/<path>`. Scans report leftover
temporaries; `recover_leftover` archives them, because one may hold a
displaced original.

## Consequences

- A crash leaves either the old or the new entry at each path. A displaced
  entry may remain under a temporary name; the next observation reports it
  and recovery archives it.
- The parent check before placement narrows, but does not close, the window
  in which a moved parent directory could receive the entry.
- Removing an ACL on the source may not clear the target's; `fcopyfile`'s
  behavior there is unverified.
- Untested: physical devices, network volumes and exFAT targets.
