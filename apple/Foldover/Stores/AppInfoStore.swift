// The store boundary (PLAN.md §4.5): stores own core calls and sessions.
// The workspace version is immutable, so read it once.

import FoldoverCore
import Observation

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
