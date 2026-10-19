# Limitations

What you need to know before trusting a pair with your data.

## Safety nets

- **The first run always needs you.** Every pair's first run stops with
  its preview until you review it; automatic schedules only start after
  that ([ADR 0020](../adr/0020-run-execution.md),
  [ADR 0022](../adr/0022-scheduled-runs.md)).
- **Deletion fuse.** An interactive run stops when a target would lose
  more than 50 paths and more than 1% of what it holds; an unattended
  run stops above 50%. You confirm with the exact preview you saw; a
  changed plan needs a fresh review
  ([ADR 0013](../adr/0013-deletion-review-fuse.md),
  [ADR 0020](../adr/0020-run-execution.md)).
- **A suddenly empty folder pauses the run.** If one side holds nothing
  while Foldover remembers content there, archiving on the other side
  waits for your review ([ADR 0020](../adr/0020-run-execution.md)).
- **Identity check.** The first sync writes `.foldover/id` in each
  folder. A missing or different file stops the run, which is how an
  unmounted disk does not look like a mass deletion
  ([ADR 0020](../adr/0020-run-execution.md)). A folder you cannot write
  to cannot hold this file, so read-only folders cannot sync.
- **Stopped automatic runs wait for you.** An unattended run that needs
  your approval stays stopped until you sync by hand. The app notifies
  you while it is running; with the app closed, nothing alerts you
  ([ADR 0022](../adr/0022-scheduled-runs.md)).

## SFTP folders

- **Trust the server.** A process on the server can redirect writes
  outside the selected folder between checks; use SFTP only with servers
  you trust ([ADR 0028](../adr/0028-sftp-endpoints.md),
  [PLAN.md](../../PLAN.md) section 10 item 7).
- Only mode bits and file modification times in whole seconds are kept;
  extended attributes and ACLs are not synced, and a file with a
  resource fork fails to copy
  ([ADR 0028](../adr/0028-sftp-endpoints.md)).
- A file replaced on the server by another with the same size, time,
  mode and owner inside the same second is only noticed at the next full
  read ([ADR 0028](../adr/0028-sftp-endpoints.md)).
- Only OpenSSH's `sftp-server` is tested; other servers may order
  symlink arguments differently
  ([ADR 0028](../adr/0028-sftp-endpoints.md)).
- Full durability is not available
  ([ADR 0028](../adr/0028-sftp-endpoints.md)).

## Archives and Undo

- **Archives expire.** Replaced and removed items live in
  `.foldover/archive/<run>/` inside each folder for 30 days, then are
  deleted permanently. That pruning is the only path that deletes data
  ([ADR 0020](../adr/0020-run-execution.md)).
- **Put Back never replaces.** A restored item goes beside an occupied
  path as `name (restored).ext`, then `(restored 2)`; the next sync
  copies it to the other folder like any new entry
  ([ADR 0025](../adr/0025-archive-browser.md)).
- **Undo covers only the newest run that wrote.** It restores content a
  run created, replaced or archived, but never metadata, which is not
  kept, and never the other folder's side. Entries changed since the
  run, already pruned, or in a folder no longer empty stay and are
  listed ([ADR 0029](../adr/0029-undo.md)).
- Empty folders are not recreated by Undo
  ([ADR 0029](../adr/0029-undo.md)).

## Filters and limits

- **One filter for both sides.** Pair rules and both folders'
  `.foldoverignore` files combine and apply to both folders. An excluded
  entry reads as absent on both sides and is forgotten, not deleted;
  removing the rule makes it appear as new on both sides
  ([ADR 0024](../adr/0024-filters.md)).
- `.foldover`, Foldover temporaries and `.DS_Store` are always excluded
  and cannot be switched off ([ADR 0024](../adr/0024-filters.md)).
- A `.foldoverignore` that is not UTF-8 stops the run
  ([ADR 0024](../adr/0024-filters.md)).
- **Size limit**: a file over the limit in either folder is skipped in
  both. **Age limit**: a file unchanged longer than the limit is skipped
  only when it is old or absent in both folders; editing either side
  makes it sync again ([ADR 0024](../adr/0024-filters.md)).
- **Always Skip** writes an exact-path rule; names that are not UTF-8
  cannot be written and are not offered
  ([ADR 0024](../adr/0024-filters.md)).
- **Skip once lives in memory.** An agent restart forgets it and the
  next preview shows the change again
  ([ADR 0023](../adr/0023-conflict-resolution.md)).
- A folder deleted on one side cannot be removed on the other while it
  still holds excluded entries; that removal fails and is reported
  ([ADR 0024](../adr/0024-filters.md)).
- AppleDouble sidecar files (`._*`) inside a synced folder stop the
  scan; add a `._*` rule to skip them instead
  ([ADR 0024](../adr/0024-filters.md),
  `crates/foldover-endpoint/src/local.rs`).

## Conflicts

- **Keep Both** copies Folder B's version beside the original as
  `name (from Folder B, DATE).ext`, then Folder A wins at the original
  path. The copy is written before the next run and stays even if you
  discard the choice ([ADR 0023](../adr/0023-conflict-resolution.md)).
- Keep and skip choices apply only while both sides still match what you
  previewed ([ADR 0023](../adr/0023-conflict-resolution.md)).
- A file/folder type mismatch needs you to resolve it by hand
  ([ADR 0023](../adr/0023-conflict-resolution.md)).

## What never syncs

- Owner and group, creation time, link counts, ctime and inode numbers.
- Directory and symlink modification times.
- Lock and append-only flags; storage flags such as compression and
  data-vault markers.
- System-managed attributes: `com.apple.lastuseddate#PS`,
  `com.apple.macl`, `com.apple.provenance` and `com.apple.rootless`.
- Hard links arrive as independent files.

([ADR 0018](../adr/0018-local-entry-versions.md))

## Watching and schedules

- FSEvents and inotify events are hints; each one starts a full
  observation, and "When files change" also verifies hourly
  ([ADR 0022](../adr/0022-scheduled-runs.md)).
- A mounted share only reports changes your Mac makes. Writes from other
  computers are found by the hourly check or your interval; SMB
  notification behavior is unverified
  ([ADR 0002](../adr/0002-change-detection.md)).
- Schedule timers live in agent memory; restarting the agent restarts
  every interval ([ADR 0022](../adr/0022-scheduled-runs.md)).

## Not yet

Planned in [PLAN.md](../../PLAN.md) but not implemented:

- **Mac-to-Mac pairs.** There is no peer folder type; the README's
  "Mac to Mac" highlight and the QUIC transport in `foldover-net` are
  not wired into sync.
- **Sparkle updates.** Builds update by downloading and re-running the
  activation steps by hand.
- **FSEvents replay after remount.** Replay is a hint only; a remount or
  a changed store identity forces a full rescan, and replay continuity
  is unproven ([ADR 0002](../adr/0002-change-detection.md)).
- **Low Power Mode, battery and metered-network policy.** Scheduled runs
  do not yet adapt to power or network state
  ([PLAN.md](../../PLAN.md) section 3.5).
- **Unicode and case-insensitive name matching.** Names are compared
  byte-exact; NFC/NFD matching and name-collision handling on
  case-insensitive volumes are unvalidated
  ([ADR 0005](../adr/0005-metadata-fidelity.md)).
- **Root leases.** Nothing stops two Macs' agents from syncing the same
  share; Foldover has no multi-Mac lock on a folder.
- **Per-volume capability probing.** Foldover does not yet test what a
  chosen folder can store before the first write; unsupported metadata
  fails the entry instead of being predicted
  ([ADR 0005](../adr/0005-metadata-fidelity.md),
  [ADR 0017](../adr/0017-filesystem-feature-declarations.md)).
- **Verified write fidelity beyond APFS.** exFAT, physical disks and
  network volumes are untested write targets
  ([ADR 0019](../adr/0019-local-write-primitives.md)).
