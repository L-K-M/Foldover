# ADR 0007: Entry reconciliation and per-side baselines

Status: Accepted (classification only)

## Context

[PLAN.md §4.7](../../PLAN.md#47-state-and-reconciliation) requires a
three-way comparison. [Recorded reality](../../PLAN.md#410-metadata-and-names)
requires retaining what each endpoint actually stored, including accepted
metadata loss. One shared metadata fingerprint would cause copy loops on
lossy targets.

## Decision

`foldover-types` holds immutable content/metadata fingerprints and mode,
side, change and conflict enums. `BaselineEntry` retains both endpoints'
versions and rejects differing content or kinds. Metadata may differ.

`foldover-engine::reconcile_entry` classifies one fully observed path:

- Compare each side with its own baseline.
- Without a base, Sync creates missing entries and never infers deletion.
- Concurrent edits, deletion versus editing, type clashes and metadata-only
  disagreements become conflicts. Identical results can update the base.
- Mirror restores the configured source to its target. Backup keeps
  target-only entries and independent target edits until its source changes;
  it still fills a missing target. Neither mode propagates target edits back.
- Content replacement requires archiving the previous version. Removal is
  represented only as archive, never bare deletion.

Absence means a completed scan found no entry. Scan errors and unavailable
roots must not enter this classifier as absence.

## Verification

Seven tests cover the classification table, first-run behavior, lossy
metadata, baseline validation and Backup target edits. A finite model checks
side-swap symmetry, concurrent-edit preservation, archive eligibility,
source authority and type safety across three content hashes, three metadata
hashes, all entry kinds, valid per-side baselines and five mode/direction
combinations. The Backup regression failed before its fix.

## Remaining work

This is not an executable plan. Tree dependencies, name collisions, gates,
freshness checks, journal, archive, filesystem execution and crash recovery
remain required. It does not satisfy the full simulation or dogfood gates.
