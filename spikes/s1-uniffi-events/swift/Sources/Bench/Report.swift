// The report the bench writes: every measurement, the pass criteria
// evaluated against PLAN.md section 8 S1, and a stdout summary.

import Foundation

/// The whole report. `ok` is every check having passed; `failures`
/// names the misses. A miss is a measurement, not a harness failure:
/// the process still exits 0.
struct BenchReport: Codable {
    var spike: String
    var ok: Bool
    var failures: [String]
    var environment: BenchEnvironment
    var warmupEvents: UInt64
    var throughput: [StreamMeasurement]
    var echo: EchoMeasurement
    var memory: MemoryMeasurement
    var cancelCheck: CancelMeasurement
    var concurrencyEscapes: [String]
    var checks: [PassCheck]

    /// Pretty-printed, sorted-keys JSON for the results file.
    func encoded() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return try encoder.encode(self)
    }

    /// The short human summary run.sh's stdout carries into the
    /// GitHub step summary.
    var stdoutSummary: String {
        var lines = ["s1-uniffi-events: ok=\(ok)"]
        for failure in failures {
            lines.append("  failed: \(failure)")
        }
        for run in throughput {
            lines.append(
                "  events/s batch \(run.batchSize): "
                    + "\(Int(run.eventsPerSecond.rounded())) "
                    + "(\(run.deliveredEvents)/\(run.totalEvents) in "
                    + "\(String(format: "%.2f", run.seconds))s)"
            )
        }
        lines.append(
            "  echo p50/p95 us: "
                + "\(String(format: "%.0f", echo.p50Microseconds))/"
                + "\(String(format: "%.0f", echo.p95Microseconds)) "
                + "over \(echo.calls) calls"
        )
        lines.append(
            "  memory growth over \(memory.streams) streams: "
                + "\(String(format: "%.2f", memory.growthPercent))%"
        )
        lines.append(
            "  concurrency escapes: "
                + (concurrencyEscapes.isEmpty ? "none" : concurrencyEscapes.joined(separator: "; "))
        )
        return lines.joined(separator: "\n")
    }
}

/// The machine the run measured: sysctl probes plus the OS version.
struct BenchEnvironment: Codable {
    var hwModel: String
    var cpuBrand: String
    var virtualized: Bool
    var osVersion: String
}

/// One throughput run's outcome: wall time, what Rust reported
/// delivering, what the actor counted, and whether it all arrived in
/// order.
struct StreamMeasurement: Codable {
    var batchSize: UInt32
    var totalEvents: UInt64
    var seconds: Double
    var eventsPerSecond: Double
    var deliveredEvents: UInt64
    var deliveredBatches: UInt64
    var countedEvents: UInt64
    var outOfOrder: UInt64
    var completeAndOrdered: Bool
}

/// `echoAfter` round-trip latency: p50/p95/max over the raw samples
/// in microseconds.
struct EchoMeasurement: Codable {
    var calls: Int
    var badReplies: Int
    var p50Microseconds: Double
    var p95Microseconds: Double
    var maxMicroseconds: Double
}

/// `phys_footprint` around the repeated streams, as growth percent.
struct MemoryMeasurement: Codable {
    var streams: Int
    var eventsPerStream: UInt64
    var batchSize: UInt32
    var footprintBeforeBytes: UInt64
    var footprintAfterBytes: UInt64
    var growthPercent: Double
}

/// Whether `cancel` actually stopped a long stream early.
struct CancelMeasurement: Codable {
    var requestedTotal: UInt64
    var deliveredEvents: UInt64
    var reportedCancelled: Bool
    var stoppedEarly: Bool
}

/// One pass criterion (PLAN.md section 8 S1), evaluated.
struct PassCheck: Codable {
    var name: String
    var target: String
    var value: String
    var pass: Bool
}
