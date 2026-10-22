// The main window's sidebar, detail, inspector and undo read everything
// from the view models; these pin the strings, symbols and state they
// hand the views.

import Foundation
import Testing
@testable import Foldover

private enum WindowFixtureError: Error { case unexpected }

private let photosWindowID = "44444444444444444444444444444444"
private let docsWindowID = "55555555555555555555555555555555"

private func windowPair(_ id: String, _ name: String, mode: PairMode = .sync,
                        left: String = "/Users/a/Docs", right: String = "/b") -> SyncPair {
    SyncPair(id: id, revision: 1, settings: PairSettings(
        name: name, left: SelectedFolder(bytes: Data(left.utf8)),
        right: SelectedFolder(bytes: Data(right.utf8)), mode: mode
    ))
}

/// A plan whose counts match its changes: conflicts as given, the rest copies.
private func plan(_ changes: [PlannedChange], conflicts: UInt64 = 0, revision: UInt64 = 1) -> PreviewSummary {
    PreviewSummary(
        changes: changes, copies: UInt64(changes.count) - conflicts, updates: 0, archives: 0,
        conflicts: conflicts, blocked: 0, kept: 0,
        a: DeletionCheck(deletions: 0, paths: 10, needsApproval: false),
        b: DeletionCheck(deletions: 0, paths: 10, needsApproval: false),
        reviewToken: String(repeating: "c", count: 64), pairRevision: revision
    )
}

private func conflictPlan(revision: UInt64 = 1) -> PreviewSummary {
    plan([PlannedChange(path: Data("a.txt".utf8), action: .conflict(.bothChanged)),
          PlannedChange(path: Data("b.txt".utf8), action: .conflict(.deletedVersusEdited))],
         conflicts: 2, revision: revision)
}

private func runSummary(conflicts: UInt64 = 0, failures: [RunFailure] = [], failureCount: UInt64 = 0,
                      cancelled: Bool = false, archive: String = "run") -> RunSummary {
    RunSummary(cancelled: cancelled, copied: 2, updated: 0, archived: 0, recovered: 0,
               conflicts: conflicts, failures: failures, failureCount: failureCount, archive: archive)
}

private func undoSummary(notUndone: [RunFailure] = []) -> UndoSummary {
    UndoSummary(cancelled: false, removed: 2, restored: 0, beside: 0, notUndone: notUndone,
                notUndoneCount: UInt64(notUndone.count), archive: "undo")
}

private actor WindowCatalog: PairClient {
    private let pairs: [SyncPair]

    init(_ pairs: [SyncPair]) { self.pairs = pairs }

    func page(after: String?) async throws -> PairCatalogPage {
        PairCatalogPage(pairs: after == nil ? pairs : [], next: nil)
    }
    func create(_ settings: PairSettings) async throws -> SyncPair { throw WindowFixtureError.unexpected }
    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair {
        throw WindowFixtureError.unexpected
    }
    func remove(_ pair: SyncPair) async throws -> SyncPair { throw WindowFixtureError.unexpected }
    func check(_ settings: PairSettings) async throws -> [PairReadinessWarning] { [] }
    func servers() async throws -> [NearbyServer] { [] }
    func browse(_ locator: String) async throws -> ServerListing {
        ServerListing(locator: locator, parent: nil, folders: [])
    }
}

/// Answers each pair with the state the test sets. Every run finishes at
/// once having copied two files, into its own archive folder so runs
/// differ, and undo reverses it.
private actor WindowSyncClient: SyncClient {
    /// How the next runs answer.
    enum RunReply {
        case finish
        /// Report working until the test sets the pair's state.
        case work
        case fail
    }

    private var states: [String: PairSyncState] = [:]
    private var runs = 0
    private var runReply = RunReply.finish

    func set(_ state: PairSyncState, for id: String) { states[id] = state }
    func setRunReply(_ reply: RunReply) { runReply = reply }

    func preview(_ id: String) async throws -> PairSyncState { states[id] ?? .idle }
    func status(_ id: String) async throws -> PairSyncState { states[id] ?? .idle }
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64,
             reviewToken: String?) async throws -> PairSyncState {
        runs += 1
        switch runReply {
        case .finish: break
        case .work:
            states[id] = .working(.run, cancelling: false)
            return .working(.run, cancelling: false)
        case .fail: throw WindowFixtureError.unexpected
        }
        let state = PairSyncState.finished(runSummary(archive: "run-\(runs)"))
        states[id] = state
        return state
    }
    func undo(_ id: String) async throws -> PairSyncState {
        let state = PairSyncState.undone(undoSummary())
        states[id] = state
        return state
    }
    func cancel(_ id: String) async throws -> PairSyncState { states[id] ?? .idle }
    func resolve(_ id: String, path: Data, keep: FolderSide,
                 reviewToken: String) async throws -> PairSyncState { states[id] ?? .idle }
    func keepBoth(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState {
        states[id] ?? .idle
    }
    func skipOnce(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState {
        states[id] ?? .idle
    }
    func history(_ id: String) async throws -> [RunRecord] { [] }
    func changes(_ id: String, after: Data?, filter: ChangeFilter?,
                 reviewToken: String) async throws -> ChangePage {
        ChangePage(changes: [], more: false)
    }
}

/// What PairStore asked Finder to show.
@MainActor
private final class RevealRecorder {
    var paths: [[String]] = []

    func record(_ urls: [URL]) { paths.append(urls.map(\.path)) }
}

/// A main window view model connected to an agent serving `pairs`.
@MainActor
private func windowModel(_ pairs: [SyncPair], sync: WindowSyncClient = WindowSyncClient(),
                         pollInterval: Duration = .milliseconds(1),
                         reveal: @escaping @MainActor ([URL]) -> Void = { _ in }) async -> MainViewModel {
    let viewModel = MainViewModel(
        appInfo: AppInfoStore(), agent: AgentStore(statusRequest: { AgentReport(version: "1.0") }),
        pairs: PairStore(client: WindowCatalog(pairs), reveal: reveal),
        sync: SyncStore(client: sync, pollInterval: pollInterval, alert: { _ in })
    )
    await viewModel.refreshAgent()
    return viewModel
}

/// Have the agent report `state` for the view model's pair, and read it.
@MainActor
private func show(_ state: PairSyncState, on viewModel: PairSyncViewModel,
                  via client: WindowSyncClient) async {
    await client.set(state, for: viewModel.pair.id)
    await viewModel.refresh()
}

/// Polls a MainActor condition with a deadline, so a state a bug never
/// reaches fails the test instead of hanging it.
@MainActor
private func eventuallyInWindow(_ condition: @MainActor () -> Bool,
                                timeout: Duration = .seconds(3)) async -> Bool {
    let deadline = ContinuousClock.now + timeout
    while !condition() {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(1))
    }
    return true
}

@MainActor
@Test
func pairRowsSayHowEachPairReadsAtAGlance() async {
    let photos = windowPair(photosWindowID, "Photos", mode: .backupLeft)
    let docs = windowPair(docsWindowID, "Docs")
    let client = WindowSyncClient()
    let viewModel = await windowModel([photos, docs], sync: client)
    #expect(viewModel.menuBarSymbol == "arrow.triangle.2.circlepath")

    await client.set(.previewed(conflictPlan()), for: docs.id)
    await viewModel.sync(for: docs).preview()

    let rows = viewModel.pairRows
    #expect(rows.map(\.name) == ["Docs", "Photos"])
    #expect(rows[0].activity == .attention)
    #expect(rows[0].conflicts == 2)
    #expect(rows[0].symbol == "arrow.left.arrow.right")
    #expect(rows[1] == PairRow(id: photos.id, name: "Photos", status: "Ready",
                               symbol: "arrow.right.to.line.compact", modeTitle: "Back up A to B",
                               activity: .idle,
                               conflicts: 0, canSync: true))
    #expect(viewModel.canSyncAll)
    #expect(viewModel.pairsLoaded)
    #expect(viewModel.agentState == .connected)
    #expect(viewModel.agentAction == nil)
    #expect(viewModel.menuBarSymbol == "exclamationmark.arrow.triangle.2.circlepath")
}

@MainActor
@Test
func theAgentStatusOffersOneFittingAction() async {
    let offline = MainViewModel(appInfo: AppInfoStore(),
                                agent: AgentStore(statusRequest: { throw WindowFixtureError.unexpected }))
    #expect(offline.agentState == .off)
    #expect(offline.agentAction == .turnOn)
    #expect(offline.menuBarSymbol == "exclamationmark.arrow.triangle.2.circlepath")
    #expect(!offline.pairsLoaded)

    let safeMode = MainViewModel(
        appInfo: AppInfoStore(),
        agent: AgentStore(statusRequest: { AgentReport(version: "1.0", safeModeStops: 3) },
                          leaveSafeModeRequest: { AgentReport(version: "1.0") },
                          alert: { _, _ in }),
        pairs: PairStore(client: WindowCatalog([]))
    )
    await safeMode.refreshAgent()
    #expect(safeMode.agentState == .attention)
    #expect(safeMode.agentAction == .resumeSyncing)

    await safeMode.perform(.resumeSyncing)
    #expect(safeMode.agentState == .connected)
    #expect(safeMode.agentAction == nil)
}

@MainActor
@Test
func eachStateShowsItsPlaceholderActivityAndConflicts() async throws {
    let pair = windowPair(photosWindowID, "Photos")
    let client = WindowSyncClient()
    let viewModel = PairSyncViewModel(pair: pair, store: SyncStore(client: client, pollInterval: .milliseconds(1)),
                                      pairs: PairStore(client: WindowCatalog([pair])), agentConnected: true)

    #expect(viewModel.placeholder == PairPlaceholder(title: "No Preview Yet", symbol: "eye",
                                                     message: viewModel.statusText, action: .preview))
    #expect(viewModel.activity == .idle)
    #expect(viewModel.conflictCount == 0)

    await show(.previewed(plan([])), on: viewModel, via: client)
    #expect(viewModel.placeholder == PairPlaceholder(title: "Folders Are in Sync", symbol: "checkmark.circle",
                                                     message: "Nothing needs to change.", action: nil))
    #expect(viewModel.activity == .idle)

    await show(.previewed(conflictPlan()), on: viewModel, via: client)
    #expect(viewModel.placeholder == nil)
    #expect(viewModel.activity == .attention)
    #expect(viewModel.conflictCount == 2)

    // A plan for another pair revision is information, not a review.
    await show(.previewed(conflictPlan(revision: 2)), on: viewModel, via: client)
    #expect(viewModel.placeholder?.title == "Preview Is Out of Date")
    #expect(viewModel.placeholder?.action == .preview)
    #expect(viewModel.activity == .idle)
    #expect(viewModel.conflictCount == 0)

    await show(.needsApproval(conflictPlan()), on: viewModel, via: client)
    #expect(viewModel.placeholder == nil)
    #expect(viewModel.activity == .attention)

    await show(.needsApproval(conflictPlan(revision: 2)), on: viewModel, via: client)
    #expect(viewModel.placeholder?.title == "Preview Is Out of Date")

    await show(.finished(runSummary()), on: viewModel, via: client)
    #expect(viewModel.placeholder == PairPlaceholder(title: "Sync Finished", symbol: "checkmark.circle",
                                                     message: viewModel.statusText, action: .preview))
    #expect(viewModel.activity == .idle)

    await show(.finished(runSummary(conflicts: 3)), on: viewModel, via: client)
    #expect(viewModel.placeholder?.title == "Sync Finished")
    #expect(viewModel.placeholder?.symbol == "exclamationmark.triangle")
    #expect(viewModel.activity == .attention)
    #expect(viewModel.conflictCount == 3)

    await show(.finished(runSummary(cancelled: true)), on: viewModel, via: client)
    #expect(viewModel.placeholder?.title == "Sync Cancelled")

    let denied = RunFailure(path: Data("locked.txt".utf8), message: "Permission denied")
    await show(.finished(runSummary(failures: [denied], failureCount: 1)), on: viewModel, via: client)
    #expect(viewModel.placeholder == nil)
    #expect(viewModel.activity == .attention)
    let failure = try #require(viewModel.rows.first)
    #expect(failure.symbol == "xmark.octagon.fill")
    #expect(failure.tone == .failure)
    #expect(failure.actionText == "Permission denied")
    #expect(failure.sizeText.isEmpty)

    await show(.undone(undoSummary()), on: viewModel, via: client)
    #expect(viewModel.placeholder?.title == "Sync Undone")
    #expect(viewModel.placeholder?.symbol == "arrow.uturn.backward.circle")
    #expect(viewModel.activity == .idle)

    await show(.undone(undoSummary(notUndone: [denied])), on: viewModel, via: client)
    #expect(viewModel.placeholder == nil)
    #expect(viewModel.activity == .attention)

    await show(.failed("Disk full"), on: viewModel, via: client)
    #expect(viewModel.placeholder == PairPlaceholder(title: "Sync Stopped", symbol: "exclamationmark.octagon",
                                                     message: "Sync stopped: Disk full", action: .preview))
    #expect(viewModel.activity == .attention)

    await show(.waiting(.b), on: viewModel, via: client)
    #expect(viewModel.placeholder == PairPlaceholder(title: "Folder B Isn't Connected",
                                                     symbol: "externaldrive.badge.xmark",
                                                     message: viewModel.statusText, action: nil))
    #expect(viewModel.activity == .waiting)

    // Working shows progress, not a placeholder.
    await client.set(.working(.run, cancelling: false), for: pair.id)
    let polling = Task { await viewModel.refresh() }
    #expect(await eventuallyInWindow { viewModel.isWorking })
    #expect(viewModel.placeholder == nil)
    #expect(viewModel.activity == .working)
    await client.set(.idle, for: pair.id)
    await polling.value
    #expect(viewModel.activity == .idle)
}

@MainActor
@Test
func changeRowsCarryTheirSymbolToneAndSizeApart() async {
    let pair = windowPair(photosWindowID, "Photos")
    let client = WindowSyncClient()
    let viewModel = PairSyncViewModel(pair: pair, store: SyncStore(client: client),
                                      pairs: PairStore(client: WindowCatalog([pair])), agentConnected: true)
    let size = PairSyncViewModel.byteText(2_000_000)
    await show(.previewed(plan([
        PlannedChange(path: Data("a.jpg".utf8), action: .copy(to: .b), bytes: 2_000_000),
        PlannedChange(path: Data("b.txt".utf8), action: .copy(to: .a)),
        PlannedChange(path: Data("c.txt".utf8), action: .archive(on: .a)),
        PlannedChange(path: Data("d.txt".utf8), action: .conflict(.bothChanged)),
        PlannedChange(path: Data("new/e.txt".utf8), action: .move(on: .b), movedFrom: Data("old/e.txt".utf8)),
    ], conflicts: 1)), on: viewModel, via: client)

    let rows = viewModel.rows
    #expect(rows.map(\.symbol) == ["arrow.right", "arrow.left", "archivebox",
                                   "exclamationmark.triangle.fill", "arrow.turn.up.right"])
    #expect(rows.map(\.tone) == [.copy, .copy, .archive, .conflict, .copy])
    #expect(rows[0].actionText == "Copy to Folder B")
    #expect(rows[0].sizeText == size)
    #expect(rows[0].description == "Copy to Folder B · \(size)")
    #expect(rows[1].sizeText.isEmpty)
    #expect(rows[4].actionText == "Move in Folder B from old/e.txt")

    #expect(PairSyncViewModel.symbol(.update(to: .a)) == "tag")
    #expect(PairSyncViewModel.symbol(.blocked) == "hourglass")
    #expect(PairSyncViewModel.symbol(.keep) == "equal.circle")
    #expect(PairSyncViewModel.tone(.update(to: .b)) == .update)
    #expect(PairSyncViewModel.tone(.blocked) == .neutral)
    #expect(PairSyncViewModel.tone(.keep) == .neutral)
}

@MainActor
@Test
func eachPairRemembersItsSelectedChange() async {
    let photos = windowPair(photosWindowID, "Photos")
    let docs = windowPair(docsWindowID, "Docs")
    let client = WindowSyncClient()
    let changes = ["a.txt", "b.txt", "c.txt"].map {
        PlannedChange(path: Data($0.utf8), action: .copy(to: .b))
    }
    await client.set(.previewed(plan(changes)), for: photos.id)
    await client.set(.previewed(plan(changes)), for: docs.id)
    let viewModel = await windowModel([photos, docs], sync: client)
    await viewModel.sync(for: photos).preview()
    await viewModel.sync(for: docs).preview()

    // Nothing to remember it for without a selected pair.
    viewModel.selectedChangeID = 1
    #expect(viewModel.selectedChangeID == nil)
    #expect(viewModel.selectedSync == nil)

    viewModel.selectedPairID = photos.id
    viewModel.selectedChangeID = 1
    #expect(viewModel.selectedSync?.pair.id == photos.id)
    #expect(viewModel.selectedChange?.path == "b.txt")

    viewModel.selectedPairID = docs.id
    #expect(viewModel.selectedChangeID == nil)
    #expect(viewModel.selectedChange == nil)
    viewModel.selectedChangeID = 0

    viewModel.selectedPairID = photos.id
    #expect(viewModel.selectedChangeID == 1)

    // An index the listed rows no longer reach selects nothing.
    viewModel.selectedChangeID = 7
    #expect(viewModel.selectedChange == nil)
}

@MainActor
@Test
func onlyWindowSyncsBecomeUndoableUntilUndone() async throws {
    let photos = windowPair(photosWindowID, "Photos")
    let viewModel = await windowModel([photos])
    viewModel.selectedPairID = photos.id
    #expect(viewModel.undoableSync == nil)

    await viewModel.syncSelected()
    let first = try #require(viewModel.undoableSync)
    #expect(first.pairID == photos.id)
    #expect(first.actionName == "Sync of “Photos”")
    #expect(viewModel.canUndoSync(pairID: photos.id))

    await viewModel.syncInWindow(photos.id)
    let second = try #require(viewModel.undoableSync)
    #expect(second.serial != first.serial)

    await viewModel.undoSync(pairID: photos.id)
    #expect(viewModel.undoableSync == nil)
    #expect(!viewModel.canUndoSync(pairID: photos.id))

    // Menu-bar syncs can be undone from the detail, never from Edit > Undo,
    // and a newer one retires the window's record.
    let menuBar = await windowModel([photos])
    await menuBar.syncPair(id: photos.id)
    #expect(menuBar.canUndoSync(pairID: photos.id))
    #expect(menuBar.undoableSync == nil)
    await menuBar.syncInWindow(photos.id)
    #expect(menuBar.undoableSync != nil)
    await menuBar.syncPair(id: photos.id)
    #expect(menuBar.undoableSync == nil)
}

// The detail view refreshes a pair when it appears, which takes over a
// running job's poller, so the window's own sync call can return while
// the run still works. Edit > Undo must still offer it once it ends.
@MainActor
@Test
func windowSyncsStayUndoableWhenARefreshTakesOverTheirPoller() async throws {
    let photos = windowPair(photosWindowID, "Photos")
    let client = WindowSyncClient()
    await client.setRunReply(.work)
    // Slow enough polling that the refresh's reply lands between two
    // polls of the run's poller instead of being outdated by one.
    let viewModel = await windowModel([photos], sync: client, pollInterval: .milliseconds(100))

    let syncing = Task { await viewModel.syncInWindow(photos.id) }
    #expect(await eventuallyInWindow { viewModel.sync(for: photos).isWorking })
    let refreshing = Task { await viewModel.sync(for: photos).refresh() }
    // Let the refresh take over and the run's poller notice and end.
    try await Task.sleep(for: .milliseconds(250))

    await client.set(.finished(runSummary(archive: "slow")), for: photos.id)
    await syncing.value
    await refreshing.value
    #expect(viewModel.undoableSync?.pairID == photos.id)
}

// A window sync whose request fails leaves the menu bar's earlier run
// on screen; Edit > Undo must not adopt that run as the window's.
@MainActor
@Test
func failedWindowSyncsDoNotAdoptAnEarlierRun() async {
    let photos = windowPair(photosWindowID, "Photos")
    let client = WindowSyncClient()
    let viewModel = await windowModel([photos], sync: client)
    await viewModel.syncPair(id: photos.id)
    #expect(viewModel.canUndoSync(pairID: photos.id))

    await client.setRunReply(.fail)
    await viewModel.syncInWindow(photos.id)
    #expect(viewModel.sync(for: photos).requestError != nil)
    #expect(viewModel.undoableSync == nil)
}

@MainActor
@Test
func aPairsPlanAndSkipsReadLikeTheEditor() {
    let editor = PairEditorViewModel(store: PairStore(client: WindowCatalog([])))
    editor.mode = .backupLeft
    editor.options.filters = ["*.tmp", "", "build/"]
    editor.options.sizeLimit = 1_000_000_000
    editor.options.schedule = .onChange
    #expect(PairEditorViewModel.planText(mode: editor.mode, options: editor.options) == editor.planText)

    let pair = SyncPair(id: photosWindowID, revision: 1, settings: PairSettings(
        name: "Photos", left: SelectedFolder(bytes: Data("/a".utf8)),
        right: SelectedFolder(bytes: Data("/b".utf8)), mode: editor.mode, options: editor.options
    ))
    let viewModel = MainViewModel(appInfo: AppInfoStore())
    #expect(viewModel.planText(for: pair) == editor.planText)
    #expect(viewModel.skipSummary(for: pair)
        == "2 rules, files larger than \(PairSyncViewModel.byteText(1_000_000_000))")
    #expect(viewModel.skipSummary(for: windowPair(docsWindowID, "Docs")) == "None")

    var aging = PairOptions.defaults
    aging.ageLimit = 30
    let old = SyncPair(id: docsWindowID, revision: 1, settings: PairSettings(
        name: "Docs", left: SelectedFolder(bytes: Data("/a".utf8)),
        right: SelectedFolder(bytes: Data("/b".utf8)), mode: .sync, options: aging
    ))
    #expect(viewModel.skipSummary(for: old) == "Files unchanged for more than 30 days")
    #expect(PairMode.mirrorRight.symbol == "arrow.left")
}

@MainActor
@Test
func foldersSayWhereTheyAreAndOnlyLocalOnesReveal() async throws {
    let local = SelectedFolder(bytes: Data("/Users/a/Docs".utf8))
    let volume = SelectedFolder(bytes: Data("/Volumes/Backup/Docs".utf8))
    let server = SelectedFolder(bytes: Data("sftp://nas.local/srv/docs".utf8))
    let peer = SelectedFolder(bytes: Data("peer://studio/docs".utf8))
    let recorder = RevealRecorder()
    let mixed = windowPair(photosWindowID, "Photos", left: "/Users/a/Docs", right: "sftp://nas.local/srv/docs")
    let viewModel = await windowModel([mixed], reveal: { recorder.record($0) })

    #expect([local, volume, server, peer].map { viewModel.folderSymbol($0) }
        == ["folder", "externaldrive", "server.rack", "laptopcomputer"])
    #expect(viewModel.localURL(local)?.path == "/Users/a/Docs")
    #expect(viewModel.localURL(volume)?.path == "/Volumes/Backup/Docs")
    #expect(viewModel.localURL(server) == nil)
    #expect(viewModel.localURL(peer) == nil)

    #expect(!viewModel.canReveal(.a))
    viewModel.selectedPairID = mixed.id
    #expect(viewModel.canReveal(.a))
    #expect(!viewModel.canReveal(.b))
    viewModel.reveal(.b)
    viewModel.reveal(.a)
    #expect(recorder.paths == [["/Users/a/Docs"]])

    // A change's file reveals only where it is on this Mac.
    let client = WindowSyncClient()
    let sync = PairSyncViewModel(pair: mixed, store: SyncStore(client: client),
                                 pairs: PairStore(client: WindowCatalog([mixed]), reveal: { recorder.record($0) }),
                                 agentConnected: true)
    await show(.previewed(plan([
        PlannedChange(path: Data("a.txt".utf8), action: .copy(to: .b)),
        PlannedChange(path: Data("b.txt".utf8), action: .copy(to: .a)),
    ])), on: sync, via: client)
    sync.reveal(sync.rows[1])
    sync.reveal(sync.rows[0])
    #expect(recorder.paths == [["/Users/a/Docs"], ["/Users/a/Docs/a.txt"]])

    let editor = PairEditorViewModel(store: PairStore(client: WindowCatalog([])))
    #expect(editor.folderSymbol(.left) == "folder.badge.plus")
    editor.chooseServer("sftp://nas.local/srv/docs", side: .right)
    #expect(editor.folderSymbol(.right) == "server.rack")
}

@MainActor
@Test
func theEditorTakesTheFirstDroppedFolder() throws {
    let root = FileManager.default.temporaryDirectory
        .appendingPathComponent("foldover-window-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: root) }
    let file = root.appendingPathComponent("note.txt")
    try Data("x".utf8).write(to: file)

    let editor = PairEditorViewModel(store: PairStore(client: WindowCatalog([])))
    #expect(!editor.chooseDropped([file], side: .left))
    #expect(editor.folderText(.left) == "Choose a folder")
    #expect(editor.chooseDropped([file, root], side: .left))
    #expect(editor.folderText(.left) == root.path)
    #expect(editor.folderSymbol(.left) == "folder")
}

@MainActor
@Test
func archiveWindowsGetTheirOwnStoreAndFindAsksForFocus() async {
    let photos = windowPair(photosWindowID, "Photos")
    let viewModel = await windowModel([photos])
    let store = viewModel.makeArchiveStore()
    #expect(viewModel.archive(for: photos.id, store: store)?.pairName == "Photos")
    #expect(viewModel.archive(for: "missing", store: store) == nil)
    #expect(viewModel.makeArchiveStore() !== store)

    let before = viewModel.findRequests
    viewModel.requestFind()
    #expect(viewModel.findRequests == before + 1)
}
