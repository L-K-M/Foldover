# ADR 0002: Change detection from a non-app process

Status: Accepted with fallback (local probes only); replay implemented

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
DiskArbitration on the supported macOS, and do persisted event IDs replay
reliably?

## Method

A throwaway Rust binary under `spikes/s3-change-detection/` calls
CoreServices FSEvents and DiskArbitration through a small documented
extern layer (`src/sys.rs`), with no `notify` dependency so `sinceWhen`
stays explicit. CI now uses the macOS 27 `xcode-27` image via
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
events separately and uses browsable images.

Revised run
([38963774123](https://github.com/L-K-M/Foldover/actions/runs/38963774123)),
commit `2b369f2`, same macOS version and architecture:

| Metric | Result |
|---|---|
| Boot replay before HistoryDone | 500/500, 21.09 ms |
| APFS image replay before HistoryDone | 500/500, 14.81 ms |
| ExFAT image replay before HistoryDone | 500/500, 14.01 ms |
| Late live events needed to reach 500 | None on any volume |
| APFS FSEvents-store UUID | Absent before and after reattach |
| ExFAT FSEvents-store UUID | Present, but changed after reattach |
| Reattach replay marker | Absent on both images |
| Unprivileged device-relative stream | Marker delivered |
| DiskArbitration appeared/disappeared | Both received; 79.85 / 301.80 ms |
| Burst | All 100,000 paths in 100,045 events; no drop flags |

The three harness verdict flags pass. Image continuity is not a verdict
gate and did not pass: neither image replayed its reattach marker.
An immediate-resume guarantee cannot be inferred from a settled replay,
and these runs do not isolate the cause of the first run's misses.
Neither burst produced drop flags, so dropped-event recovery remains
unverified. SMB notifications, polling cost and deep-change detection
are outside this harness; the full S3 risk remains open.

### macOS 27 follow-up

[Run 39019031434](https://github.com/L-K-M/Foldover/actions/runs/39019031434),
commit `305d6ee`, used macOS 27.0.1 (26A434), arm64. All three harness
verdicts passed; the remount and dropped-event limits remained:

| Metric | Result |
|---|---|
| Settled replay before HistoryDone | 500/500 on boot, APFS and exFAT |
| Replay latency | 17.41 / 8.98 / 38.27 ms, respectively |
| Image replay marker after reattach | Absent on both |
| APFS FSEvents-store UUID | Absent before detach, present after reattach |
| exFAT FSEvents-store UUID | Changed after reattach |
| Mount/unmount callbacks | Both received; 91.72 / 121.40 ms |
| Burst | All 100,000 paths in 100,015 events; no drop flags |

Keep the full-root-rescan fallback. This run does not prove remount
continuity, forced-drop recovery, share polling or physical-volume behavior.

## Decision

Keep path-scoped watchers and DiskArbitration in the agent. Use replay
only as a hint backed by metadata verification. Store a cursor with both
the volume UUID and the FSEvents-store UUID; reuse it only when both match.
A missing or changed UUID, or any remount, requires a full root metadata
rescan. Remount replay must be proven before relaxing that fallback.

## Implementation

Start-up replay for "When files change" pairs:

```text
pair quiet ──every minute──▶ cursor per local side: root, store UUID,
                             event ID seen one tick (5 s) earlier
agent starts ──▶ replay each root from its cursor until HistoryDone
  relevant event            ──▶ changed: the pair runs
  HistoryDone, none found   ──▶ unchanged: no run
  anything else             ──▶ changed
```

- `foldover-watch` opens a path-scoped stream with `sinceWhen` set to
  the cursor and reads until `HistoryDone`, at most 30 s; its FFI is the
  audited `history/fsevents.rs`.
- A cursor (state schema 22, `watch_cursor`) is recorded only while the
  pair is quiet: no run in progress, no change waiting for a run, its
  start-up replay done and its folders watched. Nothing before the
  recorded event is then left unsynced.
- Foldover's own names never count. Drop, root-changed, mount and
  unmount flags, a timeout, a store UUID that is missing or differs, a
  cursor for another root, and folders without a history (network
  volumes, SFTP, paired Macs, Linux) all count as a change, which costs
  one full observation.

## Consequences

- `foldover-watch` owns change detection and mount lifecycle in the agent;
  the measured callbacks do not require an app relay.
- The FSEvents-store UUID is distinct from volume identity. A readable
  volume alone does not make a saved cursor reusable.
- A device-relative marker proves availability, not replay correctness;
  keep per-root streams until their replacement is validated.
- The SMB polling interval and the "checked every N min" line in §4.8
  remain unvalidated. Forced-drop recovery and physical-volume tests
  are still required before closing S3.
