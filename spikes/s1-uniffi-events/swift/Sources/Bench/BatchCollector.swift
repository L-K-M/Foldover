// The Swift half of the bridge under test: `CountingSink`, the
// `EventSink` the Rust stream thread calls, and `BatchCollector`,
// the one actor every batch is forwarded into.

import S1BenchCore
import Synchronization

/// The single actor the sink forwards into. `onBatch` is synchronous,
/// so it cannot `await` anything actor-isolated, and a `Task` per
/// batch would spawn up to a million of them — distorting the very
/// crossing being measured. Instead the sink drops each batch into
/// `mailbox`, a `Mutex` the collector owns: the only nonisolated
/// entry point is `enqueue`, and no escape (`nonisolated(unsafe)`,
/// `@unchecked Sendable`, `@preconcurrency`) is needed.
actor BatchCollector {
    /// The running tallies a stream leaves behind. `Sendable` so the
    /// `Mutex` may hold it and `snapshot` may return it.
    struct Counters: Sendable, Equatable {
        var events: UInt64 = 0
        var batches: UInt64 = 0
        var outOfOrder: UInt64 = 0
    }

    private let mailbox = Mutex(Counters())

    /// The mailbox drop the sink calls from the Rust stream thread.
    /// `mailbox` is a `let` holding a Sendable `Mutex`, so a
    /// `nonisolated` synchronous method may lock it.
    nonisolated func enqueue(_ events: [Event]) {
        mailbox.withLock { counters in
            counters.batches += 1
            for event in events {
                // Contiguous from zero also rejects duplicates and missing events.
                if event.seq != counters.events {
                    counters.outOfOrder += 1
                }
                counters.events += 1
            }
        }
    }

    /// The tallies after the stream's `wait` resolved.
    func snapshot() -> Counters {
        mailbox.withLock { $0 }
    }
}

/// The `EventSink` the bridge calls. `final` with only `let` Sendable
/// members, so the protocol's `Sendable` requirement is checked by
/// the compiler rather than assumed.
final class CountingSink: EventSink {
    private let collector: BatchCollector

    init(collector: BatchCollector) {
        self.collector = collector
    }

    func onBatch(events: [Event]) {
        collector.enqueue(events)
    }
}
