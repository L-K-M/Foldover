import XCTest

@testable import Bench

private func measurement() -> SoakMeasurement {
    SoakMeasurement(
        requestedSeconds: SoakMeasurement.fullHourSeconds,
        elapsedSeconds: Double(SoakMeasurement.fullHourSeconds),
        streams: 10, failedStreams: 0, retainedObjects: 0,
        samples: [
            FootprintSample(elapsedSeconds: 0, bytes: 1_000),
            FootprintSample(elapsedSeconds: 3_600, bytes: 1_010),
        ],
        heapScan: HeapScan(exitStatus: 0, output: "Process 123: 0 leaks for 0 total leaked bytes.")
    )
}

final class SoakTests: XCTestCase {
    func testAcceptsCompleteCleanSoak() {
        XCTAssertTrue(measurement().checks.allSatisfy(\.pass))
    }

    func testSmokeRunCannotPassTheHourCriterion() {
        var soak = measurement()
        soak.requestedSeconds = 10
        XCTAssertEqual(soak.checks.first?.pass, false)

        soak.requestedSeconds = SoakMeasurement.fullHourSeconds
        soak.elapsedSeconds = Double(SoakMeasurement.fullHourSeconds) - 1
        XCTAssertEqual(soak.checks.first?.pass, false)
    }

    func testRejectsMissingFootprintSamples() {
        var soak = measurement()
        soak.samples[1].bytes = 0
        XCTAssertFalse(soak.growthPercent.isFinite)
        XCTAssertFalse(soak.checks.allSatisfy(\.pass))

        soak.samples = []
        XCTAssertFalse(soak.checks.allSatisfy(\.pass))
    }

    func testRejectsGrowthRetainedObjectsAndLostEvents() {
        var soak = measurement()
        soak.samples[1].bytes = 1_050
        soak.retainedObjects = 1
        soak.failedStreams = 1
        XCTAssertEqual(soak.checks.filter { !$0.pass }.count, 3)
    }

    func testRejectsLeakingOrUnavailableHeapScan() {
        for scan in [
            HeapScan(exitStatus: 1, output: "Process 123: 1 leaks for 32 total leaked bytes."),
            HeapScan(exitStatus: 0, output: "task_for_pid failed"),
            HeapScan(exitStatus: 1, output: "Process 123: 0 leaks for 0 total leaked bytes."),
        ] {
            var soak = measurement()
            soak.heapScan = scan
            XCTAssertEqual(soak.checks.last?.pass, false)
        }
    }
}
