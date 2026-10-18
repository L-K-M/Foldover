import Foundation
import Testing
@testable import Foldover

private enum SyncFixtureError: Error { case unreachable }

/// Replies with scripted states: the start reply, then one per status poll.
private actor ScriptedSyncClient: SyncClient {
    private var replies: [PairSyncState]
    private(set) var approvals: [(UInt64, UInt64)] = []
    private(set) var cancelled = false
    private(set) var kept: [FolderSide] = []
    private(set) var keptBoth: [Data] = []
    private let runs: [RunRecord]

    init(_ replies: [PairSyncState], history: [RunRecord] = []) {
        self.replies = replies
        runs = history
    }

    func history(_ id: String) async throws -> [RunRecord] { runs }

    /// Two pages of copies after the status's first page.
    func changes(_ id: String, after: Data?, filter: ChangeFilter?) async throws -> ChangePage {
        let all = (0..<4).map { PlannedChange(path: Data("p\($0)".utf8), action: .copy(to: .b)) }
        let rest = all.filter { change in after.map { $0.lexicographicallyPrecedes(change.path) } ?? true }
        let kept = filter == .archives ? [] : rest
        return ChangePage(changes: Array(kept.prefix(2)), more: kept.count > 2)
    }

    func keepBoth(_ id: String, path: Data) async throws -> PairSyncState {
        keptBoth.append(path)
        return next()
    }

    func preview(_ id: String) async throws -> PairSyncState { next() }
    func status(_ id: String) async throws -> PairSyncState { next() }

    func run(_ id: String, approvedA: UInt64, approvedB: UInt64) async throws -> PairSyncState {
        approvals.append((approvedA, approvedB))
        return next()
    }

    func cancel(_ id: String) async throws -> PairSyncState {
        cancelled = true
        return next()
    }

    func resolve(_ id: String, path: Data, keep: FolderSide) async throws -> PairSyncState {
        kept.append(keep)
        return next()
    }

    private func next() -> PairSyncState {
        replies.isEmpty ? .idle : replies.removeFirst()
    }
}

private struct UnreachableSyncClient: SyncClient {
    func preview(_ id: String) async throws -> PairSyncState { throw SyncFixtureError.unreachable }
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64) async throws -> PairSyncState {
        throw SyncFixtureError.unreachable
    }
    func status(_ id: String) async throws -> PairSyncState { throw SyncFixtureError.unreachable }
    func cancel(_ id: String) async throws -> PairSyncState { throw SyncFixtureError.unreachable }
    func resolve(_ id: String, path: Data, keep: FolderSide) async throws -> PairSyncState {
        throw SyncFixtureError.unreachable
    }
    func history(_ id: String) async throws -> [RunRecord] { throw SyncFixtureError.unreachable }
    func changes(_ id: String, after: Data?, filter: ChangeFilter?) async throws -> ChangePage {
        throw SyncFixtureError.unreachable
    }
    func keepBoth(_ id: String, path: Data) async throws -> PairSyncState {
        throw SyncFixtureError.unreachable
    }
}

private func preview(archivesInA: UInt64, needsApproval: Bool) -> PreviewSummary {
    PreviewSummary(
        changes: [PlannedChange(path: Data("photos/a.jpg".utf8), action: .copy(to: .b), bytes: 2_000_000),
                  PlannedChange(path: Data([0x6f, 0xff]), action: .archive(on: .a))],
        copies: 1, updates: 0, archives: archivesInA, conflicts: 0, blocked: 0, kept: 0,
        a: DeletionCheck(deletions: archivesInA, paths: 100, needsApproval: needsApproval),
        b: DeletionCheck(deletions: 0, paths: 100, needsApproval: false), copyBytes: 2_000_000
    )
}

/// Holds the test pair and records saved settings.
private actor CatalogClient: PairClient {
    private(set) var saved: [PairSettings] = []

    func page(after: String?) async throws -> PairCatalogPage {
        PairCatalogPage(pairs: after == nil ? [pair()] : [], next: nil)
    }
    func create(_ settings: PairSettings) async throws -> SyncPair { pair() }
    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair {
        saved.append(settings)
        return SyncPair(id: pair.id, revision: pair.revision + 1, settings: settings)
    }
    func remove(_ pair: SyncPair) async throws -> SyncPair { pair }
}

@MainActor
private func catalog(_ client: CatalogClient = CatalogClient()) -> PairStore {
    PairStore(client: client)
}

private func pair() -> SyncPair {
    SyncPair(id: "11111111111111111111111111111111", revision: 1, settings: PairSettings(
        name: "Photos", left: SelectedFolder(bytes: Data("/a".utf8)),
        right: SelectedFolder(bytes: Data("/b".utf8)), mode: .sync
    ))
}

@MainActor
@Test
func previewsPollUntilTheJobFinishes() async {
    let summary = preview(archivesInA: 3, needsApproval: false)
    let client = ScriptedSyncClient([.working(.preview, cancelling: false),
                                     .working(.preview, cancelling: false), .previewed(summary)])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    await viewModel.preview()
    #expect(store.state(for: pair().id) == .previewed(summary))
    let size = PairSyncViewModel.byteText(2_000_000)
    #expect(viewModel.statusText.contains("1 to copy (\(size))"))
    #expect(viewModel.statusText.contains("3 to archive"))
    #expect(viewModel.rows.map(\.description) == ["Copy to Folder B · \(size)", "Archive in Folder A"])
    #expect(viewModel.rows[1].path == "o\u{FFFD}")
    #expect(viewModel.hiddenRowsText == "and 2 more")
    #expect(viewModel.canStart)
}

@MainActor
@Test
func runsNeedingApprovalRetryWithTheReviewedCountsOrKeepThePreview() async throws {
    let summary = preview(archivesInA: 60, needsApproval: true)
    let result = RunSummary(cancelled: false, copied: 1, updated: 0, archived: 60, recovered: 0,
                            conflicts: 0, failures: [], failureCount: 0,
                            archive: "2026-10-18T09-30-00.123Z")
    let client = ScriptedSyncClient([.needsApproval(summary), .working(.run, cancelling: false),
                                     .finished(result)])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)

    await viewModel.sync()
    #expect(viewModel.approvalRequest?.contains("60") == true)
    let approval = try #require(viewModel.approval)
    // Dismissing the dialog declines; the captured approval still runs.
    viewModel.declineApproval()
    await viewModel.approve(approval)
    let approvals = await client.approvals
    #expect(approvals.map(\.0) == [0, 60])
    #expect(approvals.map(\.1) == [0, 0])
    #expect(viewModel.statusText.contains("archived 60"))
    #expect(viewModel.archiveNote?.contains("2026-10-18T09-30-00.123Z") == true)

    let declined = SyncStore(client: ScriptedSyncClient([.needsApproval(summary)]),
                             pollInterval: .milliseconds(1))
    let declining = PairSyncViewModel(pair: pair(), store: declined, pairs: catalog(), agentConnected: true)
    await declining.sync()
    declining.declineApproval()
    #expect(declined.state(for: pair().id) == .previewed(summary))
    #expect(declining.approvalRequest == nil)
}

@MainActor
@Test
func workingJobsCanBeCancelledAndControlsFollowTheAgent() async {
    let client = ScriptedSyncClient([.working(.run, cancelling: true), .idle])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    await viewModel.cancel()
    #expect(await client.cancelled)
    #expect(store.state(for: pair().id) == .idle)

    let offline = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: false)
    #expect(!offline.canStart)
}

@MainActor
@Test
func unreachableAgentsReportErrorsWithoutChangingState() async {
    let store = SyncStore(client: UnreachableSyncClient(), pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    await viewModel.preview()
    #expect(viewModel.requestError != nil)
    #expect(store.state(for: pair().id) == .idle)
}

@MainActor
@Test
func keepingASideResolvesThenPreviewsAgain() async throws {
    let conflicted = PreviewSummary(
        changes: [PlannedChange(path: Data("notes.txt".utf8), action: .conflict(.bothChanged)),
                  PlannedChange(path: Data("folder".utf8), action: .conflict(.typeMismatch))],
        copies: 0, updates: 0, archives: 0, conflicts: 2, blocked: 0, kept: 0,
        a: DeletionCheck(deletions: 0, paths: 2, needsApproval: false),
        b: DeletionCheck(deletions: 0, paths: 2, needsApproval: false)
    )
    let resolved = preview(archivesInA: 0, needsApproval: false)
    let client = ScriptedSyncClient([.previewed(conflicted), .previewed(conflicted), .previewed(resolved)])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    await viewModel.preview()
    let rows = viewModel.rows
    #expect(rows[0].resolvable == Data("notes.txt".utf8))
    #expect(rows[1].resolvable == nil)

    await viewModel.keep(.b, for: rows[0])
    #expect(await client.kept == [.b])
    #expect(store.state(for: pair().id) == .previewed(resolved))
}

@MainActor
@Test
func keepBothIsOfferedOnlyForFilesChangedInBothFolders() async {
    let conflicted = PreviewSummary(
        changes: [PlannedChange(path: Data("notes.txt".utf8), action: .conflict(.bothChanged)),
                  PlannedChange(path: Data("gone.txt".utf8), action: .conflict(.deletedVersusEdited))],
        copies: 0, updates: 0, archives: 0, conflicts: 2, blocked: 0, kept: 0,
        a: DeletionCheck(deletions: 0, paths: 2, needsApproval: false),
        b: DeletionCheck(deletions: 0, paths: 2, needsApproval: false)
    )
    let client = ScriptedSyncClient([.previewed(conflicted), .previewed(conflicted), .previewed(conflicted)])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    await viewModel.preview()
    let rows = viewModel.rows
    #expect(rows.map(\.keepsBoth) == [true, false])

    await viewModel.keepBoth(for: rows[1])
    await viewModel.keepBoth(for: rows[0])
    #expect(await client.keptBoth == [Data("notes.txt".utf8)])
}

private actor AlertRecorder {
    private(set) var alerts: [SyncAlert] = []
    func record(_ alert: SyncAlert) { alerts.append(alert) }
}

@MainActor
@Test
func automaticOutcomesThatNeedAPersonBecomeAlertsOnlyOnChange() async {
    let approval = preview(archivesInA: 60, needsApproval: true)
    let conflicted = RunSummary(cancelled: false, copied: 0, updated: 0, archived: 0, recovered: 0,
                                conflicts: 2, failures: [], failureCount: 0, archive: "run")
    let client = ScriptedSyncClient([.finished(conflicted), .finished(conflicted),
                                     .needsApproval(approval), .previewed(approval)])
    let recorder = AlertRecorder()
    let store = SyncStore(client: client, pollInterval: .milliseconds(1)) { alert in
        await recorder.record(alert)
    }
    // Launch: existing conflicts are not replayed.
    await store.checkAll([pair()])
    await store.checkAll([pair()])
    #expect(await recorder.alerts.isEmpty)
    await store.checkAll([pair()])
    let alerts = await recorder.alerts
    #expect(alerts.count == 1)
    #expect(alerts.first?.title == "Photos")
    #expect(alerts.first?.body.contains("60") == true)
    // Previews never alert.
    await store.checkAll([pair()])
    #expect(await recorder.alerts.count == 1)
}

@Test
func alertTextsNameTheProblem() {
    let conflicted = RunSummary(cancelled: false, copied: 0, updated: 0, archived: 0, recovered: 0,
                                conflicts: 2, failures: [], failureCount: 0, archive: "run")
    #expect(SyncStore.alert(for: pair(), state: .finished(conflicted))?.body.contains("2") == true)
    #expect(SyncStore.alert(for: pair(), state: .idle) == nil)
}

@MainActor
@Test
func finishedRunsRefreshTheHistory() async {
    let started = Date(timeIntervalSince1970: 1_000)
    let runs = [
        RunRecord(started: started, finished: started, trigger: .scheduled, end: .completed, copied: 2,
                  updated: 0, archived: 1, conflicts: 1, failures: 0, message: nil),
        RunRecord(started: started, finished: started, trigger: .manual, end: .failed, copied: 0,
                  updated: 0, archived: 0, conflicts: 0, failures: 0, message: "Disk full"),
        RunRecord(started: started, finished: started, trigger: .manual, end: .completed, copied: 0,
                  updated: 0, archived: 0, conflicts: 0, failures: 0, message: nil),
    ]
    let result = RunSummary(cancelled: false, copied: 2, updated: 0, archived: 1, recovered: 0,
                            conflicts: 1, failures: [], failureCount: 0, archive: "run")
    let client = ScriptedSyncClient([.working(.run, cancelling: false), .finished(result)], history: runs)
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    #expect(viewModel.historyRows.isEmpty)

    await viewModel.sync()
    #expect(viewModel.historyRows.map(\.summary) == [
        "Copied 2, updated 0, archived 1. 1 conflicts.", "Stopped: Disk full", "No changes.",
    ])
    #expect(viewModel.historyRows[0].when.hasSuffix("automatic"))
}

@MainActor
@Test
func automaticSyncsWaitingForAFolderSayWhichOneQuietly() async {
    let store = SyncStore(client: ScriptedSyncClient([.waiting(.b)]), pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    await viewModel.refresh()
    #expect(viewModel.statusText.hasPrefix("Folder B is not connected."))
    #expect(viewModel.shortStatus == "Waiting for folder")
    #expect(viewModel.canStart)
    #expect(SyncStore.alert(for: pair(), state: .waiting(.b)) == nil)
}

@MainActor
@Test
func skipRulesMatchExactlyOnePath() {
    #expect(PairStore.skipRule(for: Data("docs/a*.txt".utf8)) == "/docs/a\\*.txt")
    #expect(PairStore.skipRule(for: Data("odd [1]{2}?\\ ".utf8)) == "/odd \\[1\\]\\{2\\}\\?\\\\\\ ")
    #expect(PairStore.skipRule(for: Data([0x6f, 0xff])) == nil)
}

@MainActor
@Test
func alwaysSkipAddsAFilterRuleThenPreviewsAgain() async throws {
    let summary = preview(archivesInA: 0, needsApproval: false)
    let client = ScriptedSyncClient([.previewed(summary), .previewed(summary)])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pairs = CatalogClient()
    let catalog = catalog(pairs)
    await catalog.refresh()
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog, agentConnected: true)
    await viewModel.preview()
    let row = try #require(viewModel.rows.first)
    #expect(viewModel.rows.map { $0.skippable != nil } == [true, false])

    await viewModel.skipAlways(row)
    #expect(await pairs.saved.map(\.options.filters) == [["/photos/a.jpg"]])
    #expect(viewModel.requestError == nil)
}

@MainActor
@Test
func previewsListMoreChangesByKindAndResetOnTheNextPreview() async {
    let summary = preview(archivesInA: 3, needsApproval: false)
    let client = ScriptedSyncClient([.previewed(summary), .idle])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    await viewModel.preview()
    #expect(viewModel.showsChangeFilter)
    #expect(viewModel.canLoadMoreChanges)

    await viewModel.loadMoreChanges()
    #expect(viewModel.rows.map(\.path) == ["p0", "p1", "p2", "p3"])
    #expect(!viewModel.canLoadMoreChanges)
    #expect(viewModel.hiddenRowsText == nil)

    await viewModel.showChanges(.archives)
    #expect(viewModel.changeFilter == .archives)
    #expect(viewModel.rows.isEmpty)

    await viewModel.refresh()
    #expect(store.listing(for: pair().id) == nil)
}
