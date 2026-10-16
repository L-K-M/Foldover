# ADR 0002: Change detection from a non-app process

Status: Proposed

## Context

PLAN.md risk S3 ([§8](../../PLAN.md#8-risks-and-spikes)): foldoverd is a
LaunchAgent, a plain command-line process with no app context. The
sync engine's trigger path depends on three assumptions nobody has
measured yet: that a path-scoped FSEvents stream replays changes from a
persisted event ID without loss, that mount and unmount notifications
reach such a process through DiskArbitration, and that the per-device
stream API is usable without privilege. The §4.8 design
([change detection](../../PLAN.md#48-change-detection)) stores a
`watch_cursor` FSEvents ID per volume UUID; if replay is unreliable the
cursor is worthless and watch design changes.

## Question

What does a plain command-line process actually get from FSEvents and
DiskArbitration on macOS 26, and do persisted event IDs replay reliably?

## Method

A throwaway Rust binary under `spikes/s3-change-detection/` calls
CoreServices FSEvents and DiskArbitration through a small documented
extern layer (`src/sys.rs`), with no `notify` dependency so `sinceWhen`
stays explicit. CI runs it on `macos-26` via
`spikes/s3-change-detection/run.sh <output.json>` (workflow
[spike-s3.yml](../../.github/workflows/spike-s3.yml)), which builds
release and writes a JSON report plus a human summary. Experiments:

1. **Item flags and latency** (`e1_item_flags`): path-scoped stream on a
   temp dir with FileEvents, WatchRoot, NoDefer, UseCFTypes and
   UseExtendedData; create, modify, rename, chmod, xattr-set, delete; per
   operation the union of item flags and operation-to-callback latency.
2. **Replay** (`e2_replay`): record the last delivered event ID, stop the
   stream, create 500 files, wait 3 s (the agent never resumes within
   milliseconds of a change), reopen with `sinceWhen` set to that ID;
   count how many of the 500 appear before `HistoryDone`, and how many
   at all once live events settle.
3. **Device-relative stream** (`e3_device_stream`): whether
   `FSEventStreamCreateRelativeToDevice` creates, starts and delivers to
   an unprivileged process. Watches `/` on the temp dir's device (firmlinks
   make lexical device-relative paths unreliable) and matches a uniquely
   named marker file.
4. **Volume UUIDs** (`e4_volume_uuids`): `FSEventsCopyUUIDForDevice` for
   the boot volume and each attached image.
5. **Disk images** (`e5_disk_images`): `hdiutil` APFS and ExFAT images,
   mounted browsable like a real external drive; experiments 1 and 2 on
   each, then detach, reattach, and check whether
   the volume UUID and pre-detach event ID still replay.
6. **DiskArbitration** (`e6_diskarbitration`): a session on a dedicated
   CFRunLoop thread registers appeared/disappeared callbacks while an
   image is attached and detached; callback receipt and latency.
7. **Burst** (`e7_burst`): 100,000 files created as fast as possible;
   counts of `MustScanSubDirs`, `UserDropped`, `KernelDropped` and total
   events delivered.

Each experiment reports pass/fail plus numbers in the JSON. A harness
failure is reported separately from a measured failure so a broken spike
can never masquerade as a result.

## Pass criteria

From PLAN.md §8, made precise:

- **Replay completeness**: 500 of 500 files changed while the stream was
  stopped are visible in the reopened stream before `HistoryDone`.
- **Mount visibility**: DiskArbitration appeared and disappeared
  callbacks both reach the non-app process when a disk image attaches and
  detaches.
- **Device stream answer**: `e3_device_stream` reports `available` true
  or false; either answer passes, an inconclusive measurement does not.

## Results

Pending the first CI run of spike-s3.

| Metric | Value |
|---|---|
| e1 flags per operation (create, modify, rename, chmod, xattr, delete) | |
| e1 operation-to-callback latency, mean ms | |
| e2 changes replayed (of 500) | |
| e2 HistoryDone arrived | |
| e3 device-relative stream available | |
| e4 boot volume FSEvents UUID | |
| e5 APFS image: replayed, UUID stable across reattach | |
| e5 ExFAT image: replayed, UUID stable across reattach | |
| e5 reattach replay from pre-detach event ID | |
| e6 appeared/disappeared callbacks, latency ms | |
| e7 events delivered for 100,000 files, drop flags | |

## Decision

Pending.

## Consequences

- If replay is complete: `watch_cursor` in §4.8 stands; `foldover-watch`
  persists the last event ID per volume UUID and resumes after restarts.
  If replay loses events or IDs die with detach, the cursor degrades to
  a hint and every restart or remount forces a shallow rescan, which
  strengthens the case for the §8 fallback line.
- If mount callbacks arrive: mount management and watch lifecycle live in
  the agent as designed. If they do not, the agent needs polling of the
  mount table or IPC from the app, and §4.2/§4.8 change.
- If per-device streams work unprivileged: a single stream per volume
  replaces per-pair streams, simplifying `foldover-watch`. If not,
  per-pair path streams stay.
- The SMB notification part of S3 is not covered here; §4.8's shallow
  interval and the "checked every N min" honesty line are unaffected by
  this result.
