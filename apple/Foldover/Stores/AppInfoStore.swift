// The store boundary (PLAN.md §4.5): the only file that may import
// FoldoverCore in P0. Reads the one exported call eagerly; later
// stores own the client session and event streams.

import FoldoverCore

/// Information about the Rust core, fetched once at start.
@Observable
final class AppInfoStore {
    let coreVersion: String

    init() {
        // Module-qualified: the property name shadows the UniFFI
        // function otherwise.
        coreVersion = FoldoverCore.coreVersion()
    }
}
