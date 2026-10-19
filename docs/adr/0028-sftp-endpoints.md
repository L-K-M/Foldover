# ADR 0028: SFTP folders

Status: Implemented; tested against OpenSSH's `sftp-server`, not yet
against other servers

## Root-safety decision

Owner decision, 2026-10-19: support standard SFTP writes on trusted servers.
Path validation and version checks remain mandatory, but SFTP has no dirfd or
atomic no-follow operation: another server process can swap an intermediate
directory for a symlink between checks. The app and CLI must warn before writes.
A server-side jail or Foldover helper is not required. Local and peer roots keep
their held-root isolation contract.

## Decision

Either side of a pair can be a folder on an SFTP server, written as a
locator kept in the same root column and field as local paths:

```text
sftp://alice@nas.local:2222/srv/photos   absolute path
sftp://backup/~/photos                   below the login's home folder
```

**Transport.** Foldover runs `/usr/bin/ssh -o BatchMode=yes -s -- host
sftp` and speaks SFTP version 3 on its streams (`foldover-net::sftp`).
OpenSSH handles keys, the agent, `~/.ssh/config`, known hosts and jump
hosts; batch mode means it never asks for a password, so a login that
needs one fails with ssh's own message. Names are raw bytes. Reads and
writes keep 16 requests of 32 KiB in flight.

**Same rules as local roots** (ADR 0019, 0020):

- writes go to `.foldover-tmp-…` beside the target, are checked against
  the planned content digest, and are then renamed into place;
- creating never replaces (version 3 renames refuse an existing target);
- a replacement first renames the current version into
  `.foldover/archive/<run>/`, then the new one into place, so the path is
  briefly empty; if the second rename fails, the old version waits in the
  archive;
- a source or target that changed since the plan is not overwritten;
- the identity file, filters, `.foldoverignore`, size and age limits,
  retention, archive browsing, put-back and keep-both work the same way.

**What differs.**

| | Local root | SFTP folder |
|---|---|---|
| Metadata kept | mode, times, xattrs, flags, ACL | mode, modification time in whole seconds |
| Unchanged-stat proof | full stat incl. inode | unavailable; contents and link targets are re-read |
| Change notifications | FSEvents / inotify | none; hourly verification on "When files change" |
| Free space | `fstatvfs` | `statvfs@openssh.com`, else unchecked |
| Durability "Full" | `F_FULLFSYNC` | unsupported: file fsync cannot flush namespace changes or guarantee drive-cache flush |

Versions are computed the same way, so content compares across kinds; a
local file with a resource fork cannot be stored on a server and fails
with "did not store the content exactly". Copies between two local roots
keep their native path; any copy touching a server streams through
Foldover.

SFTP Standard uses the server's acknowledged-write policy. Full is rejected
before writes: `fsync@openssh.com` only applies to files, so it cannot provide
the local endpoint's power-loss-safe rename and directory-flush contract.

**Availability.** An unreachable server, or one refusing the login, is
waited for like an unmounted disk on automatic runs (ADR 0022) and fails
manual runs with ssh's message. Adding or changing a pair connects to its
own servers but compares other pairs' SFTP folders by locator, so an
offline server never blocks the catalog.

## Consequences

- Servers other than OpenSSH may order `SYMLINK` arguments differently;
  links there are untested.
- Without inode numbers, a file replaced by another with the same size,
  time, mode and owner within the same second is only noticed by the next
  full read.
- The tests need an `sftp-server` binary; they run on macOS
  (`/usr/libexec/sftp-server`) and wherever `FOLDOVER_SFTP_SERVER` points.
