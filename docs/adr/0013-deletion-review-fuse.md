# ADR 0013: Deletion review thresholds

Status: Implemented; execution binds confirmations to the held preview

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
filesystem operations. Root availability/identity, canary, space, freshness
and journal checks remain required. Sudden emptiness is an additional review
stop when removal protection is enabled. Extra confirmation binds
to the exact reviewed plan and configuration revision: a preview carries an
opaque `review_token`, a deterministic fingerprint of the pair, its revision,
the counts and every planned item's path, action, versions and bytes —
including items beyond a response page. The agent holds each pair's latest
preview, and a run executes a nonzero approval only when the presented token
names that held preview and a fresh preview still equals it. A changed or
superseded plan stops for review again rather than reusing old counts, so an
approval can never follow a preview another client published after it.
`sync.resolve` and `sync.items` bind the same way: a missing or stale token
fails with `PREVIEW_REQUIRED` before any write or page.

The owner selected two independent per-pair settings on 2026-10-25 after a live
home-folder backup repeatedly rescanned and requested approval without copying.
`Require approval before syncing` defaults off (`--review automatic`). Turning
it on (`required`) requires a reviewed plan on every run. Ordinary syncs use
one fresh observation and execute without an extra review scan.

`Stop unusually large removals` defaults on (`--removal-protection enabled`).
It preserves the numeric fuse and the sudden-empty review stop independently
of routine approval. Disabling it allows those removals under the remaining
filesystem checks. It does not disable archiving or root identity, canary,
space, lease, revision and per-file freshness checks. Explicit approval of a
stopped plan still requires the held token and an unchanged fresh plan.

## Verification

Tests cover exact and fractional boundaries, small unattended pairs, invalid
counts, maximum counts, and every valid count combination through 300 paths.
The finite model also checks that increasing deletions never clears a tripped
fuse. These tests do not establish execution or recovery safety.
