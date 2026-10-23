# ADR 0010: Streaming local metadata inventory

Status: Implemented library; content snapshots and preview integration pending

## Scope

`LocalEndpoint::scan_metadata` walks a held local root through the filesystem
driver. It emits exact root-relative name bytes, observed attributes and
symlink target bytes. It never opens regular file data or follows a symlink.
This is metadata inventory, not a content snapshot or an executable plan.

The driver can clone a root handle and hold descendant directories without
following any symlink component. The endpoint owns traversal and exclusions;
raw directory operations remain in `foldover-fs`.

## Selected root overlap

`LocalPair::open` holds both selected roots and rejects equal or nested
directories. The filesystem driver compares device/inode identities and
walks held parent directories, so selected symlink/case aliases and later
ambient-path replacements do not turn overlap into a string-prefix check.
`LocalEndpoint::relation_to` also exposes this observation for comparisons
with other selected roots.

The ancestry probe uses ambient authority only to inspect parent identities.
It exposes no parent handles, enumerates no parent entries and reads no file
contents. Lookup failures, detected cycles and the 4096-ancestor limit are
errors. Each probe retains only its current parent handle and visited IDs.

This is a selection-time geometry check. Pair-catalog overlap checks, volume
UUIDs, mount/readiness policy and execution-time revalidation remain required.
Moving a held root changes its observed relation without switching it to a
replacement directory at its original path.

## Traversal and protected paths

Rows stream depth-first in filesystem enumeration order. The scanner retains
one iterator per active directory rather than buffering and sorting a whole
wide directory before the first row. Plans must impose their own stable path
ordering. Depth does not consume the Rust call stack.

At every depth, exclude `.foldover`, the reserved `.foldover-tmp-` filename
prefix and default Finder state `.DS_Store`. Reserve ASCII case aliases too,
because the other endpoint may be case-insensitive. These internal paths
cannot become user data on another endpoint. A skipped directory's children
are never visited. Future writers must use this temporary-file namespace.

Special files (sockets, named pipes and devices) are counted as skipped and
listed as unavailable without reading them. The engine skips the same paths
and their descendants in both folders and the base for that run, preserving
existing data and baseline rows. Regular files at those paths sync again.
Entries on a different observed device and directory cycles are reported
as unavailable branches; independent entries continue. Device/inode values are session
observations, not volume UUIDs or proof of mount availability.

On Darwin, the same no-follow stat observation includes `SF_DATALESS`.
A dataless root stops the inventory. A dataless entry below it, a file or a
folder iCloud keeps only in the cloud, is listed as unavailable and left
unread, with nothing below it enumerated; the engine skips it in both folders
with everything below it on either side, and hides it from the base for the
run, so it is never read as a deletion (PLAN.md §4.10). It syncs again once
downloaded. This flag check does not replace an enforced non-materializing
I/O policy or cloud-root readiness.

On a volume that declares no native extended attributes (exFAT, FAT),
macOS keeps each file's attributes in its `._` sidecar and presents them
as the file's own, moving and removing the sidecar with it, so a scan
skips `._` files there; a folder so named is not a sidecar. Elsewhere, including volumes that do not say,
`._` names are left unchanged with an explicit sidecar-classification error:
they are neither silently omitted nor treated as ordinary content. The need for separate
filesystem and operation-fidelity probes remains as recorded in
[ADR 0005](0005-metadata-fidelity.md).

## Completion and errors

Only a finished traversal returns statistics. Scoped I/O failures and
mutations are reported with unavailable subtrees; callers discard any rows
already emitted inside them. The engine excludes those branches from both
folders and the base, protecting ancestors that might displace them.
Cancellation, root failures and failures without a bounded path discard
all rows. An unreadable directory never looks empty.

Recheck each emitted entry, every opened directory's identity/attributes and
its parent's binding when leaving it. Recheck directory attributes after its
last row too. These detect observed races; they do not create an atomic tree
snapshot. Content changes after a row, restored timestamps and watcher gaps
still require watcher coordination, content hashes and per-operation freshness
checks. Cancellation occurs between rows, not inside a blocked filesystem call.

## Verification and remaining gates

Tests exercise nested/empty folders, protected names at multiple depths,
outside/dangling links, raw names and targets, cancellation, directory-to-link
replacement, disappearing directories, special items, sidecar isolation,
last-row mutations and replacement of the root's ambient path. Linux checks
run locally; macOS coverage runs in CI.
Root-selection tests cover equal/nested roots, selected symlink aliases,
filesystem-dependent case aliases, prefix-only siblings, held-root moves,
unavailable selections and scanning through a checked disjoint pair.

Full metadata capture, naming and
capability probes, pair-catalog overlap validation, volume UUIDs, stable snapshots,
readiness gates and performance calibration remain required. Inventory and
[data-fork hashing](0012-local-content-hashing.md) remain below the agent API.
