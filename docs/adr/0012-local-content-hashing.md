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
  Kind, size, mtime, device/inode and ctime are compared. Atime is excluded
  because reading may update it.

Before/after checks are observations, not a filesystem snapshot. Watchers,
snapshot generations, mount continuity and execution-time freshness checks
must still close the planning/execution boundary. Coarse or unstable filesystem
attributes need capability-specific policy.

## Residency boundary

Opening file data can materialize cloud placeholders. This primitive does not
claim to prevent that. Content reads must remain inaccessible through the
agent until the host enforces a tested non-materializing policy and cloud-root
readiness is implemented.

Apple documents process/thread materialization policy in
[`getiopolicy_np`](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man3/getiopolicy_np.3).
Launchd also exposes `MaterializeDatalessFiles`. A managed-launch policy and
live process-context verification are candidates; neither is established by
this read primitive. A hand-written unsafe wrapper is not permitted by the
repository's Rust rules. Account-backed File Provider verification remains open.

The packaged LaunchAgent now explicitly sets `MaterializeDatalessFiles=false`.
CI checks the template and runs isolated launchd jobs with disabled/enabled
policies, inspecting the main thread, worker threads and a child through native
`getiopolicy_np`. This verifies managed-launch policy inheritance without
reading cloud data. It does not verify that a manually started agent has that
policy, or close the account-backed File Provider gate. A production runtime
context check is still required before exposing content methods.

## Verification

Tests cover official empty/`abc` BLAKE3 vectors, multichunk reads, cancellation,
stale observations, shrink/growth during reads, path substitution to an outside
link, FIFO refusal, held roots after ambient replacement, and independent
endpoint protection of internal/AppleDouble names. Native checks run in macOS CI.
These tests do not establish cloud residency, full metadata fidelity or any
execution/recovery gate.
