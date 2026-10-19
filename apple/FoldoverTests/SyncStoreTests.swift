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
    private var previewError: (any Error)?

    func setStatusReply(_ reply: PairSyncState) { statusReply = reply }
    func setPreviewReply(_ reply: PairSyncState) { previewReply = reply }
    func setPreviewError(_ error: (any Error)?) { previewError = error }

    func preview(_ id: String) async throws -> PairSyncState {
        if let previewError { throw previewError }
        return previewReply
    }
    func status(_ id: String) async throws -> PairSyncState { statusReply }
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64) async throws -> PairSyncState { .idle }
    func cancel(_ id: String) async throws -> PairSyncState { .idle }
    func resolve(_ id: String, path: Data, keep: FolderSide) async throws -> PairSyncState { .idle }
}

/// Holds every status reply until the test releases it, so a checkAll
/// request can stay in flight while a manual job starts.
private actor DeferredSyncClient: SyncClient {
    private(set) var statusCalls = 0
    private(set) var previewCalls = 0
    private(set) var runCalls = 0
    private(set) var cancelled = false
    private var pending: [CheckedContinuation<PairSyncState, any Error>] = []
    private var waiters: [(id: UUID, count: Int, then: CheckedContinuation<Bool, Never>)] = []
    private var pendingStarts: [CheckedContinuation<PairSyncState, any Error>] = []
    private var startWaiters: [(id: UUID, job: SyncJob, count: Int,
                               then: CheckedContinuation<Bool, Never>)] = []
    private var holdStarts = false
    private var previewReply: PairSyncState = .idle

    func setPreviewReply(_ reply: PairSyncState) { previewReply = reply }
    /// Park preview/run requests until the test releases them.
    func holdStartReplies() { holdStarts = true }

    func preview(_ id: String) async throws -> PairSyncState {
        started(.preview)
        guard holdStarts else { return previewReply }
        return try await withCheckedThrowingContinuation { pendingStarts.append($0) }
    }
    func status(_ id: String) async throws -> PairSyncState {
        statusCalls += 1
        let ready = waiters.filter { statusCalls >= $0.count }
        waiters.removeAll { statusCalls >= $0.count }
        for waiter in ready { waiter.then.resume(returning: true) }
        return try await withCheckedThrowingContinuation { pending.append($0) }
    }
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64) async throws -> PairSyncState {
        started(.run)
        guard holdStarts else { return .idle }
        return try await withCheckedThrowingContinuation { pendingStarts.append($0) }
    }
    func cancel(_ id: String) async throws -> PairSyncState {
        cancelled = true
        return .idle
    }
    func resolve(_ id: String, path: Data, keep: FolderSide) async throws -> PairSyncState { .idle }

    private func started(_ job: SyncJob) {
        switch job {
        case .preview: previewCalls += 1
        case .run: runCalls += 1
        }
        let calls = job == .preview ? previewCalls : runCalls
        let ready = startWaiters.filter { $0.job == job && calls >= $0.count }
        startWaiters.removeAll { $0.job == job && calls >= $0.count }
        for waiter in ready { waiter.then.resume(returning: true) }
    }

    /// Resumes the oldest unanswered status request.
    func reply(_ state: PairSyncState) {
        pending.removeFirst().resume(returning: state)
    }

    /// Resumes the newest unanswered status request.
    func replyLatest(_ state: PairSyncState) {
        pending.removeLast().resume(returning: state)
    }

    /// Fails the oldest unanswered status request.
    func replyError(_ error: any Error) {
        pending.removeFirst().resume(throwing: error)
    }

    /// Resumes the oldest parked preview/run request.
    func replyStart(_ state: PairSyncState) {
        pendingStarts.removeFirst().resume(returning: state)
    }

    var pendingStatusCalls: Int { pending.count }
    var pendingStartCalls: Int { pendingStarts.count }

    /// Suspends until at least `count` status requests are in flight.
    func waitForStatusCalls(_ count: Int) async {
        _ = await waitForStatusCalls(count, timeout: .seconds(30))
    }

    /// Bounded variant: a request a bug suppresses fails instead of
    /// hanging the test.
    @discardableResult
    func waitForStatusCalls(_ count: Int, timeout: Duration) async -> Bool {
        if statusCalls >= count { return true }
        let id = UUID()
        return await withCheckedContinuation { continuation in
            waiters.append((id, count, continuation))
            Task {
                try? await Task.sleep(for: timeout)
                self.expire(id)
            }
        }
    }

    /// Bounded wait on preview/run requests of one kind, so a start a
    /// bug suppresses fails instead of hanging the test.
    @discardableResult
    func waitForStartCalls(_ job: SyncJob, _ count: Int, timeout: Duration) async -> Bool {
        let calls = job == .preview ? previewCalls : runCalls
        if calls >= count { return true }
        let id = UUID()
        return await withCheckedContinuation { continuation in
            startWaiters.append((id, job, count, continuation))
            Task {
                try? await Task.sleep(for: timeout)
                self.expireStartWaiter(id)
            }
        }
    }

    private func expire(_ id: UUID) {
        guard let index = waiters.firstIndex(where: { $0.id == id }) else { return }
        waiters.remove(at: index).then.resume(returning: false)
    }

    private func expireStartWaiter(_ id: UUID) {
        guard let index = startWaiters.firstIndex(where: { $0.id == id }) else { return }
        startWaiters.remove(at: index).then.resume(returning: false)
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

/// Polls a MainActor condition with a deadline, so an effect a bug
/// suppresses fails the test instead of hanging it.
@MainActor
private func eventually(_ condition: @MainActor () -> Bool,
                        timeout: Duration = .seconds(3)) async -> Bool {
    let deadline = ContinuousClock.now + timeout
    while !condition() {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(1))
    }
    return true
}

// The stale-reply guard counts writes, not state values: a pair can
// leave .idle and cycle back to it while a checkAll request is in
// flight, and that reply is still stale (ABA).
@MainActor
@Test
func staleCheckAllRepliesStayDroppedWhenTheStateCyclesBack() async {
    let summary = preview(archivesInA: 60, needsApproval: true)
    let client = DeferredSyncClient()
    await client.setPreviewReply(.working(.preview, cancelling: false))
    let recorder = AlertRecorder()
    let store = SyncStore(client: client, pollInterval: .milliseconds(1)) { alert in
        await recorder.record(alert)
    }
    let pair = pair()

    let check = Task { await store.checkAll([pair]) }
    await client.waitForStatusCalls(1)

    // A manual job cycles the pair .idle → .working → .idle.
    let manual = Task { await store.preview(pair.id) }
    await client.waitForStatusCalls(2)
    await client.replyLatest(.idle)
    await manual.value
    #expect(store.state(for: pair.id) == .idle)

    // The parked checkAll reply must not resurrect the approval even
    // though the pair is .idle again, the value its request started on.
    await client.reply(.needsApproval(summary))
    await check.value
    #expect(store.state(for: pair.id) == .idle)
    #expect(await recorder.alerts.isEmpty)
}

// Regression for PR57 follow-up: the live poller wrote each status reply
// unconditionally. A reply requested before a cancel-and-restart was
// stale, could overwrite the new job's .working with a terminal result,
// and then retired the only poller — the new job never updated again.
@MainActor
@Test
func staleLivePollRepliesCannotRetireANewerJob() async {
    let stale = preview(archivesInA: 3, needsApproval: false)
    let done = preview(archivesInA: 0, needsApproval: false)
    let client = DeferredSyncClient()
    await client.setPreviewReply(.working(.preview, cancelling: false))
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()

    // A working job is being polled; its status request is in flight.
    let first = Task { await store.preview(pair.id) }
    await client.waitForStatusCalls(1)

    // An explicit cancel lands .idle, then a new job starts .working
    // while the old poller is still suspended on its status request.
    await store.cancel(pair.id)
    #expect(store.state(for: pair.id) == .idle)
    let second = Task { await store.preview(pair.id) }
    #expect(await eventually {
        store.state(for: pair.id) == .working(.preview, cancelling: false)
    })

    // The stale poll reply must not overwrite the new job's state, and
    // a poller must keep asking until the job really finishes.
    await client.reply(.previewed(stale))
    let keptPolling = await client.waitForStatusCalls(2, timeout: .seconds(3))
    #expect(store.state(for: pair.id) == .working(.preview, cancelling: false))
    #expect(keptPolling)

    // Drain every outstanding request so buggy and fixed paths finish.
    while await client.pendingStatusCalls > 0 {
        await client.reply(.previewed(done))
    }
    await first.value
    await second.value
    #expect(store.state(for: pair.id) == .previewed(done))
}

// Regression for PR57 follow-up: navigating back to a pair refreshes
// its status through perform, which cleared the declined marker, so
// the same agent result resurrected the approval dialog. Refreshes and
// live polls must keep the decline; only a new explicit preview/run
// (or a changed approval result) re-arms it.
@MainActor
@Test
func declinedApprovalsSurviveStatusRefreshesAndPolls() async {
    let summary = preview(archivesInA: 60, needsApproval: true)
    let expanded = preview(archivesInA: 61, needsApproval: true)
    let client = StubSyncClient()
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()
    let viewModel = PairSyncViewModel(pair: pair, store: store, agentConnected: true)

    await client.setStatusReply(.needsApproval(summary))
    await store.checkAll([pair])
    #expect(store.state(for: pair.id) == .needsApproval(summary))

    store.declineApproval(pair.id)
    #expect(store.state(for: pair.id) == .previewed(summary))
    #expect(viewModel.approvalRequest == nil)

    // The view's automatic refresh must not resurrect the decline.
    await store.refresh(pair.id)
    #expect(store.state(for: pair.id) == .previewed(summary))
    #expect(viewModel.approvalRequest == nil)

    // Neither does a live poll that meets the same declined result.
    await client.setStatusReply(.working(.run, cancelling: false))
    let refreshing = Task { await store.refresh(pair.id) }
    #expect(await eventually {
        store.state(for: pair.id) == .working(.run, cancelling: false)
    })
    await client.setStatusReply(.needsApproval(summary))
    await refreshing.value
    #expect(store.state(for: pair.id) == .previewed(summary))
    #expect(viewModel.approvalRequest == nil)

    // A changed approval result still resurfaces.
    await client.setStatusReply(.needsApproval(expanded))
    await store.refresh(pair.id)
    #expect(store.state(for: pair.id) == .needsApproval(expanded))
    #expect(viewModel.approvalRequest != nil)
}

// Regression for PR57 follow-up: when the caller Task is cancelled
// while its poller has a status request in flight, a new job must
// still get its own poller. The cancelled owner's late reply must not
// overwrite the new job or strip the successor's ownership.
@MainActor
@Test
func cancelledPollersYieldOwnershipToNewerJobs() async {
    let stale = preview(archivesInA: 3, needsApproval: false)
    let done = preview(archivesInA: 0, needsApproval: false)
    let client = DeferredSyncClient()
    await client.setPreviewReply(.working(.preview, cancelling: false))
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()

    let first = Task { await store.preview(pair.id) }
    await client.waitForStatusCalls(1)
    first.cancel()

    // The new job's caller becomes the successor poller even though the
    // cancelled one is still suspended on its status request.
    let second = Task { await store.preview(pair.id) }
    #expect(await eventually {
        store.state(for: pair.id) == .working(.preview, cancelling: false)
    })

    // The stale reply belongs to the cancelled owner: drop it without
    // clearing the successor's ownership.
    await client.reply(.previewed(stale))
    let keptPolling = await client.waitForStatusCalls(2, timeout: .seconds(3))
    #expect(store.state(for: pair.id) == .working(.preview, cancelling: false))
    #expect(keptPolling)

    // Drain every outstanding request so buggy and fixed paths finish.
    while await client.pendingStatusCalls > 0 {
        await client.reply(.previewed(done))
    }
    await first.value
    await second.value
    #expect(store.state(for: pair.id) == .previewed(done))
}

// Regression for PR57 follow-up: the poller swallowed Task.sleep's
// CancellationError, so a caller Task that was cancelled kept issuing
// status requests with no delay while the job stayed .working. The
// caller's cancellation must stop polling without cancelling the job.
@MainActor
@Test
func cancellingTheCallerStopsPollingWithoutCancellingTheJob() async {
    let done = preview(archivesInA: 0, needsApproval: false)
    let client = DeferredSyncClient()
    await client.setPreviewReply(.working(.preview, cancelling: false))
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()

    let job = Task { await store.preview(pair.id) }
    await client.waitForStatusCalls(1)
    job.cancel()

    // The in-flight reply still applies; then polling must stop instead
    // of firing the next status request from a cancelled task.
    await client.reply(.working(.preview, cancelling: false))
    let polledAgain = await client.waitForStatusCalls(2, timeout: .seconds(3))
    #expect(!polledAgain)
    #expect(await client.cancelled == false)
    #expect(store.state(for: pair.id) == .working(.preview, cancelling: false))

    // A terminal reply ends any poller the buggy path left running.
    while await client.pendingStatusCalls > 0 {
        await client.reply(.previewed(done))
    }
    await job.value
}

// Regression for PR57 follow-up: perform writes the request's reply
// unconditionally. A view-driven refresh parked inside its status
// request can resolve after a manual job started, overwrite the new
// .working with .idle, and — through the generation bump — retire the
// successor's live poller. The stale reply must be dropped.
@MainActor
@Test
func staleRefreshRepliesCannotOverwriteManuallyStartedJobs() async {
    let done = preview(archivesInA: 0, needsApproval: false)
    let client = DeferredSyncClient()
    await client.setPreviewReply(.working(.preview, cancelling: false))
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()

    // The automatic refresh parks inside its status request.
    let refreshing = Task { await store.refresh(pair.id) }
    await client.waitForStatusCalls(1)

    // A manual preview starts and takes over live polling.
    let manual = Task { await store.preview(pair.id) }
    #expect(await eventually {
        store.state(for: pair.id) == .working(.preview, cancelling: false)
    })

    // The refresh reply predates the job: it must not win, and a poller
    // must keep asking until the job really finishes.
    await client.reply(.idle)
    await refreshing.value
    let keptPolling = await client.waitForStatusCalls(2, timeout: .seconds(3))
    #expect(store.state(for: pair.id) == .working(.preview, cancelling: false))
    #expect(keptPolling)

    // Drain every outstanding request so buggy and fixed paths finish.
    while await client.pendingStatusCalls > 0 {
        await client.reply(.previewed(done))
    }
    await manual.value
    #expect(store.state(for: pair.id) == .previewed(done))
}

// Regression for PR57 follow-up: the same stale-perform path writes
// requestErrors unconditionally. A parked refresh that fails after a
// newer job started must not report its error on the healthy successor.
@MainActor
@Test
func staleRefreshErrorsCannotLandOnANewerJob() async {
    let done = preview(archivesInA: 0, needsApproval: false)
    let client = DeferredSyncClient()
    await client.setPreviewReply(.working(.preview, cancelling: false))
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()

    let refreshing = Task { await store.refresh(pair.id) }
    await client.waitForStatusCalls(1)

    let manual = Task { await store.preview(pair.id) }
    #expect(await eventually {
        store.state(for: pair.id) == .working(.preview, cancelling: false)
    })

    // The parked refresh fails after the job started: neither the error
    // nor any stale state may touch the successor.
    await client.replyError(SyncFixtureError.unreachable)
    await refreshing.value
    #expect(store.requestError(for: pair.id) == nil)
    #expect(store.state(for: pair.id) == .working(.preview, cancelling: false))

    while await client.pendingStatusCalls > 0 {
        await client.reply(.previewed(done))
    }
    await manual.value
    #expect(store.state(for: pair.id) == .previewed(done))
}

// Regression for PR57 follow-up: a caller task cancelled while its
// request is in flight must not publish the CancellationError as a
// request error the user sees on the pair.
@MainActor
@Test
func cancelledRefreshesDoNotReportCancellationAsAnError() async {
    let client = DeferredSyncClient()
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()

    let refreshing = Task { await store.refresh(pair.id) }
    await client.waitForStatusCalls(1)
    refreshing.cancel()

    await client.replyError(CancellationError())
    await refreshing.value
    #expect(store.requestError(for: pair.id) == nil)
    #expect(store.state(for: pair.id) == .idle)
}

// Regression for PR57 follow-up: while a start RPC is parked the pair
// still reads .idle, so canStart stays true and a second tap or an
// overlapping caller issues a duplicate job. A pending start must look
// busy and a concurrent start must not reach the agent — while the
// accepted first start still lands its reply.
@MainActor
@Test
func pendingStartsAreBusyAndRejectDuplicateStarts() async {
    let done = preview(archivesInA: 0, needsApproval: false)
    let client = DeferredSyncClient()
    await client.holdStartReplies()
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()
    let viewModel = PairSyncViewModel(pair: pair, store: store, agentConnected: true)

    let first = Task { await store.preview(pair.id) }
    #expect(await client.waitForStartCalls(.preview, 1, timeout: .seconds(3)))
    #expect(!viewModel.canStart)

    // A concurrent start must not issue a second preview RPC.
    let duplicate = Task { await store.preview(pair.id) }
    let secondIssued = await client.waitForStartCalls(
        .preview, 2, timeout: .milliseconds(500))
    #expect(!secondIssued)

    // The accepted start still applies its reply.
    while await client.pendingStartCalls > 0 {
        await client.replyStart(.previewed(done))
    }
    await first.value
    await duplicate.value
    #expect(store.state(for: pair.id) == .previewed(done))

    // The same holds while a run RPC is parked.
    let runJob = Task { await store.run(pair.id) }
    #expect(await client.waitForStartCalls(.run, 1, timeout: .seconds(3)))
    #expect(!viewModel.canStart)
    let duplicateRun = Task { await store.run(pair.id) }
    let secondRunIssued = await client.waitForStartCalls(
        .run, 2, timeout: .milliseconds(500))
    #expect(!secondRunIssued)
    while await client.pendingStartCalls > 0 {
        await client.replyStart(.idle)
    }
    await runJob.value
    await duplicateRun.value
    #expect(store.state(for: pair.id) == .idle)
}

// Regression for PR57 follow-up: preview/run clear the declined marker
// before the request is known to succeed, so a failed explicit attempt
// spends the decline and the next automatic check resurrects the same
// approval. A failed start must preserve the decline.
@MainActor
@Test
func declinedApprovalsSurviveFailedJobAttempts() async {
    let summary = preview(archivesInA: 60, needsApproval: true)
    let client = StubSyncClient()
    let recorder = AlertRecorder()
    let store = SyncStore(client: client, pollInterval: .milliseconds(1)) { alert in
        await recorder.record(alert)
    }
    let pair = pair()
    let viewModel = PairSyncViewModel(pair: pair, store: store, agentConnected: true)

    await client.setStatusReply(.needsApproval(summary))
    await store.checkAll([pair])
    store.declineApproval(pair.id)
    #expect(store.state(for: pair.id) == .previewed(summary))

    // A failed explicit attempt reports its error but must not spend
    // the decline.
    await client.setPreviewError(SyncFixtureError.unreachable)
    await store.preview(pair.id)
    #expect(store.requestError(for: pair.id) != nil)
    #expect(store.state(for: pair.id) == .previewed(summary))

    // The next scheduled check sees the same result: still declined.
    await store.checkAll([pair])
    #expect(store.state(for: pair.id) == .previewed(summary))
    #expect(viewModel.approvalRequest == nil)
    #expect(await recorder.alerts.isEmpty)
}

// Regression for PR57 follow-up: while a start is pending, refresh and
// checkAll must not issue a status request — the start's own reply is
// fresher — and a reply requested before the start stays stale even when
// it lands inside the pending window. A freshness gate that counts
// writes but forgets the start's registration would let such a reply
// retire the start's answer.
@MainActor
@Test
func startsOwnThePairWhileTheirRequestIsInFlight() async {
    let done = preview(archivesInA: 0, needsApproval: false)
    let client = DeferredSyncClient()
    await client.holdStartReplies()
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()

    // A refresh parks inside its status request before the start.
    let refreshing = Task { await store.refresh(pair.id) }
    await client.waitForStatusCalls(1)

    // The start parks too; while it is unanswered no status read may run.
    let starting = Task { await store.preview(pair.id) }
    #expect(await client.waitForStartCalls(.preview, 1, timeout: .seconds(3)))
    await store.refresh(pair.id)
    await store.checkAll([pair])
    #expect(await client.statusCalls == 1)

    // The refresh's reply lands inside the pending window: requested
    // before the start, it must not publish or retire the start's reply.
    await client.reply(.idle)
    await refreshing.value
    await client.replyStart(.working(.preview, cancelling: false))
    #expect(await eventually {
        store.state(for: pair.id) == .working(.preview, cancelling: false)
    })

    // The accepted job polls to its real terminal result.
    #expect(await client.waitForStatusCalls(2, timeout: .seconds(3)))
    while await client.pendingStatusCalls > 0 {
        await client.reply(.previewed(done))
    }
    await starting.value
    #expect(store.state(for: pair.id) == .previewed(done))
}
