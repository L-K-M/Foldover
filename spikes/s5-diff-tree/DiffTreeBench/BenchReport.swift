// The benchmark's report model (ADR docs/adr/0003-diff-tree-view.md):
// a `Codable` tree encoded as pretty JSON with sorted keys, plus the
// percentile math and the stdout summary the launch-argument run
// prints before quitting. Foundation only.
//
// Adapted from L-K-M/Sweetbriar (BenchmarkStatistics.swift and
// BenchmarkReport.swift): the nearest-rank percentile and the
// report/summary layout carry over; the shape is flattened because
// this spike measures phases, not per-event latency.

import Foundation

/// The whole report. `harnessOK` separates a broken harness from a
// measured miss: the workflow fails only when the harness breaks, so
// this flag is what a verdict of "fail" is allowed to mean.
struct DiffTreeBenchReport: Codable {
    var harnessOK: Bool
    /// Harness-level problems — a run that could not start, a missing
    /// view — plus each run's failures, prefixed.
    var failures: [String]
    var environment: BenchEnvironment
    var runs: [BenchRun]
    /// The pass criteria evaluated; `gate` marks the ones the
    /// AppKit implementation must pass (PLAN.md §8 S5). SwiftUI's
    /// identical numbers are recorded, not gated.
    var checks: [BenchCheck]
    /// `pass` = every gate check passed; `fail`, `incomplete` or
    /// `wedged` otherwise.
    var verdict: String

    /// Pretty-printed, sorted-keys JSON.
    func encoded() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return try encoder.encode(self)
    }

    /// The short stdout summary printed before quitting.
    var stdoutSummary: String {
        var lines = ["s5-diff-tree: verdict=\(verdict) harnessOK=\(harnessOK)"]
        for failure in failures {
            lines.append("  failed: \(failure)")
        }
        for run in runs {
            lines.append("  \(run.implementation): ok=\(run.ok)"
                + " firstRows=\(format(run.firstRowsMilliseconds))ms"
                + " mem=\(run.memoryAfterLoadBytes / 1_048_576)→"
                + "\(run.memoryAfterScrollBytes / 1_048_576)MB"
                + " (\(format(run.memoryGrowthPercent))%)")
            for phase in run.phases {
                lines.append("    \(phase.name): frames=\(phase.frameCount)"
                    + " p50=\(format(phase.frameP50Milliseconds))"
                    + " p95=\(format(phase.frameP95Milliseconds))"
                    + " max=\(format(phase.frameMaxMilliseconds))"
                    + " stall=\(format(phase.maxStallMilliseconds))ms"
                    + phase.wallMilliseconds.map {
                        " call=\(format($0))ms"
                    }.or("")
                )
            }
            if !run.batches.isEmpty {
                lines.append("    batch p95: model=\(format(BenchStatistics.percentile(run.batches.map(\.modelMilliseconds), p: 95)))ms"
                    + " inserts=\(format(BenchStatistics.percentile(run.batches.compactMap(\.insertionMilliseconds), p: 95)))ms"
                    + " expansion=\(format(BenchStatistics.percentile(run.batches.compactMap(\.expansionMilliseconds), p: 95)))ms")
            }
            for failure in run.failures {
                lines.append("    failed: \(failure)")
            }
        }
        for check in checks {
            lines.append("  \(check.pass ? "pass" : "MISS") \(check.name):"
                + " \(format(check.value)) vs \(format(check.limit))"
                + (check.gate ? " (gate)" : ""))
        }
        return lines.joined(separator: "\n")
    }

    private func format(_ value: Double?) -> String {
        value.map { String(format: "%.2f", $0) } ?? "—"
    }
}

extension Optional where Wrapped == String {
    /// `optional ?? ""` for summary assembly.
    func or(_ fallback: String) -> String {
        self ?? fallback
    }
}

/// The machine the run measured: hardware, OS, Xcode, refresh rate,
/// the fixed plan shape and the pre-run baseline footprint.
struct BenchEnvironment: Codable {
    var hwModel: String
    var cpuBrand: String
    var virtualized: Bool
    var osVersion: String
    var xcodeVersion: String
    var xcodeBuild: String
    var refreshRateHertz: Double
    var nominalIntervalMilliseconds: Double
    var rowCount: Int
    var batchSize: Int
    var batchIntervalMilliseconds: Double
    var windowSize: String
    var baselineFootprintBytes: UInt64
}

/// One implementation's run: its checks, phases and memory numbers.
struct BenchRun: Codable {
    var implementation: String
    /// Every phase check this run passed.
    var ok: Bool
    var failures: [String]
    var windowWasKey: Bool
    var appWasActive: Bool
    /// Window open → the first frame showing rows, in ms.
    var firstRowsMilliseconds: Double?
    var phases: [BenchPhase]
    var memoryBaselineBytes: UInt64
    var memoryAfterLoadBytes: UInt64
    var memoryAfterScrollBytes: UInt64
    var memoryPeakBytes: UInt64
    /// `(afterScroll − afterLoad) / afterLoad × 100`.
    var memoryGrowthPercent: Double
    /// Rows the view reported on screen at the scroll phase's start —
    /// for AppKit the expanded-row count, for SwiftUI the laid-out
    /// content's row equivalent.
    var rowsOnScreenAtScroll: Int
    /// The scroll offset the phase actually reached, in points of
    /// content — proof the scroll moved.
    var scrollReachedPoints: Double
    var batches: [BenchBatchProfile] = []
}

/// Synchronous stage walls. Deferred layout/render work remains in frame/stall
/// measurements and is not attributed to these stage timers.
struct BenchBatchProfile: Codable {
    let rows: Int
    let deliveryMilliseconds: Double
    let modelMilliseconds: Double
    let insertionMilliseconds: Double?
    let expansionMilliseconds: Double?
    let parentGroups: Int?
    let newFolders: Int?
    let expansionCalls: Int?
    let visibleRows: Int?
}

/// One phase's reduced numbers: frame intervals between the phase's
/// ticks, the longest main-thread stall (a gap between tick callbacks),
/// the phase's wall time and end footprint, plus the raw intervals.
struct BenchPhase: Codable {
    var name: String
    var tickCount: Int
    var frameCount: Int
    var frameP50Milliseconds: Double
    var frameP95Milliseconds: Double
    var frameMaxMilliseconds: Double
    var maxStallMilliseconds: Double
    /// For synchronous calls (collapse/expand): the call's own wall
    /// time — the stall the ticks also see as a gap.
    var wallMilliseconds: Double?
    var footprintEndBytes: UInt64
    var frameIntervalsMilliseconds: [Double]
}

/// One pass criterion's outcome.
struct BenchCheck: Codable {
    var name: String
    var implementation: String
    var pass: Bool
    var detail: String
    var value: Double?
    var limit: Double?
    /// Whether the check counts toward the verdict — AppKit's checks
    /// gate; SwiftUI's are recorded for comparison.
    var gate: Bool
}

/// Nearest-rank percentiles over raw samples.
enum BenchStatistics {
    /// Rank ⌈p/100 · n⌉ (1-based) into the sorted samples, clamped.
    static func percentile(_ samples: [Double], p: Double) -> Double? {
        guard !samples.isEmpty else { return nil }
        let sorted = samples.sorted()
        let rank = Int((p / 100 * Double(sorted.count)).rounded(.up))
        return sorted[min(max(rank, 1), sorted.count) - 1]
    }
}
