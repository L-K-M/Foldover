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
    private var histories: [String: [RunRecord]] = [:]
    private var runs = 0
    private var runReply = RunReply.finish
    /// Status requests fail, as while the agent does not answer.
    private var statusFails = false
    private var undoFails = false
    private(set) var cancelledIDs: [String] = []

    func set(_ state: PairSyncState, for id: String) { states[id] = state }
    func setHistory(_ history: [RunRecord], for id: String) { histories[id] = history }
    func setRunReply(_ reply: RunReply) { runReply = reply }
    func setStatusFails(_ fails: Bool) { statusFails = fails }
    func setUndoFails(_ fails: Bool) { undoFails = fails }

    func preview(_ id: String) async throws -> PairSyncState { states[id] ?? .idle }
    func status(_ id: String) async throws -> PairSyncState {
        if statusFails { throw WindowFixtureError.unexpected }
        return states[id] ?? .idle
    }
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
        if undoFails { throw WindowFixtureError.unexpected }
        let state = PairSyncState.undone(undoSummary())
        states[id] = state
        return state
    }
    func cancel(_ id: String) async throws -> PairSyncState {
        cancelledIDs.append(id)
        states[id] = .idle
        return .idle
    }
    func resolve(_ id: String, path: Data, keep: FolderSide,
                 reviewToken: String) async throws -> PairSyncState { states[id] ?? .idle }
    func keepBoth(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState {
        states[id] ?? .idle
    }
    func skipOnce(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState {
        states[id] ?? .idle
    }
    func history(_ id: String) async throws -> [RunRecord] { histories[id] ?? [] }
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

/// Set when a task the test started has returned.
@MainActor
private final class Completion {
    var done = false
}

/// An agent the test can stop and start again.
private actor SwitchableAgent {
    private var running = true

    func set(running: Bool) { self.running = running }

    func report() throws -> AgentReport {
        guard running else { throw WindowFixtureError.unexpected }
        return AgentReport(version: "1.0")
    }
}

/// A health check that answers at once, or holds its next answer until
/// the test releases it.
private actor HeldHealthCheck {
    private let stops: UInt32?
    private var holdsNext = false
    private var held: CheckedContinuation<Void, Never>?

    init(safeModeStops stops: UInt32? = nil) { self.stops = stops }

    func holdNext() { holdsNext = true }

    func waitUntilHeld() async {
        while held == nil { await Task.yield() }
    }

    func release() {
        held?.resume()
        held = nil
    }

    func report() async -> AgentReport {
        if holdsNext {
            holdsNext = false
            await withCheckedContinuation { held = $0 }
        }
        return AgentReport(version: "1.0", safeModeStops: stops)
    }
}

/// A main window view model connected to an agent serving `pairs`.
@MainActor
private func windowModel(_ pairs: [SyncPair], sync: WindowSyncClient = WindowSyncClient(),
                         pollInterval: Duration = .milliseconds(1),
                         agent: AgentStore = AgentStore(statusRequest: { AgentReport(version: "1.0") }),
                         reveal: @escaping @MainActor ([URL]) -> Void = { _ in }) async -> MainViewModel {
    let viewModel = MainViewModel(
        appInfo: AppInfoStore(), agent: agent,
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

#if DEBUG
@MainActor
@Test(arguments: [MotionPreference.full, .reduced])
func theMenuBarPulseObservesWorkWithoutAScene(_ preference: MotionPreference) async throws {
    let photos = windowPair(photosWindowID, "Photos")
    let client = WindowSyncClient()
    let viewModel = await windowModel([photos], sync: client, pollInterval: .milliseconds(10))
    let animation = MenuBarAnimation(viewModel: viewModel, motion: MotionStore(preference: preference))

    await client.set(.working(.run, cancelling: false), for: photos.id)
    let refresh = Task { await viewModel.sync(for: photos).refresh() }
    #expect(await eventuallyInWindow { viewModel.menuBarStatus == .working })
    switch preference {
    case .full:
        #expect(await eventuallyInWindow {
            animation.imageName(for: .working) != MenuBarStatus.working.rawValue
        })
    case .reduced:
        try await Task.sleep(for: .milliseconds(450))
        #expect(animation.imageName(for: .working) == MenuBarStatus.working.rawValue)
    }

    await client.set(.idle, for: photos.id)
    await refresh.value
    #expect(await eventuallyInWindow {
        animation.imageName(for: .working) == MenuBarStatus.working.rawValue
    })
    try await Task.sleep(for: .milliseconds(450))
    #expect(animation.imageName(for: .working) == MenuBarStatus.working.rawValue)
}
#endif

@MainActor
@Test
func pairRowsSayHowEachPairReadsAtAGlance() async {
    let photos = windowPair(photosWindowID, "Photos", mode: .backupLeft)
    let docs = windowPair(docsWindowID, "Docs")
    let client = WindowSyncClient()
    let lastSuccess = Date(timeIntervalSince1970: 100)
    await client.setHistory([
        RunRecord(started: lastSuccess.addingTimeInterval(-60), finished: lastSuccess,
                  trigger: .manual, end: .completed, copied: 1, updated: 0, archived: 0,
                  recovered: 0, conflicts: 0, failures: 0, message: nil)
    ], for: photos.id)
    let viewModel = await windowModel([photos, docs], sync: client)
    #expect(viewModel.menuBarStatus == .idle)
    await viewModel.sync(for: photos).refresh()

    await client.set(.previewed(conflictPlan()), for: docs.id)
    await viewModel.sync(for: docs).preview()

    let rows = viewModel.pairRows
    #expect(rows.map(\.name) == ["Docs", "Photos"])
    #expect(rows[0].activity == .attention)
    #expect(rows[0].conflicts == 2)
    #expect(rows[0].symbol == "arrow.left.arrow.right")
    #expect(viewModel.sync(for: photos).lastSuccess == lastSuccess)
    #expect(rows[1] == PairRow(id: photos.id, name: "Photos", status: "Ready",
                               symbol: "arrow.right.to.line.compact", modeTitle: "Back up A to B",
                               activity: .idle,
                               conflicts: 0, canSync: true, canCancel: false,
                               lastSuccess: lastSuccess))
    #expect(viewModel.canSyncAll)
    #expect(viewModel.pairsLoaded)
    #expect(viewModel.agentState == .connected)
    #expect(viewModel.agentAction == nil)
    #expect(viewModel.menuBarStatus == .attention)
}

@MainActor
@Test
func accessResetWaitsForWorkOnAnUnselectedPair() async {
    let photos = windowPair(photosWindowID, "Photos")
    let docs = windowPair(docsWindowID, "Docs")
    let client = WindowSyncClient()
    let sync = SyncStore(client: client, alert: { _ in })
    let viewModel = MainViewModel(
        appInfo: AppInfoStore(), agent: AgentStore(statusRequest: { AgentReport(version: "1.0") }),
        pairs: PairStore(client: WindowCatalog([photos, docs])), sync: sync)
    #expect(!viewModel.canResetFullDiskAccess)
    await viewModel.refreshAgent()
    viewModel.selectedPairID = photos.id
    #expect(!viewModel.canResetFullDiskAccess)
    await sync.checkAll(viewModel.pairs)
    #expect(viewModel.canResetFullDiskAccess)

    await client.set(.working(.run, cancelling: false), for: docs.id)
    await sync.checkAll(viewModel.pairs)
    #expect(!viewModel.canResetFullDiskAccess)
    await client.set(.idle, for: docs.id)
    await sync.checkAll(viewModel.pairs)
    #expect(viewModel.canResetFullDiskAccess)
    await client.setRunReply(.fail)
    await sync.run(docs.id)
    #expect(!viewModel.canResetFullDiskAccess)
    await sync.checkAll(viewModel.pairs)
    #expect(viewModel.canResetFullDiskAccess)
    await client.setStatusFails(true)
    await sync.checkAll(viewModel.pairs)
    #expect(!viewModel.canResetFullDiskAccess)
}

@MainActor
@Test
func stoppingAPairFromTheMenuDoesNotStopTheSelectedPair() async {
    let photos = windowPair(photosWindowID, "Photos")
    let docs = windowPair(docsWindowID, "Docs")
    let client = WindowSyncClient()
    let viewModel = await windowModel([photos, docs], sync: client)
    viewModel.selectedPairID = photos.id

    await client.set(.working(.run, cancelling: false), for: docs.id)
    let refreshing = Task { await viewModel.sync(for: docs).refresh() }
    #expect(await eventuallyInWindow { viewModel.pairRows.first { $0.id == docs.id }?.canCancel == true })
    await viewModel.stopPair(id: docs.id)
    await refreshing.value
    #expect(await client.cancelledIDs == [docs.id])
    #expect(viewModel.selectedPairID == photos.id)
    #expect(viewModel.pairRows.allSatisfy { !$0.canCancel })

    // Idle and unknown IDs never send cancellation requests.
    await viewModel.stopPair(id: photos.id)
    await viewModel.stopPair(id: "removed")
    #expect(await client.cancelledIDs == [docs.id])
}

@MainActor
@Test
func theAgentStatusOffersOneFittingAction() async {
    let offline = MainViewModel(appInfo: AppInfoStore(),
                                agent: AgentStore(statusRequest: { throw WindowFixtureError.unexpected }))
    #expect(offline.agentState == .off)
    #expect(offline.agentAction == .turnOn)
    #expect(offline.menuBarStatus == .attention)
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
func scanErrorsStayVisibleWhileSafeChangesRemainSyncable() async {
    let pair = windowPair(photosWindowID, "Photos")
    let client = WindowSyncClient()
    let viewModel = PairSyncViewModel(pair: pair, store: SyncStore(client: client),
                                      pairs: PairStore(client: WindowCatalog([pair])), agentConnected: true)
    let denied = RunFailure(path: Data("private/notes.txt".utf8), message: "Folder A: Permission denied")
    var preview = plan([PlannedChange(path: Data("safe.txt".utf8), action: .copy(to: .b))])
    preview.failures = [denied]
    preview.failureCount = 3
    await show(.previewed(preview), on: viewModel, via: client)
    #expect(viewModel.canSync)
    #expect(viewModel.activity == .attention)
    #expect(viewModel.rows.count == 2)
    #expect(Set(viewModel.rows.map(\.id)).count == 2)
    #expect(viewModel.rows.last?.path == "private/notes.txt")
    #expect(viewModel.rows.last?.actionText == denied.message)
    #expect(viewModel.rows.last?.resolvable == nil)
    #expect(viewModel.statusText.contains("3 items could not be checked"))
    #expect(viewModel.hiddenRowsText == "2 more scan errors")
    #expect(!viewModel.canLoadMoreChanges)

    preview = plan([])
    preview.failures = [denied]
    preview.failureCount = 1
    await show(.previewed(preview), on: viewModel, via: client)
    #expect(viewModel.shortStatus != "In sync")
    #expect(viewModel.statusText != "Folders are in sync.")
    #expect(viewModel.placeholder == nil)
    #expect(viewModel.activity == .attention)

    // An inconsistent count cannot underflow while drawing the error list.
    preview.failureCount = 0
    await show(.previewed(preview), on: viewModel, via: client)
    #expect(viewModel.hiddenRowsText == nil)

    preview.failures = [RunFailure(path: Data(), message: "Folder A: Directory listing changed")]
    preview.failureCount = 1
    await show(.previewed(preview), on: viewModel, via: client)
    #expect(viewModel.rows.first?.path == "Folder")
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
    #expect(viewModel.menuBarStatus == .working)
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
    #expect(viewModel.menuBarStatus == .attention)
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

// Edit > Undo has spent its action by the time the undo runs. One the
// agent cannot take now must say why and be offered again: the record
// would otherwise keep its value, so the window would never register
// Undo again, even once the agent is back.
@MainActor
@Test
func anUndoThatCannotRunSaysWhyAndIsOfferedAgain() async throws {
    let photos = windowPair(photosWindowID, "Photos")
    let agent = SwitchableAgent()
    let client = WindowSyncClient()
    let viewModel = await windowModel([photos], sync: client,
                                      agent: AgentStore(statusRequest: { try await agent.report() }))
    await viewModel.syncInWindow(photos.id)
    let offered = try #require(viewModel.undoableSync)

    await agent.set(running: false)
    await viewModel.refreshAgent()
    #expect(viewModel.undoableSync == offered)
    await viewModel.undoSync(pairID: photos.id)
    #expect(viewModel.operationError == "The sync of “Photos” can’t be undone right now. Try again when the background agent is connected and the pair is idle.")
    let again = try #require(viewModel.undoableSync)
    #expect(again.pairID == photos.id)
    #expect(again.serial != offered.serial)

    await agent.set(running: true)
    await viewModel.refreshAgent()
    await viewModel.undoSync(pairID: photos.id)
    #expect(viewModel.undoableSync == nil)
    #expect(viewModel.operationError == nil)
}

// An undo request that fails leaves the run on screen; Edit > Undo
// offers it again, and the pair's status shows what went wrong.
@MainActor
@Test
func aFailedUndoIsOfferedAgain() async throws {
    let photos = windowPair(photosWindowID, "Photos")
    let client = WindowSyncClient()
    let viewModel = await windowModel([photos], sync: client)
    await viewModel.syncInWindow(photos.id)
    let offered = try #require(viewModel.undoableSync)

    await client.setUndoFails(true)
    await viewModel.undoSync(pairID: photos.id)
    #expect(viewModel.sync(for: photos).requestError != nil)
    let again = try #require(viewModel.undoableSync)
    #expect(again.serial != offered.serial)
    #expect(viewModel.operationError == nil)
}

// When the run's poll fails, nothing follows the job any more. The
// window's sync used to sleep in a loop until the job's state changed,
// which nothing does while the agent is away; it now asks once and stops.
@MainActor
@Test
func aWindowSyncStopsWaitingWhenTheAgentStopsAnswering() async {
    let photos = windowPair(photosWindowID, "Photos")
    let client = WindowSyncClient()
    let viewModel = await windowModel([photos], sync: client)
    await client.setRunReply(.work)
    await client.setStatusFails(true)

    let completion = Completion()
    let syncing = Task {
        await viewModel.syncInWindow(photos.id)
        completion.done = true
    }
    #expect(await eventuallyInWindow { completion.done })
    syncing.cancel()
    await syncing.value
    #expect(viewModel.sync(for: photos).requestError != nil)
    #expect(viewModel.undoableSync == nil)
}

// The detail's refresh can take a run's poller over and then go away
// when another pair is selected. The window's sync follows the job
// itself instead of waiting for the next health check.
@MainActor
@Test
func aWindowSyncFollowsItsJobWhenTheRefreshThatTookItOverGoesAway() async throws {
    let photos = windowPair(photosWindowID, "Photos")
    let client = WindowSyncClient()
    await client.setRunReply(.work)
    let viewModel = await windowModel([photos], sync: client, pollInterval: .milliseconds(100))

    let completion = Completion()
    let syncing = Task {
        await viewModel.syncInWindow(photos.id)
        completion.done = true
    }
    #expect(await eventuallyInWindow { viewModel.sync(for: photos).isWorking })
    let refreshing = Task { await viewModel.sync(for: photos).refresh() }
    // Let the refresh take over and the run's poller notice and end.
    try await Task.sleep(for: .milliseconds(250))
    refreshing.cancel()
    await refreshing.value

    await client.set(.finished(runSummary(archive: "followed")), for: photos.id)
    #expect(await eventuallyInWindow { completion.done })
    syncing.cancel()
    await syncing.value
    #expect(viewModel.undoableSync?.pairID == photos.id)
}

// A selection is remembered by file: a new listing reuses row numbers
// for other files, and the Pair menu must not act on whatever moved into
// the selected row.
@MainActor
@Test
func theSelectedChangeFollowsItsFileWhenTheListingChanges() async {
    let photos = windowPair(photosWindowID, "Photos")
    let client = WindowSyncClient()
    let first = PlannedChange(path: Data("a.txt".utf8), action: .conflict(.bothChanged))
    let second = PlannedChange(path: Data("b.txt".utf8), action: .conflict(.bothChanged))
    let copy = PlannedChange(path: Data("c.txt".utf8), action: .copy(to: .b))
    await client.set(.previewed(plan([first, second, copy], conflicts: 2)), for: photos.id)
    let viewModel = await windowModel([photos], sync: client)
    let sync = viewModel.sync(for: photos)
    await sync.preview()
    viewModel.selectedPairID = photos.id
    viewModel.selectedChangeID = 0
    #expect(viewModel.selectedChange?.path == "a.txt")

    // A new change lists first: the file moves down a row, and so does
    // the selection.
    let added = PlannedChange(path: Data("new.txt".utf8), action: .copy(to: .b))
    await show(.previewed(plan([added, first, second, copy], conflicts: 2)), on: sync, via: client)
    #expect(viewModel.selectedChangeID == 1)
    #expect(viewModel.selectedChange?.path == "a.txt")

    // A search that hides the row leaves the selection alone, so typing
    // never changes it; nothing hidden is acted on meanwhile.
    sync.searchText = "new"
    #expect(viewModel.selectedChangeID == 1)
    #expect(viewModel.selectedChange == nil)
    sync.searchText = ""
    #expect(viewModel.selectedChange?.path == "a.txt")

    // Resolved, the conflict leaves the list; b.txt, now in its old row,
    // is not selected in its place.
    await show(.previewed(plan([second, copy], conflicts: 1)), on: sync, via: client)
    #expect(viewModel.selectedChangeID == nil)
    #expect(viewModel.selectedChange == nil)
}

// A health check that fails reads as off and offers Turn On. Approval in
// Login Items is not covered: it needs a real SMAppService registration.
@MainActor
@Test
func aFailedHealthCheckReadsAsOffAndOffersTurnOn() async {
    let failing = MainViewModel(appInfo: AppInfoStore(),
                                agent: AgentStore(statusRequest: { throw WindowFixtureError.unexpected }))
    await failing.refreshAgent()
    #expect(failing.agentState == .off)
    #expect(failing.agentAction == .turnOn)
    #expect(failing.agentStatusText.hasPrefix("Background agent unavailable"))
    #expect(failing.agentSummary == failing.agentStatusText)
    #expect(failing.menuBarStatus == .attention)
}

// The monitor checks the agent every few seconds, and the agent reads as
// connecting until each check answers. The status surfaces keep the last
// answer meanwhile, safe mode's way out included, instead of flickering.
@MainActor
@Test
func aHealthCheckInFlightKeepsShowingTheLastAnswer() async {
    let health = HeldHealthCheck(safeModeStops: 3)
    let viewModel = MainViewModel(
        appInfo: AppInfoStore(),
        agent: AgentStore(statusRequest: { await health.report() }, alert: { _, _ in }),
        pairs: PairStore(client: WindowCatalog([]))
    )

    // With no answer yet, connecting reads as connecting.
    await health.holdNext()
    let first = Task { await viewModel.refreshAgent() }
    await health.waitUntilHeld()
    #expect(viewModel.agentState == .connecting)
    #expect(viewModel.agentAction == .turnOn)
    #expect(viewModel.agentStatusText == "Connecting to the background agent…")
    await health.release()
    await first.value
    #expect(viewModel.agentState == .attention)
    #expect(viewModel.agentAction == .resumeSyncing)
    let safeMode = viewModel.safeModeText
    #expect(safeMode != nil)
    #expect(viewModel.agentSummary == safeMode)

    await health.holdNext()
    let check = Task { await viewModel.refreshAgent() }
    await health.waitUntilHeld()
    #expect(viewModel.agentState == .attention)
    #expect(viewModel.agentAction == .resumeSyncing)
    #expect(viewModel.agentStatusText == "Agent 1.0 connected")
    #expect(viewModel.agentSummary == safeMode)
    #expect(viewModel.menuBarStatus == .attention)
    await health.release()
    await check.value
}

/// Turn On takes over a connection check immediately; its late result
/// cannot undo the recovery or prevent the catalog from loading.
@MainActor
@Test
func turnOnIsAvailableBeforeTheFirstHealthCheckAnswers() async {
    let health = HeldHealthCheck()
    let item = StaleLoginItem(startsAfter: 0)
    let suite = "foldover-tests-\(UUID().uuidString)"
    defer { UserDefaults.standard.removePersistentDomain(forName: suite) }
    let viewModel = MainViewModel(
        appInfo: AppInfoStore(),
        agent: AgentStore(statusRequest: { await health.report() }, system: item,
                          defaults: UserDefaults(suiteName: suite)),
        pairs: PairStore(client: WindowCatalog([]))
    )
    await health.holdNext()
    let checking = Task { await viewModel.refreshAgent() }
    await health.waitUntilHeld()
    let action = viewModel.agentAction
    #expect(action == .turnOn)
    if let action { await viewModel.perform(action) }
    #expect(viewModel.agentState == .connected)
    #expect(viewModel.pairsLoaded)

    await health.release()
    await checking.value
    #expect(viewModel.agentState == .connected)
}

@MainActor
@Test
func cancellingAHealthCheckPreservesTheConnectedState() async {
    let health = HeldHealthCheck()
    let viewModel = MainViewModel(
        appInfo: AppInfoStore(), agent: AgentStore(statusRequest: { await health.report() }),
        pairs: PairStore(client: WindowCatalog([]))
    )
    await viewModel.refreshAgent()
    await health.holdNext()
    let checking = Task { await viewModel.refreshAgent() }
    await health.waitUntilHeld()
    checking.cancel()
    await checking.value
    #expect(viewModel.agentState == .connected)
    #expect(viewModel.agentAction == nil)
    await health.release()
}

#if DEBUG
// The tour's runs need names of their own, like the engine's. With one
// name for every run, a window sync after a menu-bar sync finished with
// an identical result, so Edit > Undo never offered it.
@MainActor
@Test
func tourRunsAreToldApartSoWindowSyncsStayUndoable() async throws {
    let pairs = FixturePairClient()
    let viewModel = MainViewModel(
        appInfo: AppInfoStore(), agent: AgentStore(statusRequest: { AgentReport(version: "0.1.0") }),
        pairs: PairStore(client: pairs, reveal: { _ in }),
        sync: SyncStore(client: FixtureSyncClient(pairs: pairs), pollInterval: .milliseconds(1), alert: { _ in })
    )
    await viewModel.refreshAgent()
    await viewModel.syncPair(id: Fixtures.documentsID)
    await viewModel.syncInWindow(Fixtures.documentsID)
    #expect(viewModel.undoableSync?.pairID == Fixtures.documentsID)

    // Projects' Folder B is on this Mac, so Show in Finder has a folder to show.
    Fixtures.prepareFolders()
    let projects = try #require(Fixtures.pairs().first { $0.id == Fixtures.projectsID })
    let backup = try #require(PairStore.localURL(projects.settings.right))
    var isDirectory: ObjCBool = false
    #expect(FileManager.default.fileExists(atPath: backup.path, isDirectory: &isDirectory))
    #expect(isDirectory.boolValue)
}
#endif
