import Testing

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

@Test
func acceptsCompleteCleanSoak() {
    #expect(measurement().checks.allSatisfy { $0.pass })
}

@Test
func smokeRunCannotPassTheHourCriterion() {
    var soak = measurement()
    soak.requestedSeconds = 10
    #expect(soak.checks.first?.pass == false)

    soak.requestedSeconds = SoakMeasurement.fullHourSeconds
    soak.elapsedSeconds = Double(SoakMeasurement.fullHourSeconds) - 1
    #expect(soak.checks.first?.pass == false)
}

@Test
func rejectsMissingFootprintSamples() {
    var soak = measurement()
    soak.samples[1].bytes = 0
    #expect(!soak.growthPercent.isFinite)
    #expect(!soak.checks.allSatisfy { $0.pass })

    soak.samples = []
    #expect(!soak.checks.allSatisfy { $0.pass })
}

@Test
func rejectsGrowthRetainedObjectsAndLostEvents() {
    var soak = measurement()
    soak.samples[1].bytes = 1_050
    soak.retainedObjects = 1
    soak.failedStreams = 1
    #expect(soak.checks.filter { !$0.pass }.count == 3)
}

@Test(arguments: [
    HeapScan(exitStatus: 1, output: "Process 123: 1 leaks for 32 total leaked bytes."),
    HeapScan(exitStatus: 0, output: "task_for_pid failed"),
    HeapScan(exitStatus: 1, output: "Process 123: 0 leaks for 0 total leaked bytes."),
])
func rejectsLeakingOrUnavailableHeapScan(scan: HeapScan) {
    var soak = measurement()
    soak.heapScan = scan
    #expect(soak.checks.last?.pass == false)
}
