# ADR 0041: Composed names on SFTP servers

Status: Implemented for SFTP; SFM names open

## Context

PLAN.md §4.10: new files on foreign file systems are written in NFC.
An HFS+ disk stores names decomposed (NFD), and some apps write them
that way, so a Mac pair could put `café` on a server in a spelling
Linux and Windows tools show and sort apart from the composed one they
type. SFTP servers match names byte for byte, so until now Foldover
wrote the bytes it was given, and declared SFTP folders to match names
exactly (ADR 0030).

Writing another spelling than the pair's breaks byte matching: the
next scan finds the composed name, and every operation on the pair's
decomposed one misses the file. Recording both spellings per side would
touch observations, the base, the journal and every endpoint operation.

## Decision

An SFTP folder behaves like a macOS volume that ignores normalization:

- New names are written composed: folders, files, links, conflict
  copies and put-back items. A replacement keeps the name the server
  already has, as its folder lists it: a server that ignores
  normalization, as a Mac's does, finds the entry by either spelling.
- An entry is looked up by its spelling as given first; only when that
  is missing, component by component in its composed and decomposed
  spellings. A name with one spelling, such as any ASCII name, costs no
  extra request.
- Filter rules and `.foldoverignore` lines compare composed with
  composed names on every folder, so a rule leaves a name out on both
  sides whichever spelling each stores.
- The folder declares names that ignore normalization but keep case.
  The engine's existing mapping (ADR 0030) then takes the composed
  spelling it reads back for the pair's, so the next run has nothing to
  do, and two spellings meeting there become a name clash instead of
  two files.

```text
pair path    café/menu.txt   (from an HFS+ disk)
on server    café/menu.txt    written composed
next scan    café/menu.txt ─▶ mapped to the pair's spelling: no change
```

Local folders keep their own rules. macOS's SMB client already sends
composed names to the server; a mounted NFS share is treated like a
macOS volume, which a byte-exact server may not match.

## Consequences

- Names a server already holds decomposed stay so; only names Foldover
  creates are composed.
- A server folder holding both spellings of a name as two files shows a
  name clash when a run would write either.
- The simulation draws such a folder (case kept, names stored composed)
  among its quirks.
- SFM names, Windows-illegal characters a Mac stores on SMB shares as
  private-use characters (U+F0xx), are neither decoded nor flagged yet.
