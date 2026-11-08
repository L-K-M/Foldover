# ADR 0052: Compact observation storage

Status: Implemented

## Context

The performance action plan's memory gate failed before observation began:
a synthetic catalog with 1,001,000 baseline rows and two snapshots loaded
into 840 MB. A first-copy measurement alone had missed this tracked-state
cost. Periodic local rehashing exposed another peak, retaining a complete
old entry for every refreshed timestamp. See the measured cases and limits
in [qualification results](../../perf/qualification-results.md).

## Decision

- Keep exact raw paths in shared immutable byte slices and sorted dense
  `PathMap` rows. Ordering, validation, serialization and path equality stay
  byte exact. Normalized comparison remains a separate operation.
- Store a baseline's common content once, alongside both metadata digests.
  An observation carries a 16-byte opaque version handle. It resolves to a
  baseline side or an appended version in a sealed pool. Handles keep their
  pool alive, including after a preview outlives its run. Appended handles
  cannot be read until their builder seals the pool.
- Inline each observation's fingerprint header and keep its exact stat key
  in a private lossless sparse encoding when smaller. Persistence and hash
  jitter expand the original bytes. No observation field is dropped, and
  malformed persisted fingerprints remain cache misses.
- Consume prior snapshot rows in place. Readers retain only their bounded
  in-flight originals; the caller records results. New paths use a separate
  additions buffer. Reuse one percent row headroom and keep the dense row
  allocation through final sorting instead of doubling it on a small edit.
- When a verified entry changes only its observation time, move the old key
  allocation and sealed version into the new row. Keep the exact original
  time in a lazy column: ordinary dates use eight-byte nanoseconds, while
  pre-epoch or out-of-range dates use full `SystemTime` values. Share baseline
  positions only when every original raw key agrees; otherwise retain a
  separate original key index. Duplicate reads still see the original time.
- After spelling alignment, compare each effective snapshot with its valid
  original. Preserve original source and destination values before a rename
  overwrites their comparison payload. Missing rows, late failures and
  exclusions participate in this comparison; unseen rows never become
  verified observations. These flags and indexes are transient, outside
  persistence and approval tokens.
- Let the second endpoint share matching verified versions and exact path
  storage from the completed first observation. Hints contain no fingerprint
  and cannot skip hashing or stat verification. Full version equality is
  required after observation. Omit versions already represented by the
  target baseline, and drop the temporary hint index before final sorting.
- Keep verified excluded entries in the same snapshot allocation, with a
  filtered public view. Preview items retain version handles rather than
  fingerprints. Common plan decisions share immutable values. Count the immutable path
  union in a cancellable pass before reserving its exact plan row capacity.

The database driver pins a read-only WAL view and its writer-ownership
anchor. The engine captures inputs and counts from that coherent view,
then streams typed decoding outside the service mutex. Concurrent
configuration changes still invalidate the captured revision. Snapshot
persistence merges bounded keyset pages in one revision-checked transaction;
SQLite remains in WAL mode with `synchronous=FULL`.

## Consequences

The storage changes preserve file-content verification, local racy-window
and periodic rehash rules, SFTP full rereads, root checks, leases, journal
batch size, archives and preview approval binding. They add no dependency
or handwritten unsafe code, and change no persisted fingerprint format.

Allocation, exact-comparison, held-handle, malformed-cache, cancellation and
spelling-cycle regressions cover the representation boundaries. Real
million-file fixtures cover initial populated folders, tracked no-change
and forced periodic rehashes. Those samples establish specific workload
results, not a universal bound for arbitrary path lengths, metadata,
concurrent previews or installed-agent configurations. The failed million-
file LAN sample remains a qualification gap.
