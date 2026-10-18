# 0003. Diff tree view: NSOutlineView vs SwiftUI List

Status: Proposed

## Context

[PLAN.md §8](../../PLAN.md#8-risks-and-spikes) lists diff-tree
performance as spike S5: the Preview must show a 200,000-row plan,
streamed while the scan runs, at 60 fps with flat memory and first
rows under a second. PLAN.md §4 already picks a SwiftUI shell with an
AppKit `NSOutlineView` for the diff tree because SwiftUI `List`
stalls near 100k rows. S5 verifies that choice against the
alternative, since the Preview is the product's core screen and the
fallback (flat `Table` mode for huge plans) changes what the app must
build.

## Question

Can the Preview's diff tree show a 200,000-row plan, streamed while
the scan runs, at 60 fps with flat memory? Compare `NSOutlineView`
hosted in SwiftUI via `NSViewRepresentable` with SwiftUI
`List(children:)` over the same model.

## Method

`spikes/s5-diff-tree` now builds DiffTreeBench for macOS 27. The app:

- generates a deterministic 200,000-row plan (a folder tree of depth
  3 to 5; each row carries an action glyph, name, size, date and
  accessibility label),
- runs each implementation in a fresh window hosting the same
  SwiftUI root (`NSHostingView`), so hosting cost is shared,
- streams the plan into one shared model in 5,000-row batches every
  50 ms,
- then scrolls through the whole tree in 200 programmatic steps and
  collapses and expands the whole tree (model-driven for the SwiftUI
  list, which has no expansion API; that is itself measured),
- and records, on a `CADisplayLink` tick sampler: time to first
  visible rows, frame intervals (p50, p95, max) and the longest
  main-thread stall per phase, plus `phys_footprint` after load and
  after scrolling.

Each batch also records synchronous model construction, grouped outline
insertion and per-folder expansion walls, with group/folder/visible-row counts.
These stage timers help locate stream stalls; deferred layout and rendering
remain visible only in the frame/stall measurements. Row shape, batch cadence
and performance criteria are unchanged by this instrumentation.

The harness also measures the documented flat-table fallback: an `NSTableView`
with full relative paths and the same generated rows, model, cell presentation,
batch cadence and scroll sweep. Both AppKit candidates retain the original
budgets. A complete-exercise check requires all 200,000 rows, positive scroll
distance and reaching the last row; missing frame samples cannot pass a frame-time check. Flat tables have
no disclosure operation, so their reports omit collapse/expand phases rather
than recording successful no-ops. The overall verdict still requires both
AppKit candidates to pass; inspect each candidate's checks before choosing it.
The flat-table result below is a virtual-Mac measurement. Physical-Mac
calibration remains pending.

Formatting timers separately record date/size formatter walls and call counts
per batch. They apply equally to every candidate and retain the same workload
and limits; timer overhead remains part of measured delivery/frame time.

One worker actor now prepares each batch's strings before main-actor delivery.
Only one batch is in flight. Preparation wall time includes scheduling and
handoff; model wall time covers the remaining main-actor work. Frame sampling
includes both, and execution on the main thread fails the run. The standalone
row check compares ordered output against Foundation reference formatting.

Run it with `spikes/s5-diff-tree/run.sh <report.json>`; it builds the
Release app with XcodeGen and xcodebuild, launches it through
`open -W` with `-runBench`, writes the JSON report and prints a short
summary. The workflow `.github/workflows/spike-s5.yml` does this on
the macOS 27 `xcode-27` image and uploads the report as an artifact. A
measured miss is data for this ADR, not a harness failure.

## Pass criteria

AppKit implementation, from PLAN.md §8 S5 made precise:

| Metric | Pass |
|---|---|
| Frame-interval p95 while streaming | <= 16.7 ms |
| Frame-interval p95 while scrolling | <= 16.7 ms |
| Longest main-thread stall, any phase | < 100 ms |
| `phys_footprint` growth while scrolling | < 10 % |
| Time to first visible rows | < 1 s |

Identical numbers are recorded for the SwiftUI implementation; its
missing them is a finding, not a harness failure.

## Results

First complete CI run of spike-s5
([run 38955012511](https://github.com/L-K-M/Foldover/actions/runs/38955012511)):
GitHub `macos-26` runner, Apple M1 (virtual, `VirtualMac2,1`), macOS
26.6.2, Xcode 26.6, 960x640 window at 60 Hz. Verdict: fail.

| Metric | AppKit | SwiftUI |
|---|---|---|
| First visible rows (ms) | 270 | 112 |
| Stream frame p50 (ms) | 20.8 | 19.6 |
| Stream frame p95 (ms) | 510.9 | 47.1 |
| Stream frame max (ms) | 798.9 | 50.0 |
| Scroll frame p95 (ms) | 33.3 | 16.7 (not valid, see below) |
| Longest main-thread stall (ms) | 800 (stream) | 50 (stream) |
| Collapse and expand wall (ms) | 417 and 341 | 21 and 5 (model-driven) |
| `phys_footprint` after load (MB) | 151.7 | 89.5 |
| `phys_footprint` after scroll (MB) | 153.6 | 107.2 |
| Memory growth while scrolling (%) | 1.3 | 19.7 |

AppKit passes first rows and memory and fails both frame-time gates
and the stall gate. Applying a 5,000-row batch every 50 ms with one
`expandItem` call per new folder is a suspected source of stream
stalls; profiling is still needed. Scrolling holds 60 Hz at the
median (p50 16.7 ms) but drops frames at p95.

The SwiftUI numbers for scrolling are not valid: the harness could not
read the list's rows or scroll offset (0 rows on screen, 0 points
scrolled), so it measured an idle list. Its memory grew 19.7 % anyway.

These virtual-Mac measurements do not establish physical-Mac frame
times. Hardware calibration is still required.

### macOS 27 follow-up

[Run 39019031441](https://github.com/L-K-M/Foldover/actions/runs/39019031441),
commit `305d6ee`, used macOS 27.0.1 (26A434), Xcode 27.0 (27A266a) and
a virtual Apple M1, with the same row count, window size and 60 Hz sampler.
Verdict: fail.

| Metric | AppKit | SwiftUI |
|---|---|---|
| First visible rows (ms) | 185.1 | 73.1 |
| Stream frame p95 (ms) | 406.7 | 56.1 |
| Scroll frame p95 (ms) | 34.0 | 16.7 (invalid) |
| Longest main-thread stall (ms) | 782.5 | 67.4 |
| Memory growth while scrolling (%) | 1.07 | 0.02 (scroll unexercised) |

AppKit still fails the frame-time and stall gates. SwiftUI again reported
zero rows and zero scroll distance; its apparent scroll result cannot
support a performance decision. The provisional choice and follow-ups stand.

### Flat-table follow-up

[Run 39073881183](https://github.com/L-K-M/Foldover/actions/runs/39073881183),
commit `fc87465`, used the same virtual M1/macOS 27.0.1/Xcode 27 environment.
Both AppKit candidates exposed all 200,000 rows and reached the last row.
The flat table improves latency but still fails the original budgets.

| Metric | Outline | Flat table |
|---|---|---|
| First rows (ms) | 419.58 | 105.27 |
| Stream frame p95 (ms) | 673.45 | 83.33 |
| Scroll frame p95 (ms) | 48.13 | 33.68 |
| Maximum stall (ms) | 1066.74 | 100.02 |
| Scroll memory growth (%) | 1.253 | 1.043 |

Flat-table batch model p95 was 72.37 ms, versus 2.19 ms for insertion. The next
profile separates date/size formatting from other model work before choosing
an optimization. SwiftUI again reported zero rows/scroll and now explicitly
fails the complete-exercise check. No 60 fps or physical-Mac gate is closed.

### Formatter profile

[Run 39077239646](https://github.com/L-K-M/Foldover/actions/runs/39077239646),
commit `9f47a45`, retained the workload and environment above. Flat-table batch
model p95 was 25.16 ms, with size formatting at 15.44 ms and date formatting at
3.38 ms. Formatting consumed 74.6% of total model time; all 200,000 dates and
sizes were formatted. Stream p95 was 39.33 ms, scroll p95 16.67 ms, maximum
stall 50.17 ms and scroll memory growth 1.05%. The stream budget still failed.

Outline expansion p95 remained 389.90 ms. Moving unchanged formatting off the
main actor targets the measured flat-table bottleneck.

### Worker preparation

[Baseline 39082926960](https://github.com/L-K-M/Foldover/actions/runs/39082926960),
commit `16164a8`, and
[worker run 39082940427](https://github.com/L-K-M/Foldover/actions/runs/39082940427),
commit `9460803`, used the same workload and virtual-Mac configuration above.
The native worker reference check passed. Both AppKit candidates exposed all
200,000 rows and reached the last row; all dates and sizes were formatted,
with zero worker batches on the main thread.

| Flat-table metric | Baseline | Worker |
|---|---|---|
| Main-actor model p95 (ms) | 52.50 | 11.27 |
| Worker preparation p95 (ms) | Not separate | 48.16 |
| First rows (ms) | 95.85 | 112.35 |
| Stream frame p95 (ms) | 59.46 | 33.33 |
| Scroll frame p95 (ms) | 33.33 | 28.57 |
| Maximum stall (ms) | 83.36 | 91.14 |
| Scroll memory growth (%) | 1.11 | 1.12 |

Measured main-actor model cost fell, but both frame-time budgets still fail.
Outline collapse/expansion stalls also remain above 100 ms. Host variance,
scroll/layout profiling, physical-Mac calibration and valid SwiftUI scrolling
remain open. Workload, cadence, presentation and budgets are unchanged.

## Decision

Provisional: keep `NSOutlineView` in `NSViewRepresentable`, as PLAN.md
§4.5 plans. This harness measured flat memory and scrolling for AppKit;
the SwiftUI comparison remains inconclusive because its scrolling
was not exercised. The flat `Table` fallback remains open until the
AppKit failures are investigated. Two follow-ups decide it before
P3 builds the Preview:

1. Profile batch application, test coalesced updates and avoid
   per-folder expansion passes, then re-run with valid SwiftUI scrolling.
2. Re-run on a physical Mac, which CI cannot provide.

## Consequences

- AppKit passes: keep PLAN.md §4's `NSOutlineView` hosted in SwiftUI
  via `NSViewRepresentable`; the flat-`Table` fallback stays unused.
- AppKit fails: the diff tree becomes the §8 fallback, a flat
  `Table` mode for huge plans, and the spike is re-run against it.
- SwiftUI passes or nearly passes: reconsider the AppKit dependency
  before the Preview is built on it in P2.
