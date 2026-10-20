# Limitations

What you need to know before trusting a pair with your data.

## Safety nets

- **The first run always needs you.** Every pair's first run stops with
  its preview until you review it; automatic schedules only start after
  that ([ADR 0020](../adr/0020-run-execution.md),
  [ADR 0022](../adr/0022-scheduled-runs.md)).
- **Deletion fuse.** An interactive run stops when a target would lose
  both more than 50 paths and more than 1% of its paths; an unattended
  run stops only when it would lose more than half its paths. You confirm with the exact preview you saw; a
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
- **Canary file.** A pair can name, in the editor, a file it must find
  unchanged in both folders. If the canary is missing or changed on
  either side, the sync stops before anything is written, which
  catches a wrong folder or a disk that never mounted.
  The check compares content only, so a re-touched file still passes,
  and an edited canary blocks every run until it is put back
  ([ADR 0033](../adr/0033-canary-file.md)).
- **Stopped automatic runs wait for you.** An unattended run that needs
  your approval stays stopped until you sync by hand. The app notifies
  you while it is running; with the app closed, nothing alerts you
  ([ADR 0022](../adr/0022-scheduled-runs.md)).

## Readiness warnings

While you edit a pair, Foldover warns about sides that will not work as
you might expect. Warnings never block saving
([ADR 0037](../adr/0037-readiness-warnings.md)):

- A folder another sync engine manages: iCloud Drive, Desktop &
  Documents in iCloud, or a provider folder such as Dropbox.
- A side that cannot keep the extended attributes you chose to sync; an
  SFTP side always warns.
- Two sides that compare names differently, one ignoring case and one
  exact; such names become conflicts to rename.

## Several Macs on one folder

- **One sync at a time.** Every run holds a lease, the file
  `.foldover/lock` inside each folder, from before its first write until
  it ends, so two Macs never write the same folder at once. Undo takes
  the same leases ([ADR 0032](../adr/0032-root-leases.md)).
- **Waiting, then takeover.** A run that finds another Mac's live lease
  waits with "Waiting for <Mac> to finish syncing these folders…" and
  fails with "<Mac> is syncing this folder" after 150 seconds;
  scheduled runs retry on their cadence. A Mac that crashed or slept
  mid-sync holds its folders for up to two minutes; the next run waits
  that out and takes over ([ADR 0032](../adr/0032-root-leases.md)).
- After a takeover the old holder may still write for up to one beat,
  15 seconds, before it stops; staged writes and archived replacements
  keep that window recoverable ([ADR 0032](../adr/0032-root-leases.md)).
- A run on an SFTP folder opens a second SSH connection for its leases,
  and an SFTP server whose rename replaces an existing file breaks the
  exclusive create: two Macs could both take a free lease
  ([ADR 0032](../adr/0032-root-leases.md)).

## Mac-to-Mac

- **ACLs do not travel yet.** Extended attributes, resource forks and
  file flags do, at most 2 MiB of attributes per entry
  ([ADR 0031](../adr/0031-mac-to-mac.md)).
- **Only the owner syncs.** The Mac that owns a pair reconciles it; the
  other serves its shared folder, and the pair waits while the owner is
  offline ([ADR 0031](../adr/0031-mac-to-mac.md)).
- A copy between two Macs' folders relays through your Mac
  ([ADR 0031](../adr/0031-mac-to-mac.md)).
- Agents listen and advertise on IPv4 only. Where multicast is blocked,
  pair by typing the other Mac's address and pairing port
  ([ADR 0031](../adr/0031-mac-to-mac.md)).

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
- **Skip once lives in memory.** Restarting the agent (the background
  process that runs syncs for the app and the CLI) forgets it, and the
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
- **Name clashes.** Names that one folder would store as one file, such
  as `Report.txt` and `report.txt` on a case-insensitive Mac volume,
  are a conflict to rename; keeping a side, keeping both or skipping is
  refused, and case folding approximates the filesystem's own tables
  ([ADR 0030](../adr/0030-name-clashes.md)).

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

- FSEvents events (inotify on Linux) are hints; each one starts a full
  observation, and "When files change" also verifies hourly
  ([ADR 0022](../adr/0022-scheduled-runs.md)).
- A mounted share only reports changes your Mac makes. On "When files
  change", writes from other computers are found within 5 minutes at the
  share's top level and in folders recently active, and otherwise by the
  hourly check; SMB notification behavior is unverified
  ([ADR 0002](../adr/0002-change-detection.md)).
- Schedule timers live in agent memory; restarting the agent restarts
  every interval ([ADR 0022](../adr/0022-scheduled-runs.md)).
- **Daily schedules.** A pair can sync at a time of day on chosen
  weekdays in the Mac's local time zone, like "Weekdays at 18:00" or
  "Every day at 02:30". A schedule missed while the Mac slept or the
  agent was off runs once when they are back, and the time follows
  daylight saving ([ADR 0039](../adr/0039-calendar-schedules.md)).
- **Automatic syncs wait in Low Power Mode** unless "Sync automatically
  in Low Power Mode" is on in Settings. Pairs that reach a folder over
  the network (SFTP, another Mac or a mounted share) also wait while
  the network is expensive or Low Data Mode is on. Syncs you start
  yourself always run, and held pairs run on the first tick after the
  hold lifts ([ADR 0036](../adr/0036-environment-policy.md)).
- A local folder Foldover once mounted from a share keeps counting as
  network use until a sync observes it on plain disk again
  ([ADR 0036](../adr/0036-environment-policy.md)).

## Updates

- **Check for Updates…** in the app menu. Updates arrive through Sparkle
  and each must carry the release key's signature
  ([ADR 0035](../adr/0035-sparkle-updates.md)).
- Builds from before the release keys existed cannot update themselves;
  download the next release by hand for those. Each update replaces the
  app and its embedded agent together
  ([ADR 0035](../adr/0035-sparkle-updates.md)).

## Not yet

Planned in [PLAN.md](../../PLAN.md) but not implemented:

- **Name mapping between spellings.** Folders that compare names
  differently turn clashing spellings into conflicts to rename; mapping
  one spelling onto the other is not implemented, so two folders whose
  files differ only in Unicode spelling show conflicts until one is
  renamed ([ADR 0030](../adr/0030-name-clashes.md)).
- **FSEvents replay after remount.** Replay is a hint only; a remount or
  a changed store identity forces a full rescan, and replay continuity
  is unproven ([ADR 0002](../adr/0002-change-detection.md)).
- **Per-volume capability probing.** Foldover does not yet test what a
  chosen folder can store before the first write; unsupported metadata
  fails the entry instead of being predicted
  ([ADR 0005](../adr/0005-metadata-fidelity.md),
  [ADR 0017](../adr/0017-filesystem-feature-declarations.md)).
- **Verified write fidelity beyond APFS.** exFAT, physical disks and
  network volumes are untested write targets
  ([ADR 0019](../adr/0019-local-write-primitives.md)).
