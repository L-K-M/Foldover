# ADR 0030: Name clashes

Status: Implemented as conflicts; name mapping between spellings pending

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
  clashes, never fewer). SFTP folders and Linux roots match bytes exactly.
- Each path's key under a folder's rule is its name in composed Unicode
  (NFC) where normalization is ignored, lower case where case is; names
  that are not UTF-8 compare as bytes, with ASCII letters folded.
- Paths that one folder keys alike are a `NameClash` conflict when they
  would meet in that folder: the plan writes or archives two of them
  there, or writes one while another stays. The conflict covers every
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

## Consequences

- Two macOS folders whose files differ only in Unicode spelling (one from
  an old HFS+ disk, one created later) show conflicts until one is
  renamed; mapping one spelling onto the other, and writing new files in
  NFC on foreign filesystems, remain PLAN.md §4.10 work.
- Case folding uses Unicode lower-casing, which approximates but does not
  equal each filesystem's own folding tables.
- Pairs whose folders both match names exactly skip the check entirely.
