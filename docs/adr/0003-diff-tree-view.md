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

`spikes/s5-diff-tree` builds DiffTreeBench, a macOS 26 app that:

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

Run it with `spikes/s5-diff-tree/run.sh <report.json>`; it builds the
Release app with XcodeGen and xcodebuild, launches it through
`open -W` with `-runBench`, writes the JSON report and prints a short
summary. The workflow `.github/workflows/spike-s5.yml` does this on
macos-26 and uploads the report as an artifact. A measured miss is
data for this ADR, not a harness failure.

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

Pending the first CI run of spike-s5.

| Metric | AppKit | SwiftUI |
|---|---|---|
| First visible rows (ms) | | |
| Stream frame p50 (ms) | | |
| Stream frame p95 (ms) | | |
| Stream frame max (ms) | | |
| Scroll frame p95 (ms) | | |
| Longest main-thread stall (ms) | | |
| Collapse and expand wall (ms) | | |
| `phys_footprint` after load (MB) | | |
| `phys_footprint` after scroll (MB) | | |
| Memory growth while scrolling (%) | | |

## Decision

Pending.

## Consequences

- AppKit passes: keep PLAN.md §4's `NSOutlineView` hosted in SwiftUI
  via `NSViewRepresentable`; the flat-`Table` fallback stays unused.
- AppKit fails: the diff tree becomes the §8 fallback, a flat
  `Table` mode for huge plans, and the spike is re-run against it.
- SwiftUI passes or nearly passes: reconsider the AppKit dependency
  before the Preview is built on it in P2.
