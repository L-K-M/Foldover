# ADR 0049: Bounded SFTP reads and access probes

Status: Implemented

## Context

An SFTP observation knows each file's listed size, but the transport sent a
window of sixteen 32 KiB READ requests regardless of that size. A short DATA
answer made it ignore later answers in the window, including EOF, and send
another window. Small identity, lease and ignore files used that path too.
Opening an endpoint also listed the entire root to prove listing access,
before observation listed it again.

## Decision

- Known-size file reads request only remaining bytes, retaining a separate
  one-byte end probe. The endpoint still checks exact length, hashes every
  byte and compares the trailing stat. The end probe reaches its overflow
  check before any excess byte can reach the consumer. Early EOF still
  becomes `ScanError::Changed` at the endpoint.
- Control files use the same operation with a maximum length, not an assumed
  exact size. An overflow fails instead of returning a truncated identity,
  lease or ignore file. The existing unknown-length operation remains.
- Short replies restart at the first unconsumed offset. Outstanding replies
  are drained on errors, including cancellation via the sink, and failed
  drains make the session unusable. Replies cannot exceed their request.
- A startup listing probe reads and validates one directory batch, then
  closes the handle. Observation still enumerates the complete tree.

## Verification and measurements

The permanent scripted-server test `known_length_read_packet_budget` counts
actual framed READ requests and validates every byte, including resumed reads.
Before the change it failed on an empty file: 16 requests exceeded the budget.
With the bound, normal reads use `ceil(remaining / 32768) + 1` requests; the
last request is the end probe. Short DATA replies can require additional work.

| Bytes from offset zero | Previous READs | Bounded READs |
|---|---|---|
| 0 | 16 | 1 |
| 1 | 32 | 2 |
| 1,024 | 32 | 2 |
| 32,768 | 16 | 2 |
| 32,769 | 32 | 3 |
| 524,288 | 32 | 17 |

These are protocol counts, not elapsed-time speedups. The previous counts
also appear in the [pipeline review](../performance/sftp-lan-million-doc-pipeline.md).
Tests cover short replies, growth, early EOF, invalid lengths and ranges,
out-of-order replies, sink failure, deadlines, access probes and control-file
overflow. Existing endpoint tests continue to reject changed content even
when the remote stat fields match.

The 10k local scan baseline at `2301115` on the shared M4 Max host measured
5.40–7.00 seconds (Criterion estimate, 20 samples), above the existing one-second
budget. This is a loaded-host diagnostic, not qualification; this SFTP change
does not alter the local scan. No real SSH-over-LAN speedup is claimed here.

## Consequences

Small-file observations and control reads avoid speculative packets without
trusting SFTP timestamps. Large files still use up to sixteen reads in flight.
The maximum-length operation may deliver one excess byte to its rejecting
sink, so callers must check the bound before forwarding data or accepting a
control file. Startup proves access to the first batch only; errors in later
batches are reported by observation, with the existing unavailable-subtree
protection.
