# ADR 0018: Local entry versions

Status: Accepted for the local backend; snapshot reuse and agent exposure pending

## Context

The planner compares `EntryVersion`s: content that must match across
endpoints, and a metadata digest recorded per side
([ADR 0007](0007-entry-reconciliation.md)). Scans
([ADR 0010](0010-local-metadata-inventory.md)), content hashes
([ADR 0012](0012-local-content-hashing.md)) and extended metadata
([ADR 0016](0016-local-extended-metadata.md)) were raw observations.

## Decision

`LocalEndpoint::observe` scans the root, then derives one version per entry.
The root itself is excluded. Each read is checked against the entry's scan
observation, so any change, read error or cancellation fails the whole
observation instead of returning a partial tree.

**Content** (domain-separated BLAKE3):

| Kind | Content |
|---|---|
| File | Data-fork digest plus the resource fork's size and digest, if present |
| Directory | Constant |
| Symlink | Target bytes; the target is never followed |

Resource forks are content, so a fork edit cannot be resolved as a
metadata-only conflict. On compressed files the fork and
`com.apple.decmpfs` are compression storage and are ignored; decompressed
reads already cover the data.

**Metadata digest:** permission bits (not links), file mtime, user flags,
xattr names/sizes/digests and the ACL. Excluded:

- directory and symlink mtimes: they move with children and are rarely
  settable, so tracking them would cause churn;
- owner and group: an unprivileged agent cannot set them;
- creation time, link counts, ctime and inode: storage facts, not content;
- storage flags (compression, tracking, data vault, system flags);
- system-managed xattrs: `com.apple.lastuseddate#PS`, `com.apple.macl`,
  `com.apple.provenance`, `com.apple.rootless`.

Linux symlinks report no extended metadata, because their xattrs cannot be
read through a held descriptor. This is a distinct digest input, not an
empty attribute list.

## Consequences

- Equal bytes and forks produce equal content on any local endpoint.
- Metadata differences between endpoints are expected and stored per side.
- Every observation reads all content. Snapshot reuse (same size, mtime,
  ctime and file ID) is the next optimization.
- A single entry changing during observation fails the run; the caller
  retries. Per-entry retry is pending.
- Execution must read back the target's version and fail explicitly when
  content differs, for example a dropped resource fork.
