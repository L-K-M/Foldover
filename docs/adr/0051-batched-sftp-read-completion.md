# ADR 0051: Batched SFTP read completion

Status: Implemented

## Context

A bounded read ended with a separate CLOSE request and a trailing LSTAT, each
a round trip. On a NAS with many small files the read phase dominates the run,
so every completed file paid two completion round trips after its data. The
action plan's X2 lists pipelining CLOSE with the trailing LSTAT as a candidate,
provided EOF, drain, reordering and close-error behavior stay correct.

## Decision

- A read that completed sends CLOSE and the trailing LSTAT in one bounded
  `send` and flush, then registers both request IDs. The replies may arrive in
  either order; both are collected before parsing, so a server refusal cannot
  leave a reply owed on a reusable session. A failed receive poisons the
  session, and the read path's existing drain-on-error discipline is unchanged.
- Only a read that completed is batched: the read succeeded, was not cancelled,
  did not overrun, and took exactly the expected size. Every other path uses
  the existing plain CLOSE.
- A server's CLOSE status for a read is best effort, as before; the endpoint's
  change check is the trailing LSTAT. A transport error (a failed send or
  receive) still fails the read and poisons the session.
- Write close and the final stored-state checks are unchanged: writes use
  `close` and check its status, and the write path still re-reads final
  attributes after a mutation.

## Verification and measurements

The permanent wire tests cover both requests in flight before either reply,
reversed replies, a refused CLOSE with a failed LSTAT, and an unanswered
CLOSE that poisons the session. The endpoint's existing change-detection tests
still pass.

Three paired no-change runs of a 10k-file corpus (8 KiB files, 1,000 per
folder) against a NAS over SSH on a shared macOS host (M4 Max, 14 cores,
macOS 27.0.1) measured the remote observation at 67.586-86.364 s without
batching (median 68.678 s) and 54.777-56.158 s with it (median 56.135 s).
The host was busy with other agents, so this is not an idle qualification.
See [perf/sftp.md](../../perf/sftp.md) for provenance and raw ranges.

A two-write variant that sent CLOSE and LSTAT as separate writes was measured
and rejected. Its 10k remote observations were noisy and regressive
(85.206, 57.436, 56.373 s against 69.571, 71.202, 71.547 s for the same
eight-reader baseline), and a rerun with the SSH sampler still showed two
86-second outliers (86.234, 85.765, 58.008 s). It was not promoted; the
one-write batch above is the accepted change.

## Consequences

- The read completion sends CLOSE and the trailing LSTAT in one bounded write
  instead of two separate writes.
- A server that refuses CLOSE still gets its LSTAT reply collected, so the
  read's change check is unaffected and the session stays aligned.
- The two-write variant is rejected; do not reintroduce separate writes for
  the read completion.
