# ADR 0038: Simulation

Status: Implemented over local and in-memory quirk folders, with the
100k-seed nightly run; network chaos and the 14-night record pending

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

### In-memory folders

`foldover-sim` provides `MemoryEndpoint`, an `Endpoint::Memory` folder
behind `foldover-endpoint`'s `simulation` feature, which only tests turn
on. Each simulated folder draws its file system:

| Quirk | Choices |
|---|---|
| Names | byte-exact; case ignored (APFS); case ignored and stored decomposed (HFS+) |
| Modification times | exact; 2-second resolution (FAT) |
| Replace | one swap; two steps, old out before new in (plain SFTP) |
| Size | unlimited; a capacity in a dedicated test |

- Paths include spellings only some folders tell apart: `a.txt` and
  `A.txt`, a composed and a decomposed `café`.
- A run can be cut off by a fault: from a chosen write step on, every
  write to one folder fails, as when a disk goes away. Each write is
  split into the local endpoint's steps (ADR 0019), so a fault can land
  between placing a file and archiving what it displaced.
- Nothing lost counts a displaced original in a leftover temporary, as
  the next run archives it; a finished run must leave none.
- A third invariant: right after a finished run, another one writes
  nothing, so no quirk feeds a copy loop.
- Each history draws its mode: Sync, or Mirror or Backup from either
  side. One-way runs must never write their source. Outside reported
  conflicts, a Mirror's target ends equal to its source, and a Backup's
  target holds every source file, with the source's content where the
  source changed since the last finished run, and loses nothing it held.
  One-way modes run on in-memory folders only.
- About 700 cases a second in release: CI runs 64, the nightly job
  100,000 (`FOLDOVER_MEMORY_SIMULATION_CASES`).

The first runs found two engine bugs, both fixed
([ADR 0030](0030-name-clashes.md)):

- A folder that stores names decomposed respelled a copied `café.txt`,
  and the next run took the new spelling for a rename on the other
  folder. Observed spellings that differ from the base's only in Unicode
  normalization are now mapped back to the base's on folders that
  ignore normalization.
- With both spellings in a byte-exact folder and one of them in
  conflict, the other was copied onto an HFS+ disk, where it landed on
  the conflicted name. A conflicted spelling now holds its name.

## Consequences

- The 1.0 gate (PLAN.md §5.2) asks for 14 consecutive nights without a
  violation; the nightly job runs the count, the record of nights is
  kept by CI history.
- Not emulated: extended attributes, volatile file identities, lost
  change events, network chaos and the deterministic executor. SFTP and
  peer folders are not simulated yet.
