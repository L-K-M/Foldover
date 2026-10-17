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
    private let agent: AgentStore

    /// Main-actor isolated like the rest of the class: `FoldoverApp`
    /// (an `App`, so main-actor isolated too) builds it in its `@State`
    /// initializer.
    init(appInfo: AppInfoStore, agent: AgentStore = AgentStore()) {
        self.appInfo = appInfo
        self.agent = agent
    }

    var title: String { "Foldover" }
    var emptyState: String { "No sync pairs yet" }

    /// The one glanceable status line (PLAN.md §3.2 "Quiet, glanceable
    /// trust"); P0 has no pairs, so it reports the empty state.
    var statusLine: String { emptyState }

    var coreVersionText: String { "Core \(appInfo.coreVersion)" }

    var agentStatusText: String {
        switch agent.connection {
        case .disconnected: String(localized: "Background agent is off")
        case .connecting: String(localized: "Connecting to the background agent…")
        case .approvalRequired: String(localized: "Approve Foldover in Login Items, then refresh")
        case .connected(let version): String(localized: "Agent \(version) connected. Sync is not available yet.")
        case .failed(let message): String(localized: "Background agent unavailable: \(message)")
        }
    }

    var agentNeedsApproval: Bool {
        if case .approvalRequired = agent.connection { return true }
        return false
    }

    func refreshAgent() async { await agent.refresh() }
    func enableAgent() async { await agent.enable() }
    func openLoginItems() { agent.openLoginItems() }
}
