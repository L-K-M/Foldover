# ADR 0056: Post-write snapshots

Status: Implemented for local folders ([plan](../resume-interrupted-runs.md), W2);
SFTP follows

## Context

A run stores each side's observation before it writes anything
(ADR 0052), so every file it copies or updates has no stored
fingerprint. The next run reads each of them again to prove its
version, though nothing changed: after a first sync of a large folder,
the second sync rereads everything the first one wrote. A run continuing
an interrupted one (ADR 0055) rereads everything the stopped run wrote.

The version a write returns comes from the staged temporary file's stat,
before the rename that places it. The local stat key holds the file's
identity and change time, which the rename can change, so that stat
cannot prove the placed file.

## Decision

- **Writes report what they left.** `Endpoint::copy_written` and
  `update_metadata_written` return a `Written`: the stored version and,
  where the endpoint can prove it, the entry as an observation after
  placement would record it. `copy_from` and `update_metadata_from`
  keep returning only the version. Only local folders report an
  observation so far; SFTP and peer sides report none.
- **Local: a stat after placement.** After the rename, and after the
  previous content is set aside, the endpoint stats the placed path the
  same way a scan does. It reports the observation only if the file
  still has the staged file's identity, size and modification time;
  anything else means another writer got there first, and the next run
  reads the file as before. Directories and symbolic links report the
  entry the write placed or created.
- **Stored with the base.** The engine keeps each reported observation
  whose version equals the version it records, and commits it in the
  same transaction as the base batch (`change_base_observed`). A stale
  revision stores neither. The rows replace the stored ones for those
  paths; the observation stored before the run has already been
  written by then, so it cannot overwrite them.
- **Settling is unchanged.** A fingerprint taken within the 3 s racy
  window of the file's modification time is never trusted, and the
  weekly reread still applies, so a copy of a freshly edited file is
  first reused after a confirming read in a later run.

## Consequences

- The run after a sync reuses every version it wrote to a local folder
  without reading the files, and finds no observation change to store.
- Moves still report only a version; a moved file is read again by the
  next run.
- Undo does not yet drop the snapshot rows of paths it restores. For
  local folders this cannot reuse a wrong version: a put-back renames
  the archived file into place, which changes its change time, so the
  stored key no longer matches. SFTP stat keys hold no change time, so
  the SFTP part of this work drops them.
- Not verified on SMB shares; the device matrix covers APFS, HFS+,
  exFAT and FAT32 for the stat key a scan records, and the post-write
  stat uses the same code.
