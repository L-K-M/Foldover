# ADR 0005: Filesystem support is not copy fidelity

Status: Tested baseline rejected; full S4 open

## Context

[PLAN.md §8 S4](../../PLAN.md#8-risks-and-spikes) requires every metadata
change to be predicted by endpoint capabilities. Filesystem support alone
does not establish that a chosen copy operation preserves that feature.

## Method

`spikes/s4-metadata` creates a case-sensitive APFS source image and separate
APFS/exFAT target images. It probes target operations before copying ten
corpus cases with `ditto --rsrc --extattr --acl` and back. Snapshots include
directory-root metadata, file hashes, raw names, link topology, tags, forks,
FinderInfo, ACLs, flags and sparseness.

Known name collisions and unsupported entry kinds are blocked before copy.
Unknown probes cannot excuse losses. Copy, type, content and cleanup
failures remain explicit. A failed directory cleanup inside an owned image
is recorded before the image is detached and deleted; failures on existing
mounts still abort. Twenty-one logic/boundary tests accompany the harness.

## Results

[Run 39026036276](https://github.com/L-K-M/Foldover/actions/runs/39026036276),
commit `9230d29`, macOS 27.0.1 arm64, Xcode 27.0. The harness completed and
cleaned up its images. Both tested profiles failed the no-unpredicted-loss
gate; `full_s4_passed` was false.

| Observation | APFS target | exFAT target |
|---|---|---|
| Fractional mtime probe | 1 ns stored exactly | 10 ms minimum observed step |
| POSIX mode probe | Preserved | Not preserved; loss predicted |
| Combined tags/fork/FinderInfo/ACL/flags case | No differences | Copy failed with EINVAL |
| Resource-fork xattr probe | Supported | EINVAL, marked unsupported for this API |
| ACL probe | Supported | Tool reports unsupported; measurement remains unavailable |
| Hard-link group | Preserved | Unsupported; blocked before copy |
| Standalone symlink argument | Copied as a regular file | Copied as a regular file |
| Sparse file | Became dense despite filesystem support | Dense, as predicted by probe |
| NFC/NFD names | Both preserved | NFC case listed as NFD; NFD case preserved |
| Case pair | Collision blocked before copy | Collision blocked before copy |
| Bundle tree | No differences | Only predicted mode/mtime changes |

The exFAT NFC case also resisted both Python and native recursive cleanup
with ENOTEMPTY. The final run retained the remaining NFD name bytes and
marked the operation as failed, then removed the owned filesystem through
image detach/deletion. The cause is not established. The NFD-creation probe
had reported no normalization, so it did not predict the NFC behavior.

Samba with/without fruit and SFTP were unavailable. These are image and
native-tool observations, not results for physical disks or Rust drivers.

## Decision

- Do not use the tested `ditto` invocation as a general sync primitive.
  Dispatch explicitly by entry kind; copy links as links and never follow
  their targets.
- Validate each driver's operation fidelity separately from filesystem
  support. Sparse copies need a tested hole-preserving or clone path.
- Probe metadata fields separately. Warn or block before unsupported
  metadata would be lost; the resource-fork result does not rule out a
  separately tested AppleDouble representation.
- Probe NFC and NFD creation, listing, lookup and operations separately.
  Preserve observed raw names and compare through separate normalized keys.
  Do not enable unvalidated Unicode writes on the affected exFAT path.
- Retain per-side stored metadata in the base to prevent recopy loops.

## Remaining gates

Production-driver round trips, Unicode operation recovery, physical APFS/
exFAT, Samba ± fruit, SFTP extensions/hard links and crash atomicity remain
unverified. This baseline does not close S4.
