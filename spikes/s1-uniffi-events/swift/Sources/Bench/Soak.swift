import Foundation

/// Samples follow completed streams, after their foreign objects are released.
struct SoakMeasurement: Codable {
    static let fullHourSeconds: UInt64 = 3_600
    static let sampleIntervalSeconds: Double = 60
    static let maximumGrowthPercent: Double = 5
    static let eventsPerStream: UInt64 = 100_000
    static let batchSize: UInt32 = 1_024
    static let releasePollAttempts = 20
    static let releasePollInterval: Duration = .milliseconds(50)

    var requestedSeconds: UInt64
    var elapsedSeconds: Double
    var streams: Int
    var failedStreams: Int
    var retainedObjects: Int
    var samples: [FootprintSample]
    var heapScan: HeapScan

    var growthPercent: Double {
        guard let before = samples.first?.bytes,
              let after = samples.last?.bytes,
              samples.count >= 2,
              samples.allSatisfy({ $0.bytes > 0 }) else { return .infinity }

        return (Double(after) - Double(before)) / Double(before) * 100
    }

    var checks: [PassCheck] {
        [
            PassCheck(
                name: "one-hour soak", target: ">= \(Self.fullHourSeconds) seconds",
                value: String(format: "%.2f", elapsedSeconds),
                pass: requestedSeconds >= Self.fullHourSeconds
                    && elapsedSeconds >= Double(Self.fullHourSeconds)
                    && streams > 0
            ),
            PassCheck(
                name: "soak footprint growth", target: "< \(Self.maximumGrowthPercent) %",
                value: String(format: "%.2f", growthPercent),
                pass: growthPercent < Self.maximumGrowthPercent
            ),
            PassCheck(
                name: "soak delivery and ordering", target: "0 failed streams",
                value: "\(failedStreams)", pass: streams > 0 && failedStreams == 0
            ),
            PassCheck(
                name: "soak objects released", target: "0 retained after 1 second",
                value: "\(retainedObjects)", pass: streams > 0 && retainedObjects == 0
            ),
            PassCheck(
                name: "soak heap scan", target: "0 leaked allocations and bytes",
                value: heapScan.leakedAllocations.map(String.init) ?? "unavailable",
                pass: heapScan.exitStatus == 0
                    && heapScan.leakedAllocations == 0 && heapScan.leakedBytes == 0
            ),
        ]
    }
}

struct FootprintSample: Codable {
    var elapsedSeconds: Double
    var bytes: UInt64
}

/// Keep the scanner's output so an unavailable scan cannot look like zero leaks.
struct HeapScan: Codable, Sendable {
    var exitStatus: Int32?
    var leakedAllocations: UInt64?
    var leakedBytes: UInt64?
    var output: String

    init(exitStatus: Int32?, output: String) {
        self.exitStatus = exitStatus
        self.output = output
        let summary = output.firstMatch(
            of: /Process [0-9]+: ([0-9]+) leaks for ([0-9]+) total leaked bytes\./
        )
        leakedAllocations = summary.flatMap { UInt64($0.1) }
        leakedBytes = summary.flatMap { UInt64($0.2) }
    }
}
