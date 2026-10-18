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

    init(_ replies: [PairSyncState]) {
        self.replies = replies
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
}

/// Replies what the test sets, so each request sees a chosen agent state.
private actor StubSyncClient: SyncClient {
    private var statusReply: PairSyncState = .idle
    private var previewReply: PairSyncState = .idle

    func setStatusReply(_ reply: PairSyncState) { statusReply = reply }
    func setPreviewReply(_ reply: PairSyncState) { previewReply = reply }

    func preview(_ id: String) async throws -> PairSyncState { previewReply }
    func status(_ id: String) async throws -> PairSyncState { statusReply }
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64) async throws -> PairSyncState { .idle }
    func cancel(_ id: String) async throws -> PairSyncState { .idle }
    func resolve(_ id: String, path: Data, keep: FolderSide) async throws -> PairSyncState { .idle }
}

/// Holds every status reply until the test releases it, so a checkAll
/// request can stay in flight while a manual job starts.
private actor DeferredSyncClient: SyncClient {
    private(set) var statusCalls = 0
    private var pending: [CheckedContinuation<PairSyncState, Never>] = []
    private var waiters: [(Int, CheckedContinuation<Void, Never>)] = []
    private var previewReply: PairSyncState = .idle

    func setPreviewReply(_ reply: PairSyncState) { previewReply = reply }

    func preview(_ id: String) async throws -> PairSyncState { previewReply }
    func status(_ id: String) async throws -> PairSyncState {
        statusCalls += 1
        let ready = waiters.filter { statusCalls >= $0.0 }
        waiters.removeAll { statusCalls >= $0.0 }
        for (_, continuation) in ready { continuation.resume() }
        return await withCheckedContinuation { pending.append($0) }
    }
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64) async throws -> PairSyncState { .idle }
    func cancel(_ id: String) async throws -> PairSyncState { .idle }
    func resolve(_ id: String, path: Data, keep: FolderSide) async throws -> PairSyncState { .idle }

    /// Resumes the oldest unanswered status request.
    func reply(_ state: PairSyncState) {
        pending.removeFirst().resume(returning: state)
    }

    var pendingStatusCalls: Int { pending.count }

    /// Suspends until at least `count` status requests are in flight.
    func waitForStatusCalls(_ count: Int) async {
        if statusCalls >= count { return }
        await withCheckedContinuation { waiters.append((count, $0)) }
    }
}

private func preview(archivesInA: UInt64, needsApproval: Bool) -> PreviewSummary {
    PreviewSummary(
        changes: [PlannedChange(path: Data("photos/a.jpg".utf8), action: .copy(to: .b)),
                  PlannedChange(path: Data([0x6f, 0xff]), action: .archive(on: .a))],
        copies: 1, updates: 0, archives: archivesInA, conflicts: 0, blocked: 0, kept: 0,
        a: DeletionCheck(deletions: archivesInA, paths: 100, needsApproval: needsApproval),
        b: DeletionCheck(deletions: 0, paths: 100, needsApproval: false)
    )
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
    let viewModel = PairSyncViewModel(pair: pair(), store: store, agentConnected: true)
    await viewModel.preview()
    #expect(store.state(for: pair().id) == .previewed(summary))
    #expect(viewModel.statusText.contains("1 to copy"))
    #expect(viewModel.statusText.contains("3 to archive"))
    #expect(viewModel.rows.map(\.description) == ["Copy to Folder B", "Archive in Folder A"])
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
    let viewModel = PairSyncViewModel(pair: pair(), store: store, agentConnected: true)

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
    let declining = PairSyncViewModel(pair: pair(), store: declined, agentConnected: true)
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
    let viewModel = PairSyncViewModel(pair: pair(), store: store, agentConnected: true)
    await viewModel.cancel()
    #expect(await client.cancelled)
    #expect(store.state(for: pair().id) == .idle)

    let offline = PairSyncViewModel(pair: pair(), store: store, agentConnected: false)
    #expect(!offline.canStart)
}

@MainActor
@Test
func unreachableAgentsReportErrorsWithoutChangingState() async {
    let store = SyncStore(client: UnreachableSyncClient(), pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, agentConnected: true)
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
    let viewModel = PairSyncViewModel(pair: pair(), store: store, agentConnected: true)
    await viewModel.preview()
    let rows = viewModel.rows
    #expect(rows[0].resolvable == Data("notes.txt".utf8))
    #expect(rows[1].resolvable == nil)

    await viewModel.keep(.b, for: rows[0])
    #expect(await client.kept == [.b])
    #expect(store.state(for: pair().id) == .previewed(resolved))
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

// Regression for PR57: declineApproval only flipped the local state, so
// the next scheduled checkAll read the same needsApproval from the
// agent, resurrected the dialog and reposted the notification.
@MainActor
@Test
func declinedApprovalsStayDeclinedUntilTheJobOrResultChanges() async {
    let summary = preview(archivesInA: 60, needsApproval: true)
    let expanded = preview(archivesInA: 61, needsApproval: true)
    let clean = preview(archivesInA: 0, needsApproval: false)
    let client = StubSyncClient()
    let recorder = AlertRecorder()
    let store = SyncStore(client: client, pollInterval: .milliseconds(1)) { alert in
        await recorder.record(alert)
    }
    let pair = pair()
    let viewModel = PairSyncViewModel(pair: pair, store: store, agentConnected: true)

    // The first observation never alerts, so establish the quiet baseline.
    await store.checkAll([pair])
    #expect(store.state(for: pair.id) == .idle)
    #expect(await recorder.alerts.isEmpty)

    // An automatic check that stops for approval notifies once.
    await client.setStatusReply(.needsApproval(summary))
    await store.checkAll([pair])
    #expect(store.state(for: pair.id) == .needsApproval(summary))
    #expect(viewModel.approvalRequest != nil)
    #expect(await recorder.alerts.count == 1)

    store.declineApproval(pair.id)
    #expect(store.state(for: pair.id) == .previewed(summary))
    #expect(viewModel.approvalRequest == nil)

    // The same agent result keeps the decline: no dialog, no second alert.
    await store.checkAll([pair])
    #expect(store.state(for: pair.id) == .previewed(summary))
    #expect(viewModel.approvalRequest == nil)
    #expect(await recorder.alerts.count == 1)

    // A substantively different reviewable result must surface again.
    await client.setStatusReply(.needsApproval(expanded))
    await store.checkAll([pair])
    #expect(store.state(for: pair.id) == .needsApproval(expanded))
    #expect(await recorder.alerts.count == 2)

    // A new explicit job re-arms approval requests afterwards.
    store.declineApproval(pair.id)
    await client.setPreviewReply(.previewed(clean))
    await client.setStatusReply(.needsApproval(summary))
    await store.preview(pair.id)
    #expect(store.state(for: pair.id) == .previewed(clean))
    await store.checkAll([pair])
    #expect(store.state(for: pair.id) == .needsApproval(summary))
    #expect(await recorder.alerts.count == 3)
}

// Regression for PR57: a checkAll status reply requested before a manual
// job started used to overwrite the working state — the pair looked idle
// or finished while it was still syncing and the display stopped moving.
@MainActor
@Test
func staleCheckAllRepliesCannotOverwriteManuallyStartedJobs() async {
    let summary = preview(archivesInA: 3, needsApproval: false)
    let client = DeferredSyncClient()
    await client.setPreviewReply(.working(.preview, cancelling: false))
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()

    // The scheduled check parks inside its status request.
    let check = Task { await store.checkAll([pair]) }
    await client.waitForStatusCalls(1)

    // A manual preview starts; the store begins polling the working job.
    let manual = Task { await store.preview(pair.id) }
    await client.waitForStatusCalls(2)

    // The stale reply predates the job: it must not win.
    await client.reply(.idle)
    await check.value
    #expect(store.state(for: pair.id) == .working(.preview, cancelling: false))

    // The live poll still owns the pair and lands the real result.
    while await client.pendingStatusCalls > 0 {
        await client.reply(.previewed(summary))
    }
    await manual.value
    #expect(store.state(for: pair.id) == .previewed(summary))
}
