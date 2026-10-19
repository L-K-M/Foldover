# ADR 0017: Held-volume filesystem feature declarations

Status: Read-only library; operation fidelity and readiness pending

## Contract

`Root::filesystem_features` reads Darwin `ATTR_VOL_CAPABILITIES` through the
held physical volume root. It shares the UUID probe's bounded ancestry walk,
physical-device checks and non-materializing policy from
[ADR 0015](0015-held-volume-uuid.md). It never resolves the root's ambient path.
`LocalEndpoint::filesystem_features` also checks the selected root's identity,
residency and before/after metadata.

Each feature has three states: `Supported`, `Unsupported`, `NotReported`.
Darwin's format/interface words and per-bit validity masks are independent.
An invalid bit is unreported even when the capability bit is set. A valid
cleared bit is an explicit unsupported declaration.

The fixed C reply is initialized and length/mask checked. Missing capability
attributes remain unreported; malformed replies and I/O errors remain errors.
Other platforms currently return unreported declarations, without guessing
from filesystem names. No probes create files or change volume state.

The report covers case sensitivity/preservation, symlinks, hard links, sparse
files, persistent object IDs, native xattrs, ACLs, named streams, file cloning,
exclusive rename and swap rename. Persistent object-ID support does not prove
that stat inode values cannot recycle.

## Boundary

Filesystem declarations do not establish driver operation fidelity
([ADR 0005](0005-metadata-fidelity.md)). In particular, sparse support does not
prove that a copy preserves holes, and exclusive/swap rename declarations do
not prove archive or replacement crash safety.

Unicode normalization, SFM mapping, mtime granularity, metadata round trips,
permissions, space, expected endpoint identities, mount continuity and cloud
readiness still need separate probes/policy. These observations are live,
not a cache valid across remounts or capability errors. The agent exposes no
capability or execution method yet.

## Verification

Tests cover validity/capability combinations for every feature, word isolation,
missing attributes, malformed lengths/masks, unsupported-platform reports and
held-root replacement. Native tests compare layouts/constants with the SDK,
declarations with Foundation and case behavior with owned temporary directories.
The existing native UUID/held-root tests also bracket the shared-walk extraction.
Physical disks, shares, forced unmounts and production write fidelity remain open.
