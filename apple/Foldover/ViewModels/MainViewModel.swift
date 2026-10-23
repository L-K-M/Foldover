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

/// How a pair's latest state reads at a glance.
enum PairActivity: Equatable, Sendable {
    case idle
    case working
    case attention
    case waiting
}

/// One pair in the sidebar and the menu bar.
struct PairRow: Identifiable, Equatable {
    let id: String
    let name: String
    /// One or two words, as `PairSyncViewModel.shortStatus` reads.
    let status: String
    /// The direction its mode copies in.
    let symbol: String
    /// The mode's name, which the symbol stands for.
    let modeTitle: String
    let activity: PairActivity
    /// Conflicts waiting for a decision; 0 hides the badge.
    let conflicts: Int
    let canSync: Bool
}

/// The background agent's health as the status bar's dot shows it.
enum AgentIndicator: Equatable, Sendable {
    case connected
    case connecting
    /// Approval in Login Items, or syncing paused in safe mode.
    case attention
    case off
}

/// The one thing the status bar offers to do about the agent.
enum AgentAction: Equatable, Sendable {
    case turnOn
    case openLoginItems
    case resumeSyncing
    case retry
}

/// The newest main-window sync Edit > Undo can still reverse. A fresh
/// `serial` per sync tells two runs of the same pair apart.
struct UndoableSync: Equatable, Sendable {
    let pairID: String
    /// "Sync of “Photos”", completing Edit > Undo's title.
    let actionName: String
    let serial: Int
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
    private(set) var removalTarget: SyncPair?
    private(set) var operationError: String?
    /// The file Quick Look shows for the main window.
    var quickLookURL: URL?
    /// Bumped by Find (⌘F); the window focuses its search field on change.
    private(set) var findRequests = 0
    /// The path of the change selected in each pair's table, by pair ID.
    /// A path, not a row index: a new listing reuses indexes for other
    /// files.
    private var changeSelections: [String: String] = [:]
    /// A sync the main window started, with the result it finished with.
    private struct WindowSync {
        let pairID: String
        let result: RunSummary
        let serial: Int
    }
    private var windowSync: WindowSync?
    private var windowSyncSerial = 0

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

    /// The connection the status surfaces show. A health check in flight
    /// keeps showing the last settled answer, so the status bar does not
    /// flicker on every `monitor` tick; with no answer yet, connecting
    /// shows as connecting.
    private var shownConnection: AgentConnection {
        if agent.connection == .connecting, let settled = agent.lastSettled { return settled }
        return agent.connection
    }

    var agentStatusText: String {
        switch shownConnection {
        case .disconnected: String(localized: "Background agent is off")
        case .connecting: String(localized: "Connecting to the background agent…")
        case .approvalRequired: String(localized: "Approve Foldover in Login Items")
        case .connected(let version): String(localized: "Agent \(version) connected")
        case .failed(let message): String(localized: "Background agent unavailable: \(message)")
        }
    }

    /// Why nothing syncs, in crash-loop safe mode (PLAN.md §4.2).
    var safeModeText: String? {
        guard let stops = agent.safeModeStops else { return nil }
        return String(localized: "Syncing is paused: the background agent stopped unexpectedly \(stops) times in a few minutes. Resume once you have checked what changed, such as a new pair.")
    }

    var safeModeError: String? { agent.safeModeError }

    /// The agent in a line for the sidebar and the menu bar: in safe
    /// mode why nothing syncs, otherwise the connection.
    var agentSummary: String {
        guard agentAction == .resumeSyncing, let safeModeText else { return agentStatusText }
        return safeModeText
    }

    func leaveSafeMode() async {
        await agent.leaveSafeMode()
    }

    var pairRows: [PairRow] {
        pairs.map { pair in
            let pairSync = sync(for: pair)
            return PairRow(id: pair.id, name: pair.settings.name, status: pairSync.shortStatus,
                           symbol: pair.settings.mode.symbol, modeTitle: pair.settings.mode.title,
                           activity: pairSync.activity, conflicts: pairSync.conflictCount,
                           canSync: pairSync.canSync)
        }
    }

    var canSyncAll: Bool { pairRows.contains(where: \.canSync) }

    /// The menu bar icon. A problem outranks work in progress, which
    /// outranks a pause; connecting between health checks is no problem.
    var menuBarImage: String {
        let activities = pairRows.map(\.activity)
        if agentState == .off || agentState == .attention || activities.contains(.attention) {
            return "MenuBarArrowAttention"
        }
        if activities.contains(.working) { return "MenuBarArrowWorking" }
        if isPaused { return "MenuBarArrowPaused" }
        return "MenuBarArrow"
    }

    var agentState: AgentIndicator {
        switch shownConnection {
        case .connected: agent.safeModeStops == nil ? .connected : .attention
        case .connecting: .connecting
        case .approvalRequired: .attention
        case .disconnected, .failed: .off
        }
    }

    /// The trailing action the status bar offers, if any; a healthy
    /// agent needs none.
    var agentAction: AgentAction? {
        switch shownConnection {
        case .disconnected, .failed: .turnOn
        case .approvalRequired: .openLoginItems
        case .connected: agent.safeModeStops == nil ? nil : .resumeSyncing
        case .connecting: nil
        }
    }

    func perform(_ action: AgentAction) async {
        switch action {
        case .turnOn: await enableAgent()
        case .openLoginItems: openLoginItems()
        case .resumeSyncing: await leaveSafeMode()
        case .retry: await refreshAgent()
        }
    }

    func clearOperationError() { operationError = nil }

    var pairsLoaded: Bool { pairStore.hasLoaded }

    var selectedSync: PairSyncViewModel? { selectedPair.map { sync(for: $0) } }

    /// The change row selected in the main window's table, remembered
    /// per pair so switching pairs keeps each one's selection. It follows
    /// the file: when a new listing moves it to another row, that row is
    /// selected, and when the file is no longer listed, none is. A search
    /// that hides the row leaves the selection as it is, so typing in the
    /// search field does not make it change.
    var selectedChangeID: ChangeRow.ID? {
        get {
            guard let path = selectedChangePath else { return nil }
            return selectedSync?.listedRows.first { $0.path == path }?.id
        }
        set {
            guard let selectedPairID else { return }
            changeSelections[selectedPairID] = newValue.flatMap { id in
                selectedSync?.listedRows.first { $0.id == id }?.path
            }
        }
    }

    /// The selected row, while it is listed and no search hides it.
    var selectedChange: ChangeRow? {
        guard let path = selectedChangePath else { return nil }
        return selectedSync?.rows.first { $0.path == path }
    }

    private var selectedChangePath: String? { selectedPairID.flatMap { changeSelections[$0] } }

    /// Find (⌘F) asks the window to focus its search field.
    func requestFind() { findRequests += 1 }

    /// Where a pair's folder is: this Mac, an external or network
    /// volume, an SFTP server or another Mac.
    func folderSymbol(_ folder: SelectedFolder) -> String { folder.symbolName }

    /// A local folder's URL; nil for SFTP and other-Mac folders.
    func localURL(_ folder: SelectedFolder) -> URL? { PairStore.localURL(folder) }

    /// Show the selected pair's folder on `side` in Finder.
    func reveal(_ side: FolderSide) {
        guard let url = selectedFolderURL(side) else { return }
        pairStore.reveal([url])
    }

    func canReveal(_ side: FolderSide) -> Bool { selectedFolderURL(side) != nil }

    private func selectedFolderURL(_ side: FolderSide) -> URL? {
        guard let settings = selectedPair?.settings else { return nil }
        return localURL(side == .a ? settings.left : settings.right)
    }

    /// What the pair does, in the editor's words.
    func planText(for pair: SyncPair) -> String {
        PairEditorViewModel.planText(mode: pair.settings.mode, options: pair.settings.options)
    }

    /// The pair's skip rules and limits in a few words, e.g.
    /// "3 rules, files larger than 1 GB".
    func skipSummary(for pair: SyncPair) -> String {
        let clauses = PairEditorViewModel.skipClauses(pair.settings.options)
        guard !clauses.isEmpty else { return String(localized: "None") }
        let joined = clauses.joined(separator: ", ")
        return "\(joined.prefix(1).uppercased())\(joined.dropFirst())"
    }

    /// The newest sync the main window started, while Undo can still
    /// reverse it: the pair still shows that run's result. Menu-bar and
    /// Shortcuts syncs never count, and connecting between health checks
    /// does not make it flicker.
    var undoableSync: UndoableSync? {
        guard let windowSync,
              let pair = pairs.first(where: { $0.id == windowSync.pairID }),
              sync(for: pair).undoableResult == windowSync.result else { return nil }
        return UndoableSync(pairID: pair.id,
                            actionName: String(localized: "Sync of “\(pair.settings.name)”"),
                            serial: windowSync.serial)
    }

    /// Undo the named pair's last sync if it still can be. Edit > Undo
    /// has spent its action by the time this runs, and stays offered
    /// whatever the agent connection; an undo that cannot run says why,
    /// and one that cannot run or fails is offered again rather than
    /// vanishing from the menu.
    func undoSync(pairID: String) async {
        guard let pair = pairs.first(where: { $0.id == pairID }) else { return }
        let pairSync = sync(for: pair)
        guard pairSync.canUndo else {
            operationError = String(localized: "The sync of “\(pair.settings.name)” can’t be undone right now. Try again when the background agent is connected and the pair is idle.")
            rearmWindowSync(pairID)
            return
        }
        await pairSync.undo()
        // A request that failed leaves the same result on screen.
        rearmWindowSync(pairID)
    }

    /// Offer the window's sync again under a fresh serial while the pair
    /// still shows its result, so the window registers Edit > Undo anew.
    private func rearmWindowSync(_ pairID: String) {
        guard let windowSync, windowSync.pairID == pairID,
              let pair = pairs.first(where: { $0.id == pairID }),
              sync(for: pair).undoableResult == windowSync.result else { return }
        windowSyncSerial += 1
        self.windowSync = WindowSync(pairID: pairID, result: windowSync.result, serial: windowSyncSerial)
    }

    func canUndoSync(pairID: String) -> Bool {
        guard let pair = pairs.first(where: { $0.id == pairID }) else { return false }
        return sync(for: pair).canUndo
    }

    /// Sync from the main window (toolbar, Pair menu, sidebar, detail).
    /// Once the run finishes, Edit > Undo can reverse it. The detail's
    /// refresh can take the run's poller over, so the sync call may
    /// return while the run still works; the record waits for the run.
    func syncInWindow(_ pairID: String) async {
        guard let pair = pairs.first(where: { $0.id == pairID }) else { return }
        let pairSync = sync(for: pair)
        // A refused sync must not adopt an older run's result.
        guard pairSync.canSync else { return }
        let before = pairSync.undoableResult
        await pairSync.sync()
        await syncStore.waitWhileWorking(pairID)
        recordWindowSync(pairID, after: before)
    }

    /// Run the plan the approval dialog presented; like Sync Now from the
    /// window, Edit > Undo can then reverse it.
    func approveInWindow(_ request: ApprovalRequest) async {
        guard let pair = pairs.first(where: { $0.id == request.pairID }) else { return }
        let pairSync = sync(for: pair)
        let before = pairSync.undoableResult
        await pairSync.approve(request)
        await syncStore.waitWhileWorking(pair.id)
        recordWindowSync(pair.id, after: before)
    }

    /// Remember a window sync that finished with something to undo. A
    /// request that failed leaves `before`, a run the window may not have
    /// started, on screen; that and a run with nothing to undo leave the
    /// previous record, which still names the newest undoable run.
    private func recordWindowSync(_ pairID: String, after before: RunSummary?) {
        guard let pair = pairs.first(where: { $0.id == pairID }),
              let result = sync(for: pair).undoableResult, result != before else { return }
        windowSyncSerial += 1
        windowSync = WindowSync(pairID: pairID, result: result, serial: windowSyncSerial)
    }

    func syncSelected() async {
        guard let selectedPairID else { return }
        await syncInWindow(selectedPairID)
    }

    func previewSelected() async {
        guard let selectedSync, selectedSync.canStart else { return }
        await selectedSync.preview()
    }

    func stopSelected() async {
        guard let selectedSync, selectedSync.canCancel else { return }
        await selectedSync.cancel()
    }

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

    /// At launch, before `monitor`: restart the agent from this app when
    /// the app changed since the agent was registered (see
    /// `AgentStore.reregisterIfUpdated`).
    func restartAgentIfUpdated() async {
        await agent.reregisterIfUpdated()
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

    private var agentConnected: Bool {
        if case .connected = agent.connection { return true }
        return false
    }

    func sync(for pair: SyncPair) -> PairSyncViewModel {
        PairSyncViewModel(pair: pair, store: syncStore, pairs: pairStore, agentConnected: agentConnected)
    }

    /// A store for one archive window, so windows browse independently.
    func makeArchiveStore() -> ArchiveStore { archiveStore.sibling() }

    /// The archive browser for the pair with `pairID`; nil once the pair
    /// is gone.
    func archive(for pairID: String, store: ArchiveStore) -> ArchiveViewModel? {
        guard let pair = pairStore.pairs.first(where: { $0.id == pairID }) else { return nil }
        return ArchiveViewModel(pair: pair, store: store, agentConnected: agentConnected)
    }

    /// Shown instead of an empty detail until the first pair exists.
    var showsSetup: Bool { selectedPair == nil && pairs.isEmpty && !pairStore.isLoading }

    var setupSteps: [SetupStep] {
        let connected = if case .connected = agent.connection { true } else { false }
        let agentStep = SetupStep(
            id: 1, title: String(localized: "Turn on the background agent"),
            detail: agentNeedsApproval
                ? String(localized: "Allow Foldover in System Settings › General › Login Items. Foldover connects once it is allowed.")
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
