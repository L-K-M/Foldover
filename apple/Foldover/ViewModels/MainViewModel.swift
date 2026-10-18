// The view model layer (PLAN.md §4.5): turns store state into the
// exact strings the views render. Views never see the store or
// FoldoverCore.

import Foundation
import Observation

/// One pair in the menu bar.
struct MenuPairRow: Identifiable, Equatable {
    let id: String
    let name: String
    let status: String
    let canSync: Bool
}

/// Text for the main window and the menu-bar extra.
@Observable
@MainActor
final class MainViewModel {
    private let appInfo: AppInfoStore
    private let agent: AgentStore
    private let pairStore: PairStore
    private let syncStore: SyncStore
    private let archiveStore: ArchiveStore
    var selectedPairID: String?
    private(set) var editor: PairEditorViewModel?
    /// The pair whose archive browser is open.
    private(set) var archivePair: SyncPair?
    private(set) var removalTarget: SyncPair?
    private(set) var operationError: String?

    /// Main-actor isolated like the rest of the class: `FoldoverApp`
    /// (an `App`, so main-actor isolated too) builds it in its `@State`
    /// initializer.
    init(appInfo: AppInfoStore, agent: AgentStore = AgentStore(), pairs: PairStore = PairStore(),
         sync: SyncStore = SyncStore(), archive: ArchiveStore = ArchiveStore()) {
        self.appInfo = appInfo
        self.agent = agent
        pairStore = pairs
        syncStore = sync
        archiveStore = archive
    }

    var title: String { "Foldover" }
    var emptyState: String { "No sync pairs yet" }

    /// Report configuration without suggesting that a sync has run.
    var statusLine: String {
        if pairStore.loadError != nil { return String(localized: "Pair catalog unavailable") }
        guard pairStore.hasLoaded else { return String(localized: "Pairs have not loaded") }
        guard !pairs.isEmpty else { return emptyState }
        return String(localized: "Configured pairs: \(pairs.count)")
    }

    var pairs: [SyncPair] {
        pairStore.pairs.sorted {
            let order = $0.settings.name.localizedStandardCompare($1.settings.name)
            return order == .orderedSame ? $0.id < $1.id : order == .orderedAscending
        }
    }
    var selectedPair: SyncPair? { pairStore.pairs.first { $0.id == selectedPairID } }
    var detailTitle: String {
        if pairStore.isLoading { return String(localized: "Loading pairs…") }
        if pairStore.loadError != nil { return String(localized: "Cannot load pairs") }
        guard pairStore.hasLoaded else { return String(localized: "Connect to the background agent") }
        return pairs.isEmpty ? emptyState : String(localized: "Select a pair")
    }
    var pairsLoading: Bool { pairStore.isLoading }
    var pairError: String? { pairStore.loadError }
    var canConfigurePairs: Bool {
        guard case .connected = agent.connection else { return false }
        return pairStore.hasLoaded && pairStore.loadError == nil && !pairStore.isLoading && !pairStore.isSaving
    }

    var coreVersionText: String { "Core \(appInfo.coreVersion)" }

    var agentStatusText: String {
        switch agent.connection {
        case .disconnected: String(localized: "Background agent is off")
        case .connecting: String(localized: "Connecting to the background agent…")
        case .approvalRequired: String(localized: "Approve Foldover in Login Items, then refresh")
        case .connected(let version): String(localized: "Agent \(version) connected")
        case .failed(let message): String(localized: "Background agent unavailable: \(message)")
        }
    }

    var menuRows: [MenuPairRow] {
        pairs.map { pair in
            let sync = sync(for: pair)
            return MenuPairRow(id: pair.id, name: pair.settings.name, status: sync.shortStatus,
                               canSync: sync.canStart)
        }
    }

    var canSyncAll: Bool { menuRows.contains(where: \.canSync) }

    func syncPair(id: String) async {
        guard let pair = pairs.first(where: { $0.id == id }) else { return }
        await sync(for: pair).sync()
    }

    func syncAll() async {
        for pair in pairs where sync(for: pair).canStart {
            await sync(for: pair).sync()
        }
    }

    /// Keep every pair's status current while the app runs, so automatic
    /// syncs show up and their problems become notifications.
    func monitor(every interval: Duration = .seconds(15)) async {
        while !Task.isCancelled {
            if !pairStore.hasLoaded {
                await refreshAgent()
            }
            if case .connected = agent.connection, pairStore.hasLoaded {
                await syncStore.checkAll(pairStore.pairs)
            }
            try? await Task.sleep(for: interval)
        }
    }

    func sync(for pair: SyncPair) -> PairSyncViewModel {
        var connected = false
        if case .connected = agent.connection { connected = true }
        return PairSyncViewModel(pair: pair, store: syncStore, agentConnected: connected)
    }

    func archive(for pair: SyncPair) -> ArchiveViewModel {
        var connected = false
        if case .connected = agent.connection { connected = true }
        return ArchiveViewModel(pair: pair, store: archiveStore, agentConnected: connected)
    }

    func openArchive() {
        guard let selectedPair else { return }
        archivePair = selectedPair
    }

    func dismissArchive() { archivePair = nil }

    var agentNeedsApproval: Bool {
        if case .approvalRequired = agent.connection { return true }
        return false
    }

    func refreshAgent() async {
        operationError = nil
        await agent.refresh()
        if case .connected = agent.connection { await pairStore.refresh() }
    }
    func enableAgent() async {
        await agent.enable()
        if case .connected = agent.connection { await pairStore.refresh() }
    }
    func openLoginItems() { agent.openLoginItems() }

    func newPair() {
        guard canConfigurePairs else { return }
        editor = PairEditorViewModel(store: pairStore)
    }
    func editPair() {
        guard canConfigurePairs, let selectedPair else { return }
        editor = PairEditorViewModel(store: pairStore, editing: selectedPair)
    }

    func dismissEditor() {
        if let saved = editor?.savedPairID { selectedPairID = saved }
        editor = nil
    }

    func requestRemoval() {
        guard canConfigurePairs, let selectedPair else { return }
        removalTarget = selectedPair
    }

    func cancelRemoval() { removalTarget = nil }

    func removePair(_ pair: SyncPair) async {
        guard canConfigurePairs else { return }
        operationError = nil
        do {
            try await pairStore.remove(pair)
            if selectedPairID == pair.id { selectedPairID = nil }
        } catch {
            operationError = PairStore.message(for: error)
        }
    }
}
