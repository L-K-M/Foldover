// The view model layer (PLAN.md §4.5): turns store state into the
// exact strings the views render. Views never see the store or
// FoldoverCore.

import Foundation
import Observation

/// How long to hold automatic syncs.
enum PauseLength: CaseIterable, Sendable {
    case oneHour
    case fourHours
    case untilResumed

    var minutes: UInt32? {
        switch self {
        case .oneHour: 60
        case .fourHours: 240
        case .untilResumed: nil
        }
    }
}

/// What Shortcuts can ask of a pair.
enum PairAction: Sendable {
    case sync
    case preview
}

/// A pair as Shortcuts lists it.
struct IntentPair: Equatable, Sendable {
    let id: String
    let name: String
}

enum SetupProgress: Equatable {
    case done
    case current
    case upcoming
}

/// What a first-run step asks the person to do.
enum SetupAction: Equatable {
    case enableAgent
    case openLoginItems
    case newPair
}

/// One step of the first-run guide.
struct SetupStep: Identifiable, Equatable {
    let id: Int
    let title: String
    let detail: String
    let progress: SetupProgress
    let action: SetupAction?
}

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
    private let automation: AutomationStore
    /// Paired Macs, for folders they share and how those read.
    private let macToMac: MacToMacStore
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
         sync: SyncStore = SyncStore(), archive: ArchiveStore = ArchiveStore(),
         automation: AutomationStore = AutomationStore(), macToMac: MacToMacStore = MacToMacStore()) {
        self.appInfo = appInfo
        self.agent = agent
        pairStore = pairs
        syncStore = sync
        archiveStore = archive
        self.automation = automation
        self.macToMac = macToMac
    }

    /// How a pair's folder reads: its path, or a paired Mac's folder by
    /// name.
    func folderText(_ folder: SelectedFolder) -> String {
        macToMac.folderName(for: folder.bytes) ?? PairEditorViewModel.pathText(folder)
    }

    var isPaused: Bool {
        if case .paused = automation.state { return true }
        return false
    }

    /// Shown while automatic syncs are paused.
    var pauseText: String? {
        guard case .paused(let until) = automation.state else { return nil }
        guard let until else { return String(localized: "Automatic syncs are paused.") }
        return String(localized: "Automatic syncs are paused until \(until.formatted(date: .omitted, time: .shortened)).")
    }

    /// Shown while the environment holds automatic syncs (PLAN.md §3.5);
    /// a pause's own text takes the slot instead.
    var holdText: String? {
        guard !isPaused else { return nil }
        switch automation.heldBy {
        case .lowPowerMode:
            return String(localized: "Automatic syncs wait while Low Power Mode is on.")
        case .expensiveNetwork:
            return String(localized: "Automatic syncs over the network wait while this Mac's network is expensive.")
        case .constrainedNetwork:
            return String(localized: "Automatic syncs over the network wait while Low Data Mode is on.")
        case nil:
            return nil
        }
    }

    static func title(_ length: PauseLength) -> String {
        switch length {
        case .oneHour: String(localized: "For 1 Hour")
        case .fourHours: String(localized: "For 4 Hours")
        case .untilResumed: String(localized: "Until I Resume")
        }
    }

    // A pause/resume failure surfaces like any other operation error
    // and stays until the next request answers; refresh() only reads.
    func pause(_ length: PauseLength) async {
        await automation.pause(minutes: length.minutes)
        operationError = automation.error
    }
    func resume() async {
        await automation.resume()
        operationError = automation.error
    }

    /// Pause for Shortcuts and say what happened.
    func pauseForShortcuts(_ length: PauseLength) async -> String {
        await pause(length)
        return automation.error ?? pauseText ?? holdText ?? String(localized: "Automatic syncs are running.")
    }

    func resumeForShortcuts() async -> String {
        await resume()
        return automation.error ?? pauseText ?? holdText ?? String(localized: "Automatic syncs are running.")
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
                               canSync: sync.canSync)
        }
    }

    var canSyncAll: Bool { menuRows.contains(where: \.canSync) }

    func syncPair(id: String) async {
        guard let pair = pairs.first(where: { $0.id == id }) else { return }
        await sync(for: pair).sync()
    }

    /// Pairs for Shortcuts; loads them first when the app just started.
    func intentPairs() async -> [IntentPair] {
        if !pairStore.hasLoaded { await refreshAgent() }
        return pairs.map { IntentPair(id: $0.id, name: $0.settings.name) }
    }

    /// Run `action` for Shortcuts, wait for it, and say what happened.
    func perform(_ action: PairAction, onPairWithID id: String) async -> String {
        if !pairStore.hasLoaded { await refreshAgent() }
        guard let pair = pairs.first(where: { $0.id == id }) else {
            return pairError ?? agentStatusText
        }
        let before = sync(for: pair)
        let allowed = switch action {
        case .sync: before.canSync
        case .preview: before.canStart
        }
        guard allowed else {
            // A sync refused over an obsolete preview still explains
            // itself; a disconnected pair explains the agent.
            return before.canStart || before.isWorking || before.isResolving
                ? before.statusText : agentStatusText
        }
        switch action {
        case .sync: await before.sync()
        case .preview: await before.preview()
        }
        let after = sync(for: pair)
        return after.requestError ?? after.statusText
    }

    func syncAll() async {
        for pair in pairs where sync(for: pair).canSync {
            await sync(for: pair).sync()
        }
    }

    /// Keep the agent, the catalog and every pair's status current while
    /// the app runs, so automatic syncs, restarts and outside changes
    /// show up and their problems become notifications.
    func monitor(every interval: Duration = .seconds(15)) async {
        while !Task.isCancelled {
            // Inline instead of refreshAgent: a tick must not clear a
            // user-facing operationError, and PairStore.refresh skips the
            // reload while settings are saving.
            await agent.refresh()
            if case .connected = agent.connection {
                await pairStore.refresh()
                if pairStore.hasLoaded {
                    await syncStore.checkAll(pairStore.pairs)
                    await automation.refresh()
                }
            }
            try? await Task.sleep(for: interval)
        }
    }

    func sync(for pair: SyncPair) -> PairSyncViewModel {
        var connected = false
        if case .connected = agent.connection { connected = true }
        return PairSyncViewModel(pair: pair, store: syncStore, pairs: pairStore, agentConnected: connected)
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

    /// Shown instead of an empty detail until the first pair exists.
    var showsSetup: Bool { selectedPair == nil && pairs.isEmpty && !pairStore.isLoading }

    var setupSteps: [SetupStep] {
        let connected = if case .connected = agent.connection { true } else { false }
        let agentStep = SetupStep(
            id: 1, title: String(localized: "Turn on the background agent"),
            detail: agentNeedsApproval
                ? String(localized: "Allow Foldover in System Settings › General › Login Items, then refresh.")
                : String(localized: "It syncs your folders even when this window is closed."),
            progress: connected ? .done : .current,
            action: connected ? nil : (agentNeedsApproval ? .openLoginItems : .enableAgent)
        )
        let pairStep = SetupStep(
            id: 2, title: String(localized: "Create a pair"),
            detail: String(localized: "Choose two folders and whether changes go both ways or one way."),
            progress: !pairs.isEmpty ? .done : (connected ? .current : .upcoming),
            action: canConfigurePairs ? .newPair : nil
        )
        let syncStep = SetupStep(
            id: 3, title: String(localized: "Preview, then sync"),
            detail: String(localized: "Nothing changes until you sync. Replaced and removed files stay in each folder's archive for 30 days."),
            progress: .upcoming, action: nil
        )
        return [agentStep, pairStep, syncStep]
    }

    func perform(_ action: SetupAction) async {
        switch action {
        case .enableAgent: await enableAgent()
        case .openLoginItems: openLoginItems()
        case .newPair: newPair()
        }
    }

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
        editor = PairEditorViewModel(store: pairStore, macs: macToMac)
    }

    /// Open the New Pair editor with the folders among `urls` filling
    /// its sides in drop order: one folder fills Folder A, two fill
    /// both; anything else is ignored (PLAN.md §3.8).
    /// Returns whether the drop was accepted.
    @discardableResult
    func newPair(dropping urls: [URL]) -> Bool {
        guard canConfigurePairs else { return false }
        let folders = Self.droppedFolders(urls)
        guard !folders.isEmpty else { return false }
        let editor = PairEditorViewModel(store: pairStore, macs: macToMac)
        editor.chooseFolder(.success([folders[0]]), side: .left)
        if folders.count > 1 { editor.chooseFolder(.success([folders[1]]), side: .right) }
        self.editor = editor
        return true
    }

    /// The folders among dropped URLs, in drop order, at most two:
    /// only file URLs that are directories count.
    static func droppedFolders(_ urls: [URL]) -> [URL] {
        Array(urls.filter { url in
            guard url.isFileURL else { return false }
            var isDirectory: ObjCBool = false
            return FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory)
                && isDirectory.boolValue
        }.prefix(2))
    }

    func editPair() {
        guard canConfigurePairs, let selectedPair else { return }
        editor = PairEditorViewModel(store: pairStore, editing: selectedPair, macs: macToMac)
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
