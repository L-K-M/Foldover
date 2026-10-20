# ADR 0038: Simulation

Status: Implemented over local folders; in-memory and quirk endpoints,
network chaos and the 100k-seed nightly gate pending

## Context

PLAN.md §5.2 asks for the whole engine under random histories, with
crashes at every journal transition, and invariants checked after each
step; the 1.0 gate is 14 nights of at least 100k seeded runs without a
violation. The engine had example-based tests only.

## Decision

`run::tests::simulation` drives a Sync pair over two real temporary
folders with proptest:

```text
start: three files, first run ──▶ rounds of
  edits  write / delete / rename on either side (eight nested paths)
  run    finishes, is cancelled after N checkpoints, or dies at base
         commit N with its writes done; a dead or cancelled run is
         followed by one that finishes
  check  nothing lost:  every version that left a folder is in that
                        folder's archive
         converged:     after a finished run without failures, both
                        folders match outside reported conflicts
```

- Each write carries new content, so the archive check is exact.
- Real folders run the production local endpoint, staging and archive.
  The price is speed: about one case per second, so CI runs 8 and the
  nightly job 5000 (`FOLDOVER_SIMULATION_CASES`).
- proptest shrinks a failure to a minimal history and replays it from
  its seed.

## Consequences

- The 100k-seed gate needs an in-memory endpoint; it would also bring
  the quirk file system (case folding, decomposed names, coarse times,
  no atomic replace, ENOSPC) and crashes inside a write.
- Backup mode, SFTP and peer folders are not simulated yet.
