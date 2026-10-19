# ADR 0027: Reconnecting network shares

Status: Implemented for SMB, AFP, NFS and WebDAV volumes on macOS; the
mount itself is untested against a real server in CI

## Decision

Every run that reads a pair records, per side, where that root's volume
is mounted from (state schema v12), read with `fstatfs` on the held root:

| Filesystem | Mounted from | Recorded |
|---|---|---|
| `smbfs` | `//alice@nas.local/Photos` | `smb://alice@nas.local/Photos` |
| `afpfs` | `//nas/Photos` | `afp://nas/Photos` |
| `nfs` | `nas:/export/media` | `nfs://nas/export/media` |
| `webdav` | `https://host/dav` | unchanged |

When a root is missing and its side has a recorded source, Foldover mounts
it again with `NetFSMountURLSync`, UI disabled: NetFS uses the credentials
the user saved in their Keychain, as Finder's Connect to Server does, and
never shows a dialog from the background agent.

- Scheduled runs try at most every 5 minutes per pair and otherwise wait
  ([ADR 0022](0022-scheduled-runs.md)). A manual run or preview tries once
  before reading.
- Success only means the folder is back when its path exists again; a
  share mounted elsewhere, e.g. `/Volumes/Photos-1`, still counts as
  missing. The identity file then confirms it is the same folder before
  anything is written ([ADR 0020](0020-run-execution.md)).
- A side's source is forgotten when its root changes. Local volumes record
  none.
- The NetFS binding is a small `unsafe` module in `foldover-os`, behind
  `foldover_endpoint::mount_share`.

## Consequences

- Shares that need a password the Keychain does not hold stay missing;
  mounting them once in Finder with "Remember this password" fixes that.
- Linux reports no sources and never mounts.
