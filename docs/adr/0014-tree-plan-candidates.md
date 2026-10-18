# ADR 0014: Hierarchy-aware plan candidates

Status: Implemented pure planner; snapshot and preview integration pending

## Inputs and output

`plan_candidates` accepts validated root-relative paths and complete logical
left/right trees, plus the last-synced base. Every non-root entry must have a
directory parent in its own tree. Missing or non-directory parents are errors,
including in the base. Callers must not turn incomplete scans, filtered paths,
unavailable placeholders or read failures into absence.

The planner merges three ordered iterators into exact byte-path order without
building another union map. The existing entry reconciler classifies each path.
Directory indexes support hierarchy checks; traversal does not recurse.
Cancellation during validation or planning returns no complete candidate.

## Conflicted branches

Entry-level reconciliation cannot safely authorize a directory archive by
itself. In Sync mode, a new or edited surviving descendant changes an ancestor's
archive candidate into a deleted-versus-edited conflict, even when the directory's
own metadata still matches its base. Mirror retains its explicit source authority.

Every descendant of a conflicted directory is blocked by the outer conflict.
This includes copies, archives and base repairs. File-versus-directory conflicts
therefore cannot create files below a conflicting parent, and resolving one row
cannot silently swallow another row's surviving data.

Archive candidates retain every affected descendant path. Deletion review uses
each target's existing path count and the final, unblocked archive candidates;
see [ADR 0013](0013-deletion-review-fuse.md). An eventual executor must choose
operation grouping and children-first archival without double-processing paths.

## Boundary

`CandidatePlan` contains classifications, not executable operations. It does not
prove snapshot freshness, metadata fidelity, naming compatibility, volume identity
or readiness. Plan identity/revision binding, overrides, operation ordering,
journaling and recovery remain required. The agent exposes no planning method yet.

## Verification

Tests cover new/edited children beneath deleted directories, unchanged deletions,
Mirror extras, first-run parent creation, type conflicts, deep branch suppression,
invalid tree shapes, raw-byte ordering and cancellation at every checkpoint.
A finite tree model checks side symmetry and conflict blocking across Sync,
Mirror and Backup.

Proptest also generates independently edited directory/file/symlink trees,
including raw-byte paths. It checks complete ordered row coverage, one-way
authority, Backup's no-deletion rule, changed-survivor protection, conflicted
branches and side symmetry. Failures shrink to edit histories and persist
regression seeds. A 100,000-case local release run passed:

```sh
PROPTEST_CASES=100000 cargo test -p foldover-engine --release generated_trees_preserve_plan_safety
```

These are planner properties. They do not replace the whole-engine crash,
network and filesystem simulation or its 14-night acceptance gate.

The opt-in release measurement builds 500,000 paths across 5,000 directories.
On the Linux development host with Rust 1.99.0, candidate generation took
354.1 ms, excluding input generation. Run it with:

```sh
cargo test -p foldover-engine --release half_million_path_candidate_measurement -- --ignored --nocapture
```

This is a planner measurement, not a scan, native Preview, memory-budget or
physical-Mac acceptance result.
