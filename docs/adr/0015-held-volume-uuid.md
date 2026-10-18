# ADR 0015: Held-root volume UUID observations

Status: Implemented library probe; native verification and readiness integration pending

## Scope

`Root::volume_uuid` observes a macOS filesystem UUID through held directory
descriptors. It does not reopen the root's ambient path or infer an identity
from a volume name, device/inode pair or FSEvents-store UUID.

Darwin's [volume attribute API](https://keith.github.io/xcode-man-pages/getattrlist.2.html)
requires the physical volume root. The driver walks held parents, using
`ATTR_CMN_DEVID` with `FSOPT_RETURN_REALDEV` so APFS volume groups cannot hide
a physical mount boundary. Ancestor handles remain private to this metadata
probe. Cycles, more than 4096 ancestors and observed device changes are errors.

At the mount root, request only returned-attribute masks and `ATTR_VOL_UUID`.
The audited binding uses initialized C-layout buffers and verifies returned
lengths and attribute masks. Unsupported queries and malformed replies remain
errors. Missing or nil UUIDs return `None`; non-macOS hosts currently return
`None` rather than manufacture a persistent UUID. The probe enforces the
non-materializing thread policy in [ADR 0012](0012-local-content-hashing.md).

## Boundary

A UUID observation is not mount-continuity proof or permission to execute.
Persisted expected identities, `.foldover/id`, mount events, current root binding,
capability probes, canaries, space checks and per-batch revalidation remain
required. Mounted shares also need backend-specific identity policy.
The agent exposes no volume/readiness or execution method yet.

## Verification

Native tests cover malformed lengths, missing/unknown attribute masks and nil
UUIDs, compare a temporary root's UUID with Foundation, and retain its observation
after the ambient root moves. These require macOS CI and do not establish
external-device remount, forced-unmount or share behavior.
