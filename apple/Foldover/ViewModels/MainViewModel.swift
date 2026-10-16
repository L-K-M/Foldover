// The view model layer (PLAN.md §4.5): turns store state into the
// exact strings the views render. Views never see the store or
// FoldoverCore.

import Foundation
import Observation

/// Text for the main window and the menu-bar extra.
@Observable
@MainActor
final class MainViewModel {
    private let appInfo: AppInfoStore

    /// Main-actor isolated like the rest of the class: `FoldoverApp`
    /// (an `App`, so main-actor isolated too) builds it in its `@State`
    /// initializer.
    init(appInfo: AppInfoStore) {
        self.appInfo = appInfo
    }

    var title: String { "Foldover" }
    var emptyState: String { "No sync pairs yet" }

    /// The one glanceable status line (PLAN.md §3.2 "Quiet, glanceable
    /// trust"); P0 has no pairs, so it reports the empty state.
    var statusLine: String { emptyState }

    var coreVersionText: String { "Core \(appInfo.coreVersion)" }
}
