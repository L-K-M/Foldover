# ADR 0013: Deletion review thresholds

Status: Implemented pure policy; plan and execution integration pending

## Counts and policy

`DeletionCounts` validates the counts supplied by a complete target snapshot.
Evaluate each target separately, using its existing root-relative paths as the
denominator. Source-only paths and proposed creations cannot dilute the fuse.
Each proposed deletion path counts once, including descendants of an archived
directory. Content replacement is not a deletion; its old version still
requires archiving.

[PLAN.md §3.6](../../PLAN.md#36-safety-defaults) defines separate thresholds:

- Interactive: more than 50 paths **and** more than 1% of existing target paths
  requires extra confirmation. This is the higher of the two thresholds.
- Unattended: more than 50% stops for review, including on pairs smaller
  than 50 paths.

Equality does not trip either threshold. Integer cross-products use `u128`,
so fractional thresholds and counts up to `u64::MAX` remain exact. Invalid
counts fail before assessment; an empty analysis permits zero deletions only.

## Boundary

`WithinLimits` reports only this fuse's requirement. It does not authorize
filesystem operations. Root availability/identity, sudden emptiness, canary,
space, first-run preview, freshness and journal gates remain required.
Extra confirmation must bind to the exact reviewed plan and configuration
revision. A cached pair-level acceptance must not authorize a changed plan.
The agent and app do not expose executable plans yet.

## Verification

Tests cover exact and fractional boundaries, small unattended pairs, invalid
counts, maximum counts, and every valid count combination through 300 paths.
The finite model also checks that increasing deletions never clears a tripped
fuse. These tests do not establish execution or recovery safety.
