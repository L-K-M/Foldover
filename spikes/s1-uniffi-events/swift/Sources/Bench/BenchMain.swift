// The S1 bench: throughput, echo latency, memory growth and the
// cancel path across the UniFFI bridge, ending in a JSON report and
// a short stdout summary.
//
// Usage: Bench <results.json>
//
// Exit code 0 whenever the harness ran to the end — a missed target
// is a measurement the JSON records, not a harness failure. Exit 1
// only when the bench itself could not run or could not write the
// report; exit 2 on a bad argv.

import Foundation
import S1BenchCore

/// The concurrency escapes this bench needed, filled in by hand so
/// the report answers S1's question verbatim. Empty means none: the
/// sink's `Sendable` conformance is checked (a `final` class of `let`
/// members) and batches reach the actor through a `nonisolated`
/// Mutex mailbox, which is not an escape.
let concurrencyEscapes: [String] = []

@main
struct BenchMain {
    static func main() async {
        let args = CommandLine.arguments
        guard args.count == 2 else {
            FileHandle.standardError.write(Data("usage: Bench <results.json>\n".utf8))
            exit(2)
        }
        let code = await BenchRunner(resultsPath: args[1]).run()
        exit(code)
    }
}

/// The measurements, in run order.
struct BenchRunner {
    let resultsPath: String

    /// Run the bench, write the JSON, print the summary; the process
    /// exit code.
    func run() async -> Int32 {
        do {
            let report = try await measure()
            try report.encoded().write(to: URL(fileURLWithPath: resultsPath))
            print(report.stdoutSummary)
            return 0
        } catch {
            writeFailure(error)
            print("s1-uniffi-events: harness failure: \(error)")
            return 1
        }
    }

    /// Every measurement plus the pass checks evaluated.
    private func measure() async throws -> BenchReport {
        // Warm the callback path once so the first timed run does not
        // pay static-init and first-touch costs.
        _ = await startStream(
            sink: CountingSink(collector: BatchCollector()),
            total: 50_000,
            batchSize: 64
        ).wait()

        var throughput: [StreamMeasurement] = []
        for batchSize in [UInt32(1), 64, 1024] {
            throughput.append(await measureStream(total: 1_000_000, batchSize: batchSize))
        }
        let echo = await measureEcho(calls: 1_000)
        let memory = await measureMemory(streams: 20, eventsPerStream: 100_000, batchSize: 64)
        let cancel = await measureCancel()

        let checks = passChecks(
            throughput: throughput, echo: echo, memory: memory, cancel: cancel
        )
        return BenchReport(
            spike: "s1-uniffi-events",
            ok: checks.allSatisfy(\.pass),
            failures: checks.filter { !$0.pass }.map(\.name),
            environment: BenchEnvironment(
                hwModel: Probes.sysctlString("hw.model"),
                cpuBrand: Probes.sysctlString("machdep.cpu.brand_string"),
                virtualized: Probes.sysctlInt("kern.hv_vmm_present") != 0,
                osVersion: ProcessInfo.processInfo.operatingSystemVersionString
            ),
            warmupEvents: 50_000,
            throughput: throughput,
            echo: echo,
            memory: memory,
            cancelCheck: cancel,
            concurrencyEscapes: concurrencyEscapes,
            checks: checks
        )
    }

    /// One timed stream: start, `wait`, count, in order.
    private func measureStream(total: UInt64, batchSize: UInt32) async -> StreamMeasurement {
        let collector = BatchCollector()
        let sink = CountingSink(collector: collector)
        let start = ContinuousClock.now
        let handle = startStream(sink: sink, total: total, batchSize: batchSize)
        let report = await handle.wait()
        let elapsed = seconds(since: start)
        let counters = await collector.snapshot()
        return StreamMeasurement(
            batchSize: batchSize,
            totalEvents: total,
            seconds: elapsed,
            eventsPerSecond: Double(total) / elapsed,
            deliveredEvents: report.deliveredEvents,
            deliveredBatches: report.deliveredBatches,
            countedEvents: counters.events,
            outOfOrder: counters.outOfOrder,
            completeAndOrdered: report.deliveredEvents == total
                && counters.events == total
                && counters.outOfOrder == 0
        )
    }

    /// `echoAfter(0)` round trips, sequentially, timing each.
    private func measureEcho(calls: Int) async -> EchoMeasurement {
        var samples: [Double] = []
        samples.reserveCapacity(calls)
        var badReplies = 0
        for _ in 0..<calls {
            let start = ContinuousClock.now
            let reply = await echoAfter(delayMs: 0, text: "ping")
            samples.append(seconds(since: start) * 1_000_000)
            if reply != "ping" {
                badReplies += 1
            }
        }
        let sorted = samples.sorted()
        return EchoMeasurement(
            calls: calls,
            badReplies: badReplies,
            p50Microseconds: percentile(sorted, p: 50),
            p95Microseconds: percentile(sorted, p: 95),
            maxMicroseconds: sorted.last ?? 0
        )
    }

    /// `phys_footprint` before and after `streams` repeated streams,
    /// each with its own sink and handle dropped right after `wait`.
    private func measureMemory(
        streams: Int, eventsPerStream: UInt64, batchSize: UInt32
    ) async -> MemoryMeasurement {
        let before = Probes.physicalFootprint()
        for _ in 0..<streams {
            let collector = BatchCollector()
            let sink = CountingSink(collector: collector)
            let handle = startStream(sink: sink, total: eventsPerStream, batchSize: batchSize)
            _ = await handle.wait()
        }
        let after = Probes.physicalFootprint()
        let growth = (Double(after) - Double(before)) / Double(before) * 100
        return MemoryMeasurement(
            streams: streams,
            eventsPerStream: eventsPerStream,
            batchSize: batchSize,
            footprintBeforeBytes: before,
            footprintAfterBytes: after,
            growthPercent: growth
        )
    }

    /// A stream far too long to finish is cancelled mid-flight; the
    /// report must show it stopped early.
    private func measureCancel() async -> CancelMeasurement {
        let total: UInt64 = 1_000_000_000
        let handle = startStream(
            sink: CountingSink(collector: BatchCollector()),
            total: total,
            batchSize: 1
        )
        try? await Task.sleep(for: .milliseconds(50))
        handle.cancel()
        let report = await handle.wait()
        return CancelMeasurement(
            requestedTotal: total,
            deliveredEvents: report.deliveredEvents,
            reportedCancelled: report.cancelled,
            stoppedEarly: report.deliveredEvents < total
        )
    }

    /// PLAN.md section 8 S1, evaluated against the measurements.
    private func passChecks(
        throughput: [StreamMeasurement],
        echo: EchoMeasurement,
        memory: MemoryMeasurement,
        cancel: CancelMeasurement
    ) -> [PassCheck] {
        let best = throughput.map(\.eventsPerSecond).max() ?? 0
        let ordered = throughput.allSatisfy(\.completeAndOrdered)
        return [
            PassCheck(
                name: "events/s at some batch size",
                target: ">= 50000",
                value: String(Int(best.rounded())),
                pass: best >= 50_000
            ),
            PassCheck(
                name: "memory growth over 20 streams",
                target: "< 5 %",
                value: String(format: "%.2f", memory.growthPercent),
                pass: memory.growthPercent < 5
            ),
            PassCheck(
                name: "concurrency escapes",
                target: "confined to one actor wrapper",
                value: concurrencyEscapes.isEmpty
                    ? "none" : concurrencyEscapes.joined(separator: "; "),
                pass: concurrencyEscapes.isEmpty
            ),
            PassCheck(
                name: "all events delivered in seq order",
                target: "every run",
                value: ordered ? "yes" : "no",
                pass: ordered
            ),
            PassCheck(
                name: "cancel stops a stream early",
                target: "delivered < total",
                value: "\(cancel.deliveredEvents)/\(cancel.requestedTotal)",
                pass: cancel.stoppedEarly && cancel.reportedCancelled
            ),
            PassCheck(
                name: "echo round trips intact",
                target: "0 bad replies",
                value: "\(echo.badReplies)",
                pass: echo.badReplies == 0
            ),
        ]
    }

    /// On a harness failure, write the smallest honest report so the
    /// artifact still says what happened.
    private func writeFailure(_ error: Error) {
        struct FailureReport: Codable {
            var spike: String
            var ok: Bool
            var error: String
        }
        let fallback = FailureReport(
            spike: "s1-uniffi-events", ok: false, error: "\(error)"
        )
        if let data = try? JSONEncoder().encode(fallback) {
            try? data.write(to: URL(fileURLWithPath: resultsPath))
        }
    }
}

/// Elapsed seconds since `start` from the monotonic clock.
private func seconds(since start: ContinuousClock.Instant) -> Double {
    let duration = ContinuousClock.now - start
    return Double(duration.components.seconds)
        + Double(duration.components.attoseconds) * 1e-18
}

/// The nearest-rank `p` percentile of sorted samples: rank
/// ⌈p/100 · n⌉ (1-based), clamped to [1, n]. Empty → 0.
/// Adapted from L-K-M/Sweetbriar.
private func percentile(_ sorted: [Double], p: Double) -> Double {
    guard !sorted.isEmpty else { return 0 }
    let rank = Int((p / 100 * Double(sorted.count)).rounded(.up))
    return sorted[min(max(rank, 1), sorted.count) - 1]
}
