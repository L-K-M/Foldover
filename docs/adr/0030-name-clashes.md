# ADR 0030: Name clashes

Status: Implemented as conflicts; normalization respellings mapped

## Context

Folders compare names differently (PLAN.md §4.10). macOS volumes ignore
Unicode normalization, so `café` with a composed or a decomposed `é` is one
file, and most ignore case, so `Report.txt` and `report.txt` are one file.
A Linux SFTP server keeps all four as separate files. The planner matched
paths byte for byte: copying one spelling onto a folder that held the other
failed on every run, or would have stored two files where a person sees one.

## Decision

- Each folder declares how it compares names: a local root on macOS ignores
  normalization, and ignores case unless its volume declares case-sensitive
  names (an unreported answer counts as ignoring case, which flags more
  clashes, never fewer). Linux roots match bytes exactly; SFTP folders
  keep case but ignore normalization
  ([ADR 0041](0041-composed-names-on-sftp.md)).
- Each path's key under a folder's rule is its name in composed Unicode
  (NFC) where normalization is ignored, lower case where case is; names
  that are not UTF-8 compare as bytes, with ASCII letters folded.
- Paths that one folder keys alike are a `NameClash` conflict when they
  would meet in that folder: the plan writes or archives two of them
  there, or writes one while another stays, there or in a conflict
  whose resolution may write it there. The conflict covers every
  path of the group, including one that is otherwise in sync. Nothing is
  written for them; a clashing folder blocks what is inside it. Choosing a
  side, keeping both or skipping is refused: renaming one item resolves it.
- A folder that keeps the spellings apart takes them as planned, so a
  case-only rename on a Mac reaches an SFTP server as an archive of the
  old spelling and a copy of the new one.

```text
A (APFS):  Report.txt   B (SFTP): report.txt   both would be copied to A,
                                               which keys them alike:
                                               NameClash on both rows
A renames Report.txt → report.txt              B archives Report.txt and
                                               copies report.txt
```

- A folder that ignores normalization may store a name in another one
  than it was given: HFS+ decomposes. An entry the base knows in one
  spelling, observed there only in another that composes alike, counts
  as the base's spelling, so the respelling is no change; the folder
  finds the file by either. Case is never mapped: a case-only rename is
  a person's, and syncs as one. The simulation found the respelling,
  and a spelling copied beside another in conflict
  ([ADR 0038](0038-simulation.md)).

## Consequences

- Two macOS folders whose files differ only in Unicode spelling (one from
  an old HFS+ disk, one created later) show conflicts until one is
  renamed. SFTP folders write new names composed and find either
  spelling ([ADR 0041](0041-composed-names-on-sftp.md)).
- Case folding uses Unicode lower-casing, which approximates but does not
  equal each filesystem's own folding tables.
- Pairs whose folders both match names exactly skip the check entirely.
