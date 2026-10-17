import S1BenchCore
import XCTest

@testable import Bench

@MainActor
final class BatchCollectorTests: XCTestCase {
    func testRejectsMissingOrDuplicateSequences() async {
        for sequences in [[UInt64(0), 0], [0, 2], [1, 2]] {
            let collector = BatchCollector()
            collector.enqueue(sequences.map {
                Event(seq: $0, path: "/test/\($0)", kind: .created)
            })

            let counters = await collector.snapshot()
            XCTAssertGreaterThan(counters.outOfOrder, 0)
        }
    }

    func testAcceptsContiguousSequencesAcrossBatches() async {
        let collector = BatchCollector()
        for sequences in [[UInt64(0), 1], [2, 3]] {
            collector.enqueue(sequences.map {
                Event(seq: $0, path: "/test/\($0)", kind: .created)
            })
        }

        let counters = await collector.snapshot()
        XCTAssertEqual(counters.events, 4)
        XCTAssertEqual(counters.batches, 2)
        XCTAssertEqual(counters.outOfOrder, 0)
    }
}
