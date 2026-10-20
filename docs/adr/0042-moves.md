# ADR 0042: Moves

Status: Implemented for files and links; folders move by their contents

## Context

PLAN.md §4.7 enriches plans: "moves: same file ID or hash → rename", and
§4.9 promises that moves transfer nothing. A file moved in one folder
was planned as an archive of its old path and a copy to its new path on
the other folder: a reorganized photo library was copied again in full,
and every old path went through the deletion fuse.

## Decision

- **Pairing.** After conflicts and blocked branches are settled, the
  planner pairs each archive on a folder with a create on that folder of
  the same file or link content (`EntryContent` digest), one to one in
  path order. The archive row becomes `MovedAway`, the create row `Move`,
  and the plan records where each move comes from. Moved-away paths are
  not deletions for the fuse. Folders carry no content digest, so a
  renamed folder is created anew, its files move into it, and the
  emptied old folder is removed as before. Backup never removes, so it
  never moves.
- **Execution.** A `Move` row renames on the target with
  `Endpoint::move_entry`, which proves the old path still holds what was
  observed and never replaces anything at the new path (local
  `renamex_np(RENAME_EXCL)`, SFTP's non-replacing rename, composed names
  on SFTP as ADR 0041). The moved entry keeps the target's metadata; when
  the source's differs, a metadata update follows. The base forgets the
  old path and records the new one.
- **Peers.** Another Mac's folder takes no moves yet: its plans copy and
  archive, so an older agent there never meets a request it lacks.
- **Undo.** The journal records a `move` with its source path and the old
  path's base (schema 24, which raises the minimum reader). Undo renames
  the entry back when the new path still holds what the run left and the
  old path is free, recreating the old folder if the run removed it;
  otherwise the entry is listed as left as it is.
- **Preview and reports.** A move shows once, at its new path, with where
  it comes from; counts, run results and history count moves apart.

## Consequences

- A move of identical content transfers nothing and needs no review.
- Duplicate files pair in path order; any pairing gives the same content.
- File identity (inode) is not used, so a move whose content also
  changed is still an archive and a copy.
- The simulation's random renames now run as moves on every folder kind
  it models; nothing lost accepts a version found at another path of the
  same folder, as each write has its own content.
