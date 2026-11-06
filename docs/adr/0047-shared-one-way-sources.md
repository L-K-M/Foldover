# ADR 0047: Shared Mirror and Backup sources

Status: Implemented

## Context

A person backing up one folder to two targets, an external disk and a
NAS, needs two pairs with the same source. Saving the second failed
with `ROOT_OVERLAP` (-32006): PLAN.md §3.6 blocked any folder another
pair used, the same, inside or around it, and the error named the
other pair by its ID.

Lifting the block alone would not have been enough:

- Every run leased both folders ([ADR 0032](0032-root-leases.md)),
  though a Mirror or Backup only reads its source. The second backup
  waited 150 s for the first, then failed.
- A failed result stopped the pair's schedule until someone synced it
  by hand ([ADR 0022](0022-scheduled-runs.md)). ADR 0032 meant a busy
  folder to be retried on the schedule's cadence.
- Archive run folders were named by start time alone, so pairs sharing
  a folder listed and pruned each other's runs. Two runs of one agent
  starting in the same millisecond also shared a lease holder name.

## Decision

**Roles.** A pair uses each folder in one role (`PairDraft::role`): a
Mirror's or Backup's *source*, its *target*, or either folder of a
two-way Sync.

**Catalog.** Two pairs may use the same folder, or one inside the
other, only when both use it as a source. Anything else that overlaps
is refused, including the roots a pending edit still reserves, in the
role they had: a job finishing a two-way configuration keeps writing
both folders. The refusal names the other pair:

```text
A chosen folder is, holds or sits inside a folder of “Photos”; pairs
share folders only as Mirror or Backup sources
```

`pair.check` (local protocol 18) takes `{ "id"?, "configuration" }`
and answers `{ "kind": "overlap", "side", "pairName" }` for each side
that saving would refuse. `id` names the pair being edited, which never
overlaps itself. An existing pair whose folder will not open is left to
saving, which fails closed as before ([ADR 0011](0011-pair-configuration-api.md));
[ADR 0048](0048-missing-folders-in-catalog-checks.md) compares a missing one
by where it would be and reports an unreadable one before saving.
The editor blocks Save while an overlap is reported, as it does for a
folder refusing records ([ADR 0040](0040-backup-targets-without-records.md)).

**Leases.** A run leases the folders it writes. A source is written
only when a person's choice copies a change back into it ("Copy the
Other Way", [ADR 0023](0023-conflict-resolution.md)), or to recover
temporaries an interrupted write left there. Only then is it leased,
and only then are its leftovers recovered and its archive pruned.
Otherwise its `.foldover-tmp-*` files may be another writer's live
writes, and are left alone. Undo still leases both folders, since the
run it undoes may have written either. A Backup target keeping no
records is unchanged: no lease, leftovers deleted.

| Folder | Leased | Leftovers recovered | Archive pruned |
|---|---|---|---|
| Two-way, or a target | always | yes | yes |
| Source, only read | no | no | no |
| Source, written or with leftovers | yes | yes | yes |
| Backup target keeping nothing | no | deleted | no |

**Archive runs.** A run's folder is its UTC start time, its pair and a
unique run ID:

```text
.foldover/archive/2026-10-18T09-30-00.123Z_0123456789abcdef0123456789abcdef_0123456789abcdef0123456789abcdef/docs/report.txt
                  └ start time ─────────┘ └ pair ─────────────────────────┘ └ run ID ────────────────────────────┘
```

The time comes first, so names still sort by it; equal-time names use
the full name as a deterministic tie-break, not a chronological order.
The ID keeps runs sharing a start millisecond, including after a restart
or clock rollback, from sharing an archive or journal. A run folder's
name is also the lease holder's run name, so two pairs never share a holder. Names of the time alone, or of
the time and pair, written before the ID, remain readable; names of the
time alone belong to every pair.

- **Listing.** A pair's archive shows its own runs and those without a
  pair, never another pair's; asking for another pair's run by name
  finds nothing.
- **Pruning.** A run prunes, by its own clock, its pair's runs and runs
  without a pair after 30 days, and other pairs' runs only after 60. A
  Mac whose clock runs ahead cannot take another pair's runs early, yet
  the runs of a pair that no longer exists still go.
- **Another Mac's folder.** The Mac sharing a folder prunes all of it by
  its own clock, as before ([ADR 0031](0031-mac-to-mac.md)). Requests
  still carry run names as strings, but peer protocol 3 refuses a Mac
  of version 2: it cannot parse the run ID in the new folder names, so
  it would take writes into them, then fail to list them, put them back
  or undo from them. Version 2 refuses version 1 the same way over the
  pair in the name.

**Busy folders.** A run that gives up on a folder another run keeps
leased (`RunError::folder_busy`) still fails with "<Mac> is syncing
this folder", but no longer stops the schedule: the pair runs again
when it next comes due. Any other failure, an approval stop or an undo
still waits for a person. The app notifies about a failure once, not
again for the same message while retries keep failing.

## Consequences

- A backup of a folder another pair or Mac is writing may copy a file
  mid-change. The copy checks the file is unchanged and otherwise
  leaves it to the next run, as when a person edits it.
- A source with leftovers is leased to recover them, so a backup that
  finds another writer's live temporaries there waits for its lease as
  before.
- Nested sources hold different leases. Two people copying changes back
  into both at the same moment could make one run recover the other's
  live temporary; it is archived, never lost, and the other run reports
  that path.
- A busy failure on an interval schedule retries after one interval; on
  a changes schedule, on the next change or the hourly check; on a
  daily schedule, the next day.
- Paired Macs must both run this version or newer; until then each
  refuses the other with "That Mac runs another Foldover version; update
  both Macs".
- A Mac with an older Foldover syncing the same share or server folder
  neither lists nor prunes the new run folders there until it is
  updated; they are not its runs.
- Every new run folder name is 66 bytes longer than the original
  time-only names: 33 for the pair, 33 for the run ID.
