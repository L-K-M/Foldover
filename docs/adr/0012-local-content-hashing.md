# ADR 0012: Bounded local data-fork hashing

Status: Implemented read primitive; not exposed by the agent

## Scope

`Root::hash_file` produces a BLAKE3 digest for an observed regular file's data
fork. It consumes an existing metadata fingerprint and a cooperative progress
callback. `LocalEndpoint::hash_file` accepts an inventory entry, retains endpoint
namespace/device checks and maps failures into scan errors.

This does not produce a complete file version. Xattrs, ACLs, resource forks,
flags, naming capabilities and canonical metadata digests are still required.
The agent and app do not expose content reads or sync execution yet.

## Read contract

- Hold the selected root and open descendants one component at a time without
  following symlinks. Compare the named entry against its observed fingerprint
  before opening, then compare the opened descriptor before reading data.
- Open read-only and nonblocking, so a FIFO substituted during lookup cannot
  block waiting for a writer. Nonregular entries never receive a digest.
- Use a 64 KiB buffer. Read at most the observed size plus one growth-check byte;
  an actively growing file cannot extend the work budget indefinitely.
- Check cancellation between chunks. Cancellation, early EOF, I/O errors and
  changed entries yield errors, never partial digests.
- After reading, recheck the open descriptor and root-relative named binding.
  Kind, size, mtime, device/inode, ctime and observed residency are compared. Atime is excluded
  because reading may update it.

Before/after checks are observations, not a filesystem snapshot. Watchers,
snapshot generations, mount continuity and execution-time freshness checks
must still close the planning/execution boundary. Coarse or unstable filesystem
attributes need capability-specific policy.

## Residency boundary

Darwin metadata captures `SF_DATALESS` in the same no-follow `fstatat` or
descriptor `fstat` observation as the entry identity. Known dataless files
are refused before content open. Other platforms report `NotReported`, never
inventing a materialized observation. Sparse allocation is not a residency test.

Apple documents process/thread materialization policy in
[`getiopolicy_np`](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man3/getiopolicy_np.3).
The filesystem driver now enforces and reads back `OFF` on the calling thread
before namespace I/O and every content read. Iteration checks the consuming
thread, including after an iterator moves between threads. Hash callbacks
cannot leave materialization enabled for a subsequent read. The driver leaves
the thread policy disabled; it does not restore an inherited permissive policy.
Policy setup/query failures are I/O errors, never complete scans or digests.

The owner approved a narrow unsafe exception for audited macOS driver bindings
on 2026-10-17. The binding accepts only C integers, has safe internal interfaces
and `SAFETY` comments, and is isolated from filesystem traversal, which still
forbids unsafe. Other layers retain the workspace prohibition. Native tests
compare constants against the installed Darwin SDK and exercise policy changes
on real worker threads and across callbacks.

Launchd's `MaterializeDatalessFiles=false` is an additional managed-launch
defense. Account-backed File Provider verification, cloud-root readiness and
full metadata/volume gates remain open. Content reads remain below the agent API.

## Verification

Tests cover official empty/`abc` BLAKE3 vectors, multichunk reads, cancellation,
stale observations, shrink/growth during reads, path substitution to an outside
link, FIFO refusal, held roots after ambient replacement, and independent
endpoint protection of internal/AppleDouble names. Native checks run in macOS CI.
These tests do not establish cloud residency, full metadata fidelity or any
execution/recovery gate.
Additional tests cover descriptor/name metadata agreement, invalid stat values,
pre-epoch timestamps, dataless refusal before content open and Darwin flag
classification. The synthetic flag is tested through a stat-record fixture;
account-backed File Provider behavior still needs a real provider fixture.
