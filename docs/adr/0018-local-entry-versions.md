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
observation. Scoped changes and read errors leave explicit unavailable
subtrees, excluded on both sides of the plan; they never look absent.
Cancellation and root failures discard the whole observation.

Directory membership changes do not discard readable entries. Their listings
carry an unstable-directory marker, including at the root: omitted names
cannot prove deletion or baseline removal until a later stable scan. Creates
still check destination absence at the write boundary; uncertain renames use
copies while preserving the original. Directory metadata is refreshed under
the same held identity before capture. File and link versions retain exact
freshness checks, and directory substitutions still leave the subtree unread.

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

**Metadata digest:** only the categories the pair syncs (owner decision,
2026-10-18). Each is configurable per pair:

| Category | Default | Covers |
|---|---|---|
| Modification times | Sync | File mtimes |
| Extended attributes | Sync | Xattr names/sizes/digests (tags, Finder info, comments, quarantine), hidden and no-dump flags. A pair with a folder on a network volume leaves `com.apple.quarantine` out on both sides and never writes it (PLAN.md §4.10); another Mac serving a side says whether its folder is on one, and gets the decision with each run |
| Permissions | Ignore | Mode bits (not links) and the ACL; when ignored, a file's executable bit still syncs |

An ignored category is absent from the digest, so changing it never causes
an update or conflict. Each category's state is also encoded, so digests
computed under different policies never match.

Never synced:

- directory and symlink mtimes: they move with children and are rarely
  settable, so tracking them would cause churn;
- owner and group: an unprivileged agent cannot set them;
- creation time, link counts, ctime and inode: storage facts, not content;
- lock and append-only flags: local protection choices that would block
  later sync writes, and in two-way sync would block the other side;
- storage flags (compression, tracking, data vault, system flags);
- system-managed xattrs: `com.apple.lastuseddate#PS` (changes on every
  open), `com.apple.macl` (per-Mac app access grants), `com.apple.provenance`
  (set by macOS for files from downloaded apps) and `com.apple.rootless`.

`EntryMetadata::attribute_role` in the driver classifies xattrs; versions
and writes ([ADR 0019](0019-local-write-primitives.md)) share it. On Linux
only `user.` xattrs are synced.

Linux symlinks report no extended metadata, because their xattrs cannot be
read through a held descriptor. This is a distinct digest input, not an
empty attribute list.

## Consequences

- Equal bytes and forks produce equal content on any local endpoint.
- Metadata differences between endpoints are expected and stored per side.
- Observations reuse the previous run's version for every entry whose
  complete stat is unchanged (state schema v7 stores each side's last
  observation). The stat must have been at least three seconds old when
  that version was computed, because coarse timestamps cannot show an edit
  within one tick. A version is read again once it is three and a half to
  seven days old, at an age fixed by its stat so a large folder spreads the
  work: an edit that kept size and modification time is found within a
  week (PLAN.md §4.8). Snapshots are discarded with the base when roots or
  metadata categories change.
- An entry changing during observation is reported and left unchanged;
  independent entries continue. Failed entries are retried next run.
- Execution must read back the target's version and fail explicitly when
  content differs, for example a dropped resource fork.
