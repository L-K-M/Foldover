# ADR 0040: Backup targets without records

Status: Implemented

## Context

Every folder in a pair holds Foldover's records in `.foldover`: the
identity file the volume check reads (PLAN.md §4.7), the lease that
keeps two Macs from writing at once, the archive of replaced and
removed files, and partial copies to resume. Some folders refuse
`.foldover`: shares that veto names starting with a dot, upload-only
SFTP folders, appliances that allow only plain files. Every run on
such a folder failed writing its identity.

PLAN.md §3.6: such a folder can only be a Backup target, and only
after the person accepts that files replaced there are not archived.

## Decision

A pair's `records` option is `both` (default) or `source`
(`FolderRecords::SourceOnly`). Only a Backup to a local or SFTP folder
may choose `source`; another Mac's folder always keeps records, since
its agent writes them.

```text
             source                  target
records      .foldover: id, lease    nothing
identity     its id file             known to this agent only
lease        taken                   none
replaced     (Backup never writes)   deleted, not archived
leftovers    archived                deleted
resume       ·                       none: copies start over
undo         refused for the pair
```

- **Probe.** `pair.check` writes a probe file in each folder's
  `.foldover` and removes it, with `.foldover` itself when the probe
  created it. A folder that refuses gets the `recordsUnsupported`
  warning. A target already keeping nothing is not probed, so the
  check writes nothing there.
- **Runs.** The target's identity is a random id recorded in the
  agent's state only. Every batch still reads the target's free space
  through its held root, so a lost volume stops the run. Leases are
  taken on the source only. The target's run scope deletes what a
  placement displaces and any `.foldover-tmp-*` leftover, and receives
  copies without keeping partials. Retention pruning skips it.
- **Storage.** Schema 23 adds `pair.records`. Changing records, or the
  mode while records stay with the source, forgets both identities, so
  the next run is a first run again: an identity only this agent knew
  must never be checked against a folder's id file. Older readers ignore
  the column and keep records in both folders, which such a target
  refuses, so the minimum reader stays.
- **App.** The editor blocks saving while a folder refuses records and
  is not a Backup target keeping nothing; for a Backup target it offers
  "Keep nothing in Folder B", explaining what goes. Undo is hidden for
  such pairs. The CLI takes `--records both|source`.

## Consequences

- Files a Backup replaces in such a target are gone; nothing in the
  target's archive browser, and no Undo.
- The volume identity check cannot run there. The canary file, the
  suddenly-empty gate and the held root still catch a wrong or missing
  folder.
- Another Mac backing up to the same target is not noticed.
- Large copies to it that break off start over.
- The simulation draws this setup for half its Backup histories: the
  target never gains an archive, and every other invariant holds.
