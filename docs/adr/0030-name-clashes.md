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
- A pair uses the looser rule of its two folders. Each path's key is its
  name in composed Unicode (NFC) where normalization is ignored, lower
  case where case is; names that are not UTF-8 compare as bytes, with
  ASCII letters folded.
- Paths that differ but share a key are a `NameClash` conflict, on every
  path of the group, including one that is otherwise in sync. Nothing is
  written for them; a clashing folder blocks what is inside it. Choosing a
  side, keeping both or skipping is refused: renaming one item resolves it.

```text
A (APFS):  Report.txt      key "report.txt" ─┐
B (SFTP):  report.txt      key "report.txt" ─┴─▶ NameClash on both rows
```

## Consequences

- Two macOS folders whose files differ only in Unicode spelling (one from
  an old HFS+ disk, one created later) show conflicts until one is
  renamed; mapping one spelling onto the other, and writing new files in
  NFC on foreign filesystems, remain PLAN.md §4.10 work.
- Case folding uses Unicode lower-casing, which approximates but does not
  equal each filesystem's own folding tables.
- Pairs whose folders both match names exactly skip the check entirely.
