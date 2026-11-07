# ADR 0050: Borrowed planning and unchanged snapshot handoff

Status: Implemented

## Context

Preparing a run cloned both observations into new version maps solely to give
the planner read access. Snapshot export built another map of references,
then cloned paths/fingerprints; the state layer cloned these fields again
and allocated each encoded version separately. The service did this for both
sides even when the effective snapshot was identical, and built a full preview
for interactive runs with no held review, token or approval.

## Decision

- A private version projection lets the same planner read existing version
  maps or borrowed observation maps. Public candidate-planning signatures and
  all decision/error semantics remain. Only output rows own new paths.
- Merge the excluded-entry cache and current observation directly in path
  order. Current entries win duplicate paths, as before. Compare that effective
  snapshot with the previous one before serialization; unchanged sides skip
  persistence. Additions, edits, removals and filtered-entry retention count.
- Release the previous snapshot maps before planning. Observations own the
  entries reused from them; keeping both generations alive is unnecessary.
- The database accepts opaque snapshot field views. State borrows paths and
  fingerprints and keeps encoded versions inline, avoiding three per-row
  allocations. The owned database API remains available; transaction revision
  checks, sorting, duplicate rejection and changed-row-only writes are shared.
- Build the service's interactive preview only when there is a held review,
  supplied token or approval. Execution still creates the fresh preview when
  returning a gate that requires review. ADR 0013's exact plan binding stays.

## Validation

The unchanged-snapshot regression failed with unconditional persistence and
passes with the effective-snapshot comparison. It also covers an empty other
side, and additional tests cover edits, additions, removals and retained
age-excluded entries. Engine planner properties, deterministic simulation,
undo/recovery tests and the service's existing approval/token tests use the
same production path. Database/state snapshot tests exercise the borrowed
handoff and the existing general owned-row API.

No complete-run or peak-RSS improvement is claimed from source inspection.
The change eliminates entire intermediate trees and per-row field clones;
qualification must still measure the whole preparation, preview and apply
peak against PLAN section 5.3's 500 MB limit at one million tracked files.
Input decoding under the engine mutex and residual encoded snapshot storage
remain candidates for measurement and follow-up work.
