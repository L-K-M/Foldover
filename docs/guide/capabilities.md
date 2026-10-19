# What each folder kind can do

A pair joins two folders. Each side can be:

- **Local folder**: a path on a volume attached to your Mac, such as APFS.
- **Mounted network share**: a folder under `/Volumes` that macOS mounted
  over SMB, NFS or WebDAV (macOS 27 has no AFP client). Foldover treats it
  as a local folder; what the share actually keeps depends on the mounted
  filesystem, which Foldover does not probe
  ([ADR 0027](../adr/0027-share-reconnect.md),
  [ADR 0005](../adr/0005-metadata-fidelity.md)).
- **SFTP folder**: a folder on a server Foldover reaches through OpenSSH
  ([ADR 0028](../adr/0028-sftp-endpoints.md)).

| | Local folder | Mounted share | SFTP folder | Source |
|---|---|---|---|---|
| File content | Data and resource fork synced; a fork change is a content change | Same code path; what the share keeps depends on its filesystem | Data only; a file with a resource fork fails the copy | [0018](../adr/0018-local-entry-versions.md), [0028](../adr/0028-sftp-endpoints.md), [0005](../adr/0005-metadata-fidelity.md) |
| Modification times | File times synced; folder and link times never | Same code path; precision is whatever the filesystem stores | File times only, in whole seconds | [0018](../adr/0018-local-entry-versions.md), [0028](../adr/0028-sftp-endpoints.md) |
| Permissions and ACLs | Mode bits and the macOS ACL when you enable Permissions; a file's executable bit always follows the source | Same code path; fidelity depends on the share | Mode bits only; ACLs are not synced | [0018](../adr/0018-local-entry-versions.md), [0019](../adr/0019-local-write-primitives.md), [0028](../adr/0028-sftp-endpoints.md) |
| Extended attributes | Synced by default: tags, Finder info, comments, quarantine, hidden and no-dump flags | Same code path; many share filesystems cannot store macOS attributes | Not synced | [0016](../adr/0016-local-extended-metadata.md), [0018](../adr/0018-local-entry-versions.md), [0028](../adr/0028-sftp-endpoints.md) |
| Lock and append-only flags | Never synced or written | Never synced or written | Never synced or written | [0018](../adr/0018-local-entry-versions.md), [0019](../adr/0019-local-write-primitives.md) |
| Symlinks | Copied as links; the target is never followed | Same code path | Copied as links; tested against OpenSSH's server only | [0018](../adr/0018-local-entry-versions.md), [0028](../adr/0028-sftp-endpoints.md) |
| Hard links | Each path arrives as an independent file; link counts are never tracked | Same | Same | [0018](../adr/0018-local-entry-versions.md) |
| Empty folders | Created on the other side | Same | Same | [0019](../adr/0019-local-write-primitives.md), [0028](../adr/0028-sftp-endpoints.md) |
| Archive and Put Back | Replaced and removed items go to `.foldover/archive/<run>/` for 30 days; Put Back restores without replacing | Same | Same | [0020](../adr/0020-run-execution.md), [0025](../adr/0025-archive-browser.md), [0028](../adr/0028-sftp-endpoints.md) |
| Undo | Reverses the newest run that wrote; metadata-only changes are not undone | Same | Same | [0029](../adr/0029-undo.md) |
| Change detection | FSEvents hints plus a full check every run; hourly verification on "When files change" | FSEvents reports only this Mac's writes; other computers' writes are found by the hourly check or your interval | No notifications; hourly verification on "When files change" | [0002](../adr/0002-change-detection.md), [0022](../adr/0022-scheduled-runs.md), [0028](../adr/0028-sftp-endpoints.md) |
| Durability | Standard (`fsync`) or Full (`F_FULLFSYNC`, slower, also survives power loss) | Same settings; the guarantee is limited to what the mounted filesystem honors | Standard only; Full is rejected when you save the pair | [0019](../adr/0019-local-write-primitives.md), [0028](../adr/0028-sftp-endpoints.md) |
| Identity checks | First sync writes `.foldover/id`; later runs stop if it is missing or different | Same | Same | [0020](../adr/0020-run-execution.md), [0028](../adr/0028-sftp-endpoints.md) |

## Notes

- **Metadata options are per pair.** Modification times and extended
  attributes sync by default; permissions do not. Changing a pair's roots
  or metadata categories resets its sync history
  ([ADR 0018](../adr/0018-local-entry-versions.md),
  [ADR 0011](../adr/0011-pair-configuration-api.md)).
- **"Same code path" is not a guarantee.** A mounted share uses exactly
  the local implementation, but Foldover has not yet measured what real
  SMB, NFS or WebDAV shares preserve, so the table marks those rows as
  dependent on the filesystem
  ([ADR 0005](../adr/0005-metadata-fidelity.md),
  [ADR 0019](../adr/0019-local-write-primitives.md)).
- **Never synced anywhere:** owner and group, creation time, lock and
  append-only flags, storage flags, and system-managed attributes such as
  `com.apple.provenance`. See [limitations](limitations.md)
  ([ADR 0018](../adr/0018-local-entry-versions.md)).
- **Full durability is not offered for SFTP.** SFTP file sync cannot flush
  namespace changes or the drive cache, so pairs containing an SFTP
  folder only offer Standard ([ADR 0028](../adr/0028-sftp-endpoints.md)).
- **Watching is a hint, not the record.** An event only starts a full
  observation; interval schedules and the hourly check cover what events
  miss ([ADR 0002](../adr/0002-change-detection.md),
  [ADR 0022](../adr/0022-scheduled-runs.md)).
