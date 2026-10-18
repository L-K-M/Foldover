# ADR 0016: Bounded local extended-metadata observations

Status: Read-only library; file versions and agent exposure pending

## Contract

The same no-follow stat now records permissions, owner/group IDs, link count,
and, on macOS, BSD flags and creation time. Unreported fields remain `None`.
These fields participate in freshness checks; none is inferred from names.

`Root::read_extended_metadata` checks the observed entry, opens a held leaf,
checks its descriptor, captures xattrs/ACLs, then rechecks the descriptor and
root-relative binding. `read_root_extended_metadata` includes the root itself.
On Linux, reopen `.` through the held directory because cap-std may use an
`O_PATH` descriptor, which cannot read xattrs. Neither path reopens an ambient root.

Attributes retain exact name bytes, sizes and BLAKE3 value digests, ordered by
name. Empty values differ from absent names. Re-list after reading and reject
changed sizes, short reads and changed stat observations. Permission errors,
unsupported filesystems, malformed lists, cancellation and limits are errors,
never empty metadata or partial success.

Darwin bindings request compression attributes too, stream resource forks,
and copy ACLs into their portable big-endian external encoding. A missing ACL
is explicit; unsupported retrieval is an error. All unsafe remains in one
audited macOS driver module with owned ACL cleanup and `SAFETY` comments.
The non-materializing policy in [ADR 0012](0012-local-content-hashing.md)
applies after callbacks and before native reads.

Darwin opens symlinks with `O_SYMLINK` alone; combining it with `O_NOFOLLOW`
returns `ELOOP`. Attributes belong to the link, not its
target. Linux symlink xattrs remain unsupported because `fgetxattr` cannot
use an `O_PATH` link descriptor. They fail explicitly instead of falling back
to ambient paths. Linux POSIX ACLs are observed through their native xattrs;
Darwin ACLs are `NotReported` there.

## Bounds

- Name list: 64 KiB; duplicate, empty and unterminated names are invalid.
- Ordinary value: 16 MiB, one allocation at a time.
- Resource fork: 64 KiB chunks, at most `u32::MAX` bytes, matching Darwin's
  positional xattr API. Larger forks fail before reading.
- Aggregate attribute values: `u32::MAX + 16 MiB` bytes per entry.
- External ACL: 64 KiB.

Cancellation runs between attributes and resource-fork chunks. It cannot
interrupt a blocked filesystem call.

## Boundary and verification

Endpoint access preserves protected-name, AppleDouble and device checks.
These are raw observations, not canonical cross-endpoint metadata or copy
fidelity. Capability-selected fields, ownership mapping, compression policy,
AppleDouble, naming, stable snapshots and mount/cloud readiness remain required.
File versions must include resource-fork digests in content identity, so fork
edits cannot be discarded by metadata-only conflict policy.
No metadata or content read is exposed by the agent.

Tests cover exact empty/binary values, raw-name parsing, limits, cancellation,
same-mtime xattr edits, stale entries, special files, held-root replacements,
root metadata and protected endpoint access. macOS tests additionally cover
tags/FinderInfo, multichunk forks, growth/shrink during reads, link isolation,
ACL changes, BSD flags, SDK constants and callback thread policy. Physical
devices, File Provider accounts, shares and production write fidelity remain open.
