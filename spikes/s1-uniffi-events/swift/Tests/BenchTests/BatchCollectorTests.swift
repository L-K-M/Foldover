import S1BenchCore
import Testing

@testable import Bench

@Test(arguments: [[UInt64(0), 0], [0, 2], [1, 2]])
func rejectsMissingOrDuplicateSequences(sequences: [UInt64]) async {
    let collector = BatchCollector()
    collector.enqueue(sequences.map {
        Event(seq: $0, path: "/test/\($0)", kind: .created)
    })

    let counters = await collector.snapshot()
    #expect(counters.outOfOrder > 0)
}

@Test
func acceptsContiguousSequencesAcrossBatches() async {
    let collector = BatchCollector()
    for sequences in [[UInt64(0), 1], [2, 3]] {
        collector.enqueue(sequences.map {
            Event(seq: $0, path: "/test/\($0)", kind: .created)
        })
    }

    let counters = await collector.snapshot()
    #expect(counters.events == 4)
    #expect(counters.batches == 2)
    #expect(counters.outOfOrder == 0)
}
