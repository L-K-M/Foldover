# ADR 0044: Bandwidth limit

Status: Implemented

## Context

PLAN.md §4.9 promises a bandwidth cap per pair, so a large first sync
to a NAS or a server over a slow uplink leaves room for everything else
on the network. Copies ran as fast as the folders went.

## Decision

- **Option.** A pair may set a limit in bytes per second
  (`PairOptions::bandwidth_limit`), at least 100 kB/s; below that a few
  files take hours. The app and the CLI offer whole megabytes per
  second. Schema 25 stores it; older agents ignore the column and copy
  at full speed, so the minimum reader stays.
- **One clock per run.** Each run holds one pace (`RunScope`), shared by
  both folders' views of the run and by every copy worker
  ([ADR 0043](0043-parallel-copies.md)). Each copied byte books its
  share of time on it; a copy waits until its bytes are due, so the
  run's copies together never go faster than the limit. Idle time is
  not saved up for a burst.
- **Where it waits.** File data is paced as it is read: local copies
  through the driver's per-chunk checkpoint, streamed copies (SFTP,
  another Mac) in the one place all their data passes through. A wait
  asks the run's control every 100 ms, so a cancel or a lease beat is
  never held up by a long wait.
- **Scope.** The limit counts file data only, not listings, metadata or
  checksums. The Mac serving a folder to another Mac does not pace; the
  Mac that runs the pair does.

## Consequences

- A limited pair's runs take at least their data's size divided by the
  limit.
- Two pairs each keep their own limit; there is no limit across pairs.
