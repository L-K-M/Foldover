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
   stream, create 500 files, wait 3 s to probe settled history,
   reopen with `sinceWhen` set to that ID;
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

First complete run
([38952352242](https://github.com/L-K-M/Foldover/actions/runs/38952352242)),
commit `6146b60`, GitHub `macos-26` runner, macOS 26.6.2 (25G83), arm64.
Verdict: fail. The harness completed, but replay missed its criterion.

| Metric | Value |
|---|---|
| e1 operations observed | All six; no drop flags |
| e1 mean callback latency by operation (ms) | Create 81.54; modify 11.56; rename 11.55; chmod 10.96; xattr 11.41; delete 10.89 |
| e2 changes replayed (of 500) | 499; last file absent before HistoryDone |
| e2 HistoryDone arrived | Yes, after 171.26 ms |
| e3 device-relative stream available | Yes; marker delivered without privilege |
| e4 boot volume FSEvents UUID | Present |
| e5 APFS image | 0/500 replayed; FSEvents UUID absent before and after reattach |
| e5 ExFAT image | 0/500 replayed; FSEvents UUID absent before and after reattach |
| e5 reattach replay from pre-detach event ID | Marker absent on both images |
| e6 appeared/disappeared callbacks, latency ms | Both received; 70.64 / 77.18 |
| e7 events delivered for 100,000 files | 100,043 events, all 100,000 paths; no drop flags |

That run reopened immediately after writing and mounted images with
`-nobrowse`. Persistence lag and mount eligibility are hypotheses, not
established causes. The revised method above waits 3 s, counts late live
events separately and uses browsable images. Its CI result is pending.
An immediate-resume guarantee cannot be inferred from a settled replay.
The burst produced no drop flags, so dropped-event recovery remains
unverified. SMB notifications, polling cost and deep-change detection
are outside this harness.

## Decision

Provisional: keep path-scoped watchers and DiskArbitration in the agent.
Treat replay as a hint backed by metadata verification, as §4.8 requires.
Do not rely on replay after remount when no FSEvents UUID is available.
The revised run must be recorded before accepting replay completeness.

## Consequences

- If replay is complete: `watch_cursor` in §4.8 stands; `foldover-watch`
  persists the last event ID per volume UUID and resumes after restarts.
  If replay loses events or IDs die with detach, the cursor degrades to
  a hint and every restart or remount forces a full metadata rescan, which
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
