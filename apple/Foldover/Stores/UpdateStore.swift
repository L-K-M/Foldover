// The store boundary (PLAN.md §4.5): app updates go through
// FoldoverSystem's Sparkle wrapper.

import FoldoverSystem
import Observation

/// Whether and how this build updates itself (PLAN.md §9).
@MainActor
@Observable
final class UpdateStore {
    @ObservationIgnored private let updater = AppUpdater()

    /// False for builds made without the release signing key.
    var canUpdate: Bool { updater.isAvailable }

    func checkForUpdates() {
        updater.checkForUpdates()
    }
}
