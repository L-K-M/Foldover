import Foundation
import Testing
@testable import Foldover

private enum SyncFixtureError: Error { case unreachable, timedOut }

/// Canonical 64-hex review tokens the test fixtures can distinguish.
private let reviewTokenA = String(repeating: "a", count: 64)
private let reviewTokenB = String(repeating: "b", count: 64)

/// Replies with scripted states: the start reply, then one per status poll.
private actor ScriptedSyncClient: SyncClient {
    private var replies: [PairSyncState]
    /// (approvedA, approvedB, reviewToken) per run call.
    private(set) var approvals: [(UInt64, UInt64, String?)] = []
    private(set) var cancelled = false
    private(set) var undone = 0
    private(set) var kept: [FolderSide] = []
    private(set) var keptTokens: [String] = []
    private(set) var keptBoth: [Data] = []
    private(set) var keptBothTokens: [String] = []
    private(set) var skipped: [Data] = []
    private(set) var skipTokens: [String] = []
    private(set) var changeTokens: [String] = []
    private let runs: [RunRecord]

    init(_ replies: [PairSyncState], history: [RunRecord] = []) {
        self.replies = replies
        runs = history
    }

    func history(_ id: String) async throws -> [RunRecord] { runs }

    /// Two pages of copies after the status's first page.
    func changes(_ id: String, after: Data?, filter: ChangeFilter?,
                 reviewToken: String) async throws -> ChangePage {
        changeTokens.append(reviewToken)
        let all = (0..<4).map { PlannedChange(path: Data("p\($0)".utf8), action: .copy(to: .b)) }
        let rest = all.filter { change in after.map { $0.lexicographicallyPrecedes(change.path) } ?? true }
        let kept = filter == .archives ? [] : rest
        return ChangePage(changes: Array(kept.prefix(2)), more: kept.count > 2)
    }

    func keepBoth(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState {
        keptBoth.append(path)
        keptBothTokens.append(reviewToken)
        return next()
    }

    func skipOnce(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState {
        skipped.append(path)
        skipTokens.append(reviewToken)
        return next()
    }

    func preview(_ id: String) async throws -> PairSyncState { next() }
    func status(_ id: String) async throws -> PairSyncState { next() }

    func run(_ id: String, approvedA: UInt64, approvedB: UInt64,
             reviewToken: String?) async throws -> PairSyncState {
        approvals.append((approvedA, approvedB, reviewToken))
        return next()
    }

    func cancel(_ id: String) async throws -> PairSyncState {
        cancelled = true
        return next()
    }

    func undo(_ id: String) async throws -> PairSyncState {
        undone += 1
        return next()
    }

    func resolve(_ id: String, path: Data, keep: FolderSide,
                 reviewToken: String) async throws -> PairSyncState {
        kept.append(keep)
        keptTokens.append(reviewToken)
        return next()
    }

    private func next() -> PairSyncState {
        replies.isEmpty ? .idle : replies.removeFirst()
    }
}

private struct UnreachableSyncClient: SyncClient {
    func preview(_ id: String) async throws -> PairSyncState { throw SyncFixtureError.unreachable }
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64,
             reviewToken: String?) async throws -> PairSyncState {
        throw SyncFixtureError.unreachable
    }
    func status(_ id: String) async throws -> PairSyncState { throw SyncFixtureError.unreachable }
    func cancel(_ id: String) async throws -> PairSyncState { throw SyncFixtureError.unreachable }
    func undo(_ id: String) async throws -> PairSyncState { throw SyncFixtureError.unreachable }
    func resolve(_ id: String, path: Data, keep: FolderSide,
                 reviewToken: String) async throws -> PairSyncState {
        throw SyncFixtureError.unreachable
    }
    func history(_ id: String) async throws -> [RunRecord] { throw SyncFixtureError.unreachable }
    func changes(_ id: String, after: Data?, filter: ChangeFilter?,
                 reviewToken: String) async throws -> ChangePage {
        throw SyncFixtureError.unreachable
    }
    func keepBoth(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState {
        throw SyncFixtureError.unreachable
    }
    func skipOnce(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState {
        throw SyncFixtureError.unreachable
    }
}

/// Holds every changes() call until the test answers it, in call order.
private actor GatedChangesClient: SyncClient {
    private let reply: PairSyncState
    private(set) var filters: [ChangeFilter?] = []
    private var pending: [CheckedContinuation<ChangePage, Error>?] = []

    init(reply: PairSyncState) { self.reply = reply }

    func preview(_ id: String) async throws -> PairSyncState { reply }
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64,
             reviewToken: String?) async throws -> PairSyncState { reply }
    func status(_ id: String) async throws -> PairSyncState { reply }
    func cancel(_ id: String) async throws -> PairSyncState { .idle }
    func undo(_ id: String) async throws -> PairSyncState { .idle }
    func resolve(_ id: String, path: Data, keep: FolderSide,
                 reviewToken: String) async throws -> PairSyncState { .idle }
    func keepBoth(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState { .idle }
    func skipOnce(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState { .idle }
    func history(_ id: String) async throws -> [RunRecord] { [] }

    func changes(_ id: String, after: Data?, filter: ChangeFilter?,
                 reviewToken: String) async throws -> ChangePage {
        filters.append(filter)
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation {
                pending.append($0)
                if Task.isCancelled { fail(pending.count - 1, with: CancellationError()) }
            }
        } onCancel: {
            Task { await self.releasePending() }
        }
    }

    /// Bounded: a call that never arrives fails the test instead of
    /// hanging the suite.
    func waitForCalls(_ count: Int) async throws {
        var attempts = 0
        while filters.count < count, attempts < 10_000 {
            attempts += 1
            try? await Task.sleep(for: .milliseconds(1))
        }
        if filters.count < count { throw SyncFixtureError.timedOut }
    }

    func answer(_ index: Int, with page: ChangePage) {
        let continuation = pending[index]
        pending[index] = nil
        continuation?.resume(returning: page)
    }

    func fail(_ index: Int, with error: Error) {
        let continuation = pending[index]
        pending[index] = nil
        continuation?.resume(throwing: error)
    }

    /// Cancelling a task does not resume its checked continuation; free
    /// any calls a failing test leaves behind so the tasks can finish.
    private func releasePending() {
        for index in pending.indices { fail(index, with: CancellationError()) }
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
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64,
             reviewToken: String?) async throws -> PairSyncState { .idle }
    func cancel(_ id: String) async throws -> PairSyncState { .idle }
    func undo(_ id: String) async throws -> PairSyncState { .idle }
    func resolve(_ id: String, path: Data, keep: FolderSide,
                 reviewToken: String) async throws -> PairSyncState { .idle }
    func keepBoth(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState { .idle }
    func skipOnce(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState { .idle }
    func history(_ id: String) async throws -> [RunRecord] { [] }
    func changes(_ id: String, after: Data?, filter: ChangeFilter?,
                 reviewToken: String) async throws -> ChangePage {
        ChangePage(changes: [], more: false)
    }
}

/// Holds every status reply until the test releases it, so a checkAll
/// request can stay in flight while a manual job starts.
private actor DeferredSyncClient: SyncClient {
    private(set) var statusCalls = 0
    private(set) var previewCalls = 0
    private(set) var runCalls = 0
    private(set) var resolveCalls = 0
    private(set) var historyCalls = 0
    private(set) var cancelled = false
    private(set) var runTokens: [String?] = []
    private(set) var resolved: [(path: Data, keep: FolderSide, token: String)] = []
    private var pending: [CheckedContinuation<PairSyncState, any Error>] = []
    private var waiters: [(id: UUID, count: Int, then: CheckedContinuation<Bool, Never>)] = []
    private var pendingStarts: [CheckedContinuation<PairSyncState, any Error>] = []
    private var pendingResolves: [CheckedContinuation<PairSyncState, any Error>] = []
    private var pendingHistory: [CheckedContinuation<[RunRecord], any Error>] = []
    private var startWaiters: [(id: UUID, job: SyncJob, count: Int,
                               then: CheckedContinuation<Bool, Never>)] = []
    private var holdStarts = false
    private var holdResolves = false
    private var holdHistory = false
    private var previewReply: PairSyncState = .idle
    private var terminalStatus: PairSyncState?
    private var terminalHistory: [RunRecord]?

    func setPreviewReply(_ reply: PairSyncState) { previewReply = reply }
    /// Park preview/run requests until the test releases them.
    func holdStartReplies() { holdStarts = true }
    /// Park resolve requests until the test releases them.
    func holdResolveReplies() { holdResolves = true }
    /// Park history requests until the test releases them.
    func holdHistoryReplies() { holdHistory = true }

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
        if let terminalStatus { return terminalStatus }
        return try await withCheckedThrowingContinuation { pending.append($0) }
    }
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64,
             reviewToken: String?) async throws -> PairSyncState {
        started(.run)
        runTokens.append(reviewToken)
        guard holdStarts else { return .idle }
        return try await withCheckedThrowingContinuation { pendingStarts.append($0) }
    }
    func cancel(_ id: String) async throws -> PairSyncState {
        cancelled = true
        return .idle
    }

    func undo(_ id: String) async throws -> PairSyncState { .idle }
    func resolve(_ id: String, path: Data, keep: FolderSide,
                 reviewToken: String) async throws -> PairSyncState {
        resolveCalls += 1
        resolved.append((path: path, keep: keep, token: reviewToken))
        guard holdResolves else { return .idle }
        return try await withCheckedThrowingContinuation { pendingResolves.append($0) }
    }
    func keepBoth(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState { .idle }
    func skipOnce(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState { .idle }
    func history(_ id: String) async throws -> [RunRecord] {
        historyCalls += 1
        if let terminalHistory { return terminalHistory }
        guard holdHistory else { return [] }
        return try await withCheckedThrowingContinuation { pendingHistory.append($0) }
    }
    func changes(_ id: String, after: Data?, filter: ChangeFilter?,
                 reviewToken: String) async throws -> ChangePage {
        ChangePage(changes: [], more: false)
    }

    private func started(_ job: SyncJob) {
        switch job {
        case .preview: previewCalls += 1
        case .run: runCalls += 1
        // This fake answers undo directly; nothing waits on it.
        case .undo: return
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

    /// Cleanup must answer polls issued after the current queue drains too.
    func finishPolling(_ state: PairSyncState) {
        terminalStatus = state
        while !pending.isEmpty { reply(state) }
    }

    /// Fails the oldest unanswered status request.
    func replyError(_ error: any Error) {
        pending.removeFirst().resume(throwing: error)
    }

    /// Resumes the oldest parked preview/run request.
    func replyStart(_ state: PairSyncState) {
        pendingStarts.removeFirst().resume(returning: state)
    }

    /// Fails the oldest parked preview/run request.
    func replyStartError(_ error: any Error) {
        pendingStarts.removeFirst().resume(throwing: error)
    }

    /// Lets starts answer at once again and releases any still parked.
    func resumeStarts(replying reply: PairSyncState) {
        holdStarts = false
        previewReply = reply
        while !pendingStarts.isEmpty { replyStart(reply) }
    }

    /// Resumes the oldest parked resolve request.
    func replyResolve(_ state: PairSyncState) {
        pendingResolves.removeFirst().resume(returning: state)
    }

    /// Resumes the oldest parked history request.
    func replyHistory(_ runs: [RunRecord] = []) {
        pendingHistory.removeFirst().resume(returning: runs)
    }

    /// Cleanup must answer history issued after the queue drains too.
    func finishHistory(_ runs: [RunRecord] = []) {
        holdHistory = false
        terminalHistory = runs
        while !pendingHistory.isEmpty { replyHistory(runs) }
    }

    var pendingStatusCalls: Int { pending.count }
    var pendingStartCalls: Int { pendingStarts.count }
    var pendingResolveCalls: Int { pendingResolves.count }
    var pendingHistoryCalls: Int { pendingHistory.count }

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

    /// Bounded wait on history requests, so a call a bug suppresses
    /// fails instead of hanging the test.
    @discardableResult
    func waitForHistoryCalls(_ count: Int, timeout: Duration) async -> Bool {
        let deadline = ContinuousClock.now + timeout
        while historyCalls < count {
            if ContinuousClock.now >= deadline { return false }
            try? await Task.sleep(for: .milliseconds(1))
        }
        return true
    }

    /// Bounded wait on resolve requests, so a call a bug suppresses
    /// fails instead of hanging the test.
    @discardableResult
    func waitForResolveCalls(_ count: Int, timeout: Duration) async -> Bool {
        let deadline = ContinuousClock.now + timeout
        while resolveCalls < count {
            if ContinuousClock.now >= deadline { return false }
            try? await Task.sleep(for: .milliseconds(1))
        }
        return true
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

private func preview(archivesInA: UInt64, needsApproval: Bool,
                     reviewToken: String = reviewTokenA,
                     pairRevision: UInt64 = 1) -> PreviewSummary {
    PreviewSummary(
        changes: [PlannedChange(path: Data("photos/a.jpg".utf8), action: .copy(to: .b), bytes: 2_000_000),
                  PlannedChange(path: Data([0x6f, 0xff]), action: .archive(on: .a))],
        copies: 1, updates: 0, archives: archivesInA, conflicts: 0, blocked: 0, kept: 0,
        a: DeletionCheck(deletions: archivesInA, paths: 100, needsApproval: needsApproval),
        b: DeletionCheck(deletions: 0, paths: 100, needsApproval: false), copyBytes: 2_000_000,
        reviewToken: reviewToken, pairRevision: pairRevision
    )
}

/// Holds the test pair and records saved settings and the revisions
/// their updates were conditioned on.
private actor CatalogClient: PairClient {
    private(set) var saved: [PairSettings] = []
    private(set) var savedRevisions: [UInt64] = []

    func page(after: String?) async throws -> PairCatalogPage {
        PairCatalogPage(pairs: after == nil ? [pair()] : [], next: nil)
    }
    func create(_ settings: PairSettings) async throws -> SyncPair { pair() }
    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair {
        saved.append(settings)
        savedRevisions.append(pair.revision)
        return SyncPair(id: pair.id, revision: pair.revision + 1, settings: settings)
    }
    func remove(_ pair: SyncPair) async throws -> SyncPair { pair }
    func check(_ settings: PairSettings) async throws -> [PairReadinessWarning] { [] }
    func servers() async throws -> [NearbyServer] { [] }
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
    let request = try #require(viewModel.approvalRequest)
    #expect(request.message.contains("60"))
    #expect(request.pairID == pair().id)
    #expect(request.pairRevision == summary.pairRevision)
    #expect(request.reviewToken == summary.reviewToken)
    // Dismissing the dialog declines; the captured request still runs
    // and presents the token of the preview it was taken from.
    viewModel.declineApproval()
    await viewModel.approve(request)
    let approvals = await client.approvals
    #expect(approvals.map(\.0) == [0, 60])
    #expect(approvals.map(\.1) == [0, 0])
    #expect(approvals.map(\.2) == [nil, summary.reviewToken])
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
        b: DeletionCheck(deletions: 0, paths: 2, needsApproval: false),
        reviewToken: reviewTokenA, pairRevision: 1
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
    // The choice echoes the fingerprint of the preview it was made from.
    #expect(await client.keptTokens == [conflicted.reviewToken])
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
        b: DeletionCheck(deletions: 0, paths: 2, needsApproval: false),
        reviewToken: reviewTokenA, pairRevision: 1
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
    #expect(await client.keptBothTokens == [conflicted.reviewToken])
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
                  updated: 0, archived: 1, recovered: 0, conflicts: 1, failures: 0, message: nil),
        RunRecord(started: started, finished: started, trigger: .manual, end: .failed, copied: 0,
                  updated: 0, archived: 0, recovered: 0, conflicts: 0, failures: 0, message: "Disk full"),
        RunRecord(started: started, finished: started, trigger: .manual, end: .completed, copied: 0,
                  updated: 0, archived: 0, recovered: 0, conflicts: 0, failures: 0, message: nil),
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
    // The filter edit is conditioned on the revision the preview was
    // made for, so a pair that changed since rejects it.
    #expect(await pairs.savedRevisions == [summary.pairRevision])
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
    // Every page echoes the token of the preview it lists.
    #expect(await client.changeTokens == [summary.reviewToken, summary.reviewToken, summary.reviewToken])

    await viewModel.refresh()
    #expect(store.listing(for: pair().id) == nil)
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
    let viewModel = PairSyncViewModel(pair: pair, store: store, pairs: catalog(), agentConnected: true)

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
    await client.finishPolling(.previewed(summary))
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
    await client.finishPolling(.previewed(done))
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
    let viewModel = PairSyncViewModel(pair: pair, store: store, pairs: catalog(), agentConnected: true)

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
    await client.finishPolling(.previewed(done))
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
    await client.finishPolling(.previewed(done))
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
    await client.finishPolling(.previewed(done))
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

    await client.finishPolling(.previewed(done))
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
    let viewModel = PairSyncViewModel(pair: pair, store: store, pairs: catalog(), agentConnected: true)

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
    let viewModel = PairSyncViewModel(pair: pair, store: store, pairs: catalog(), agentConnected: true)

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
    await client.finishPolling(.previewed(done))
    await starting.value
    #expect(store.state(for: pair.id) == .previewed(done))
}

// Regression for PR57 follow-up: while an approving run's start RPC is
// unanswered the pair still reads .needsApproval, so approvalRequest
// stays non-nil and the dialog the user just confirmed re-presents. A
// pending start must already hide the dialog.
@MainActor
@Test
func pendingApprovedRunsHideTheApprovalDialog() async {
    let summary = preview(archivesInA: 60, needsApproval: true)
    let client = DeferredSyncClient()
    await client.setPreviewReply(.needsApproval(summary))
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()
    let viewModel = PairSyncViewModel(pair: pair, store: store, pairs: catalog(), agentConnected: true)

    await store.preview(pair.id)
    #expect(viewModel.approvalRequest != nil)

    await client.holdStartReplies()
    let approving = Task { await store.run(pair.id) }
    #expect(await client.waitForStartCalls(.run, 1, timeout: .seconds(3)))
    // An unreviewed run presents no token; approval flows echo one.
    #expect(await client.runTokens == [nil])
    #expect(viewModel.approvalRequest == nil)

    await client.replyStart(.idle)
    await approving.value
}

@MainActor
@Test
func theLastFilterTapWinsOverStaleResponses() async throws {
    let summary = preview(archivesInA: 3, needsApproval: false)
    let client = GatedChangesClient(reply: .previewed(summary))
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    await viewModel.preview()

    // Two rapid taps: the first request is still in flight when the
    // second is sent.
    async let stale = viewModel.showChanges(.copies)
    try await client.waitForCalls(1)
    async let latest = viewModel.showChanges(.archives)
    try await client.waitForCalls(2)
    #expect(await client.filters == [.copies, .archives])

    // The newest tap answers first and is fully applied; only then does
    // the stale response arrive.
    let archived = ChangePage(changes: [
        PlannedChange(path: Data("old.zip".utf8), action: .archive(on: .a)),
    ], more: false)
    let copied = ChangePage(changes: [
        PlannedChange(path: Data("new.jpg".utf8), action: .copy(to: .b)),
    ], more: false)
    await client.answer(1, with: archived)
    await latest
    await client.answer(0, with: copied)
    await stale

    // Last tap wins: the earlier response must not move the selection.
    #expect(viewModel.changeFilter == .archives)
    #expect(viewModel.rows.map(\.path) == ["old.zip"])
    #expect(viewModel.requestError == nil)
}

@MainActor
@Test
func aSupersededChangesRequestReportsNoError() async throws {
    let summary = preview(archivesInA: 3, needsApproval: false)
    let client = GatedChangesClient(reply: .previewed(summary))
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    await viewModel.preview()

    async let stale = viewModel.showChanges(.copies)
    try await client.waitForCalls(1)
    async let latest = viewModel.showChanges(.archives)
    try await client.waitForCalls(2)

    // The newest tap lands; the superseded request fails behind it.
    let archived = ChangePage(changes: [
        PlannedChange(path: Data("old.zip".utf8), action: .archive(on: .a)),
    ], more: false)
    await client.answer(1, with: archived)
    await latest
    await client.fail(0, with: SyncFixtureError.unreachable)
    await stale

    // A superseded request's failure belongs to a list nobody asked for
    // anymore: the newest filter and rows stay and no error is reported.
    #expect(viewModel.changeFilter == .archives)
    #expect(viewModel.rows.map(\.path) == ["old.zip"])
    #expect(viewModel.requestError == nil)
}

@MainActor
@Test
func overwriteOnlyApprovalsExplainThePendingCopy() async {
    let check = DeletionCheck(deletions: 0, paths: 1, needsApproval: false)
    let summary = PreviewSummary(
        changes: [PlannedChange(path: Data("note".utf8), action: .copy(to: .b))],
        copies: 1, updates: 0, archives: 0, conflicts: 0, blocked: 0, kept: 0,
        a: check, b: check, reviewToken: reviewTokenA, pairRevision: 1
    )
    let store = SyncStore(client: ScriptedSyncClient([.needsApproval(summary)]))
    let pair = pair()
    let viewModel = PairSyncViewModel(pair: pair, store: store, pairs: catalog(), agentConnected: true)
    await store.preview(pair.id)

    // First-run overwrites need review even with no standalone deletions.
    let request = viewModel.approvalRequest?.message.lowercased()
    #expect(request?.contains("copy") == true)
    #expect(request?.contains("1") == true)
    #expect(!viewModel.statusText.contains("more items"))
}

// Regression for PR57 follow-up: dismissing the dialog while an
// approving run is in flight writes .previewed and bumps the generation,
// so the accepted .working reply is dropped and no poller ever runs —
// the pair shows a stale preview while the agent may be running the
// sync. A dismissal during a pending start must not abandon the
// accepted reply or its poller.
@MainActor
@Test
func dismissingDuringAPendingStartCannotAbandonTheAcceptedRun() async {
    let summary = preview(archivesInA: 60, needsApproval: true)
    let finished = RunSummary(cancelled: false, copied: 1, updated: 0, archived: 60,
                              recovered: 0, conflicts: 0, failures: [], failureCount: 0,
                              archive: "run")
    let client = DeferredSyncClient()
    await client.setPreviewReply(.needsApproval(summary))
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()
    let viewModel = PairSyncViewModel(pair: pair, store: store, pairs: catalog(), agentConnected: true)

    await store.preview(pair.id)
    await client.holdStartReplies()
    let approving = Task { await store.run(pair.id) }
    #expect(await client.waitForStartCalls(.run, 1, timeout: .seconds(3)))
    #expect(viewModel.approvalRequest == nil)

    // A dismissal while the start is in flight must not retire it.
    store.declineApproval(pair.id)
    #expect(viewModel.approvalRequest == nil)

    await client.replyStart(.working(.run, cancelling: false))
    #expect(await eventually {
        store.state(for: pair.id) == .working(.run, cancelling: false)
    })
    #expect(await client.waitForStatusCalls(1, timeout: .seconds(3)))

    await client.finishPolling(.finished(finished))
    await approving.value
    #expect(store.state(for: pair.id) == .finished(finished))
}

// Regression for PR57 follow-up: the same dismissal also retires the
// start's error path — a run that legitimately fails lands after the
// decline's write, so its error is dropped and the failure is hidden.
@MainActor
@Test
func dismissingDuringAPendingStartCannotHideTheRunsFailure() async {
    let summary = preview(archivesInA: 60, needsApproval: true)
    let client = DeferredSyncClient()
    await client.setPreviewReply(.needsApproval(summary))
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()
    let viewModel = PairSyncViewModel(pair: pair, store: store, pairs: catalog(), agentConnected: true)

    await store.preview(pair.id)
    await client.holdStartReplies()
    let approving = Task { await store.run(pair.id) }
    #expect(await client.waitForStartCalls(.run, 1, timeout: .seconds(3)))

    store.declineApproval(pair.id)
    await client.replyStartError(SyncFixtureError.unreachable)
    await approving.value
    #expect(store.requestError(for: pair.id) != nil)
    // The run never ran: the approval it would have cleared is still live.
    #expect(viewModel.approvalRequest != nil)
}

// Regression for PR57 follow-up: a poller from the previous job can wake
// inside a new start's pending window, land a terminal reply that bumps
// the generation, and retire the new start's accepted reply — leaving no
// poller for the job the agent is actually running.
@MainActor
@Test
func oldPollerRepliesCannotRetireAPendingStart() async {
    let done = preview(archivesInA: 0, needsApproval: false)
    let finished = RunSummary(cancelled: false, copied: 1, updated: 0, archived: 3,
                              recovered: 0, conflicts: 0, failures: [], failureCount: 0,
                              archive: "run")
    let client = DeferredSyncClient()
    await client.setPreviewReply(.working(.preview, cancelling: false))
    let store = SyncStore(client: client, pollInterval: .milliseconds(250))
    let pair = pair()

    // Job one's poller is in its first sleep when the new start begins.
    let first = Task { await store.preview(pair.id) }
    #expect(await eventually {
        store.state(for: pair.id) == .working(.preview, cancelling: false)
    })

    await client.holdStartReplies()
    let second = Task { await store.preview(pair.id) }
    #expect(await client.waitForStartCalls(.preview, 2, timeout: .seconds(3)))

    // Give the old poller its poll window inside the pending start, then
    // answer every request it managed to issue with a terminal reply.
    _ = await client.waitForStatusCalls(1, timeout: .seconds(1))
    let issuedBeforeReply = await client.statusCalls
    while await client.pendingStatusCalls > 0 {
        await client.reply(.finished(finished))
    }

    // The accepted start's reply still wins and owns the live poller.
    await client.replyStart(.working(.preview, cancelling: false))
    #expect(await eventually {
        store.state(for: pair.id) == .working(.preview, cancelling: false)
    })
    #expect(await client.waitForStatusCalls(issuedBeforeReply + 1, timeout: .seconds(3)))

    await client.finishPolling(.previewed(done))
    await first.value
    await second.value
    #expect(store.state(for: pair.id) == .previewed(done))
}

// Regression for PR57 follow-up, reshaped for the review-token
// contract: a conflict choice can only be made from a preview the app
// shows, and while it is pending the pair is owned — reads skip it and
// new commands are refused, so nothing newer can supersede it. The one
// stale path left is a cancelled caller: its answer must release the
// pair, report nothing, and never issue the follow-up preview.
@MainActor
@Test
func cancelledChoicesReleaseThePairWithoutANewPreview() async {
    let conflicted = PreviewSummary(
        changes: [PlannedChange(path: Data("notes.txt".utf8), action: .conflict(.bothChanged))],
        copies: 0, updates: 0, archives: 0, conflicts: 1, blocked: 0, kept: 0,
        a: DeletionCheck(deletions: 0, paths: 1, needsApproval: false),
        b: DeletionCheck(deletions: 0, paths: 1, needsApproval: false),
        reviewToken: reviewTokenA, pairRevision: 1
    )
    let resolved = preview(archivesInA: 0, needsApproval: false)
    let client = DeferredSyncClient()
    await client.setPreviewReply(.previewed(conflicted))
    await client.holdResolveReplies()
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()
    await store.preview(pair.id)

    let resolving = Task {
        await store.resolve(pair.id, path: Data("notes.txt".utf8), keep: .a,
                            reviewToken: conflicted.reviewToken)
    }
    #expect(await client.waitForResolveCalls(1, timeout: .seconds(3)))

    // A cancelled caller's late answer must not start the re-preview.
    resolving.cancel()
    await client.replyResolve(.idle)
    await resolving.value
    let issued = await client.waitForStartCalls(.preview, 2, timeout: .milliseconds(500))
    #expect(!issued)
    #expect(store.requestError(for: pair.id) == nil)
    #expect(store.state(for: pair.id) == .previewed(conflicted))

    // The released pair accepts the next explicit command normally.
    await client.setPreviewReply(.previewed(resolved))
    await store.preview(pair.id)
    #expect(store.state(for: pair.id) == .previewed(resolved))

    while await client.pendingResolveCalls > 0 {
        await client.replyResolve(.idle)
    }
}

// Regression for PR57 follow-up: a conflict choice in flight owns the
// pair, but nothing marks it. A checkAll that reports the same cached
// preview still counts as a write, so the accepted resolve fails its
// freshness gate and the automatic re-preview never issues; the choice
// is silently abandoned.
@MainActor
@Test
func checkAllRepliesCannotRetireAPendingChoice() async {
    let conflicted = PreviewSummary(
        changes: [PlannedChange(path: Data("notes.txt".utf8), action: .conflict(.bothChanged)),
                  PlannedChange(path: Data("folder".utf8), action: .conflict(.typeMismatch))],
        copies: 0, updates: 0, archives: 0, conflicts: 2, blocked: 0, kept: 0,
        a: DeletionCheck(deletions: 0, paths: 2, needsApproval: false),
        b: DeletionCheck(deletions: 0, paths: 2, needsApproval: false),
        reviewToken: reviewTokenA, pairRevision: 1
    )
    let resolved = preview(archivesInA: 0, needsApproval: false)
    let client = DeferredSyncClient()
    await client.setPreviewReply(.previewed(conflicted))
    await client.holdResolveReplies()
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()
    await store.preview(pair.id)

    let resolving = Task {
        await store.resolve(pair.id, path: Data("notes.txt".utf8), keep: .a,
                            reviewToken: conflicted.reviewToken)
    }
    #expect(await client.waitForResolveCalls(1, timeout: .seconds(3)))

    // The agent reports the same cached preview while the choice is on
    // the wire. A fixed store may skip the read while the pair is owned,
    // so the terminal reply also covers a late issue and any later poll.
    let checking = Task { await store.checkAll([pair]) }
    await client.finishPolling(.previewed(conflicted))
    await checking.value

    // The accepted choice must still trigger its fresh preview.
    await client.setPreviewReply(.previewed(resolved))
    await client.replyResolve(.idle)
    #expect(await client.waitForStartCalls(.preview, 2, timeout: .seconds(3)))
    #expect(await eventually { store.state(for: pair.id) == .previewed(resolved) })

    await resolving.value
}

// Regression for PR57 follow-up: while a conflict choice is on the wire
// the pair is owned, so the UI must refuse new jobs. A start that slips
// through, even one the agent rejects as busy, reaches the client and
// bumps the generation out from under the accepted choice's re-preview.
@MainActor
@Test
func newStartsCannotRetireAPendingChoice() async {
    let conflicted = PreviewSummary(
        changes: [PlannedChange(path: Data("notes.txt".utf8), action: .conflict(.bothChanged)),
                  PlannedChange(path: Data("folder".utf8), action: .conflict(.typeMismatch))],
        copies: 0, updates: 0, archives: 0, conflicts: 2, blocked: 0, kept: 0,
        a: DeletionCheck(deletions: 0, paths: 2, needsApproval: false),
        b: DeletionCheck(deletions: 0, paths: 2, needsApproval: false),
        reviewToken: reviewTokenA, pairRevision: 1
    )
    let resolved = preview(archivesInA: 0, needsApproval: false)
    let client = DeferredSyncClient()
    await client.setPreviewReply(.previewed(conflicted))
    await client.holdResolveReplies()
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()
    await store.preview(pair.id)

    let resolving = Task {
        await store.resolve(pair.id, path: Data("notes.txt".utf8), keep: .a,
                            reviewToken: conflicted.reviewToken)
    }
    #expect(await client.waitForResolveCalls(1, timeout: .seconds(3)))
    #expect(await client.resolved.map(\.token) == [conflicted.reviewToken])

    let viewModel = PairSyncViewModel(pair: pair, store: store, pairs: catalog(), agentConnected: true)
    #expect(!viewModel.canStart)
    // A pending choice is busy work, not a job: it gets the honest
    // label and no Cancel it could not honour.
    #expect(viewModel.isResolving)
    #expect(!viewModel.canCancel)
    #expect(viewModel.statusText == "Resolving the conflict…")

    // A user start during the choice must not reach the client; if the
    // bug lets it through, the agent's busy rejection follows.
    await client.holdStartReplies()
    let conflicting = Task { await store.preview(pair.id) }
    let issued = await client.waitForStartCalls(.preview, 2, timeout: .milliseconds(500))
    #expect(!issued)
    if issued { await client.replyStartError(SyncFixtureError.unreachable) }

    // A second choice is a duplicate, not a new command.
    let second = Task {
        await store.resolve(pair.id, path: Data("notes.txt".utf8), keep: .b,
                            reviewToken: conflicted.reviewToken)
    }
    let secondIssued = await client.waitForResolveCalls(2, timeout: .milliseconds(500))
    #expect(!secondIssued)

    // Later starts answer at once so the choice's own re-preview can run.
    await client.resumeStarts(replying: .previewed(resolved))
    await client.replyResolve(.idle)
    #expect(await eventually { store.state(for: pair.id) == .previewed(resolved) })

    await client.finishPolling(.idle)
    while await client.pendingResolveCalls > 0 {
        await client.replyResolve(.idle)
    }
    while await client.pendingStartCalls > 0 {
        await client.replyStart(.idle)
    }
    await conflicting.value
    await second.value
    await resolving.value
    #expect(store.requestError(for: pair.id) == nil)
}

// Regression for PR57 follow-up: a job that polls to a terminal preview
// keeps its poller's owner token through the post-run history refresh.
// While that informational read is slow the pair still counts as owned,
// so the conflict buttons it shows silently refuse and the choice never
// reaches the agent.
@MainActor
@Test
func readyChoicesReachTheClientWhileHistoryIsPending() async {
    let conflicted = PreviewSummary(
        changes: [PlannedChange(path: Data("notes.txt".utf8), action: .conflict(.bothChanged))],
        copies: 0, updates: 0, archives: 0, conflicts: 1, blocked: 0, kept: 0,
        a: DeletionCheck(deletions: 0, paths: 1, needsApproval: false),
        b: DeletionCheck(deletions: 0, paths: 1, needsApproval: false),
        reviewToken: reviewTokenA, pairRevision: 1
    )
    let resolved = preview(archivesInA: 0, needsApproval: false)
    let client = DeferredSyncClient()
    await client.setPreviewReply(.working(.preview, cancelling: false))
    await client.holdHistoryReplies()
    await client.holdResolveReplies()
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()
    let starting = Task { await store.preview(pair.id) }
    await client.finishPolling(.previewed(conflicted))

    // The run is done; its history is still loading. The pair shows the
    // conflict and offers the choice normally.
    #expect(await client.waitForHistoryCalls(1, timeout: .seconds(3)))
    #expect(await eventually { store.state(for: pair.id) == .previewed(conflicted) })
    let viewModel = PairSyncViewModel(pair: pair, store: store, pairs: catalog(), agentConnected: true)
    #expect(viewModel.canStart)
    #expect(viewModel.rows.contains { $0.resolvable != nil })
    #expect(!viewModel.isResolving)

    // The tap's command must reach the agent even though the terminal
    // run's history request has not answered.
    let resolving = Task {
        await store.resolve(pair.id, path: Data("notes.txt".utf8), keep: .a,
                            reviewToken: conflicted.reviewToken)
    }
    #expect(await client.waitForResolveCalls(1, timeout: .seconds(3)))

    // The accepted choice's own fresh preview lands on the pair.
    await client.resumeStarts(replying: .previewed(resolved))
    while await client.pendingResolveCalls > 0 {
        await client.replyResolve(.idle)
    }
    #expect(await eventually { store.state(for: pair.id) == .previewed(resolved) })

    await client.finishHistory()
    await client.finishPolling(.idle)
    while await client.pendingResolveCalls > 0 {
        await client.replyResolve(.idle)
    }
    while await client.pendingStartCalls > 0 {
        await client.replyStart(.idle)
    }
    await resolving.value
    await starting.value
}

// The approval dialog acts on the request captured when it was
// presented: the pair, the plan's fingerprint, the counts and the
// wording. A state that moves on afterwards cannot redirect the tap to
// a plan the user never saw; the agent decides whether the presented
// plan is still current.
@MainActor
@Test
func approvalRequestsActOnThePlanTheyPresented() async throws {
    let first = preview(archivesInA: 60, needsApproval: true)
    let second = preview(archivesInA: 5, needsApproval: true, reviewToken: reviewTokenB)
    let client = ScriptedSyncClient([.needsApproval(first), .previewed(second), .idle])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()
    let viewModel = PairSyncViewModel(pair: pair, store: store, pairs: catalog(), agentConnected: true)

    await store.preview(pair.id)
    let request = try #require(viewModel.approvalRequest)
    #expect(request.pairID == pair.id)
    #expect(request.pairRevision == first.pairRevision)
    #expect(request.reviewToken == first.reviewToken)
    #expect(request.approvedA == 60)
    #expect(request.approvedB == 0)

    // A different preview lands before the presented request is
    // approved; the tap still acts on the plan it showed.
    await store.checkAll([pair])
    #expect(store.state(for: pair.id) == .previewed(second))
    await viewModel.approve(request)
    #expect(await client.approvals.map(\.0) == [60])
    #expect(await client.approvals.map(\.2) == [first.reviewToken])
}

// A conflict row keeps the review context of the preview it was built
// from: after a newer preview lands, a choice made on the old row still
// presents the old plan's token, which the agent can then reject as
// stale instead of applying it to a plan the user never reviewed.
@MainActor
@Test
func conflictRowsKeepTheTokenOfThePreviewTheyCameFrom() async throws {
    let first = PreviewSummary(
        changes: [PlannedChange(path: Data("notes.txt".utf8), action: .conflict(.bothChanged))],
        copies: 0, updates: 0, archives: 0, conflicts: 1, blocked: 0, kept: 0,
        a: DeletionCheck(deletions: 0, paths: 1, needsApproval: false),
        b: DeletionCheck(deletions: 0, paths: 1, needsApproval: false),
        reviewToken: reviewTokenA, pairRevision: 1
    )
    let second = preview(archivesInA: 0, needsApproval: false, reviewToken: reviewTokenB)
    let client = ScriptedSyncClient([.previewed(first), .previewed(second), .previewed(second)])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let pair = pair()
    let viewModel = PairSyncViewModel(pair: pair, store: store, pairs: catalog(), agentConnected: true)

    await store.preview(pair.id)
    let row = try #require(viewModel.rows.first)
    #expect(row.review?.token == first.reviewToken)
    #expect(row.review?.pairRevision == first.pairRevision)

    // The pair previews into a different plan; the captured row still
    // names the plan it came from, not the one now shown.
    await store.preview(pair.id)
    #expect(store.state(for: pair.id) == .previewed(second))
    await viewModel.keep(.a, for: row)
    #expect(await client.keptTokens == [first.reviewToken])
}

// A preview published for a pair revision the catalog no longer has is
// obsolete: its plan may name roots that changed since, so the UI shows
// it as information only — no approval, no resolvable rows, no alert —
// while a fresh preview stays available.
@MainActor
@Test
func outdatedPreviewsOfferNoReviewActions() async {
    let stale = preview(archivesInA: 60, needsApproval: true, pairRevision: 0)
    let conflicted = PreviewSummary(
        changes: [PlannedChange(path: Data("notes.txt".utf8), action: .conflict(.bothChanged))],
        copies: 0, updates: 0, archives: 0, conflicts: 1, blocked: 0, kept: 0,
        a: DeletionCheck(deletions: 0, paths: 1, needsApproval: false),
        b: DeletionCheck(deletions: 0, paths: 1, needsApproval: false),
        reviewToken: reviewTokenA, pairRevision: 0
    )
    let current = preview(archivesInA: 2, needsApproval: false,
                          reviewToken: reviewTokenB, pairRevision: 1)
    let pair = pair()
    #expect(pair.revision == 1)
    #expect(SyncStore.alert(for: pair, state: .needsApproval(stale)) == nil)

    let client = ScriptedSyncClient([.needsApproval(stale), .previewed(conflicted),
                                     .previewed(current)])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair, store: store, pairs: catalog(), agentConnected: true)

    await store.preview(pair.id)
    #expect(viewModel.approvalRequest == nil)
    #expect(viewModel.statusText == "The pair changed since that preview; preview again.")
    #expect(viewModel.canStart)

    // An obsolete plan cannot fall back to an unreviewed run of the
    // changed pair: Sync stays off and the tap issues nothing.
    #expect(!viewModel.canSync)
    await viewModel.sync()
    #expect(await client.approvals.isEmpty)

    // An outdated previewed plan is shown without actions either.
    await store.preview(pair.id)
    #expect(viewModel.rows.isEmpty)
    #expect(!viewModel.showsChangeFilter)
    #expect(viewModel.shortStatus == "Preview again")
    #expect(!viewModel.canSync)
    await viewModel.sync()
    #expect(await client.approvals.isEmpty)

    // From a current preview Sync is the reviewed run: it echoes that
    // plan's token rather than going out unreviewed.
    await store.preview(pair.id)
    #expect(viewModel.canSync)
    await viewModel.sync()
    #expect(await client.approvals.map(\.2) == [current.reviewToken])
}

@MainActor
@Test
func aSyncThatChangedSomethingCanBeUndoneAndTheUndoExplainsItself() async {
    let result = RunSummary(cancelled: false, copied: 2, updated: 0, archived: 1, recovered: 0,
                            conflicts: 0, failures: [], failureCount: 0,
                            archive: "2026-10-18T09-30-00.123Z")
    let undone = UndoSummary(cancelled: false, removed: 2, restored: 1, beside: 0,
                             notUndone: [RunFailure(path: Data("notes.txt".utf8),
                                                    message: "Changed since the sync; left as it is")],
                             notUndoneCount: 1, archive: "2026-10-18T10-00-00.000Z")
    let client = ScriptedSyncClient([.finished(result), .working(.undo, cancelling: false), .undone(undone)])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)

    await viewModel.refresh()
    #expect(viewModel.canUndo)
    await viewModel.undo()
    #expect(await client.undone == 1)
    #expect(store.state(for: pair().id) == .undone(undone))
    #expect(viewModel.statusText.contains("put back 1, archived 2"))
    #expect(viewModel.statusText.contains("1 items were left as they are."))
    #expect(viewModel.shortStatus == "Partly undone")
    #expect(viewModel.rows.map(\.path) == ["notes.txt"])
    // An undo is not undone again; previewing shows what syncing would redo.
    #expect(!viewModel.canUndo)
}

// Review: a cancelled undo read "Undone" in the status line and as a
// complete undo in History.
@MainActor
@Test
func stoppedUndosNeverReadAsComplete() async {
    let stopped = UndoSummary(cancelled: true, removed: 1, restored: 2, beside: 0, notUndone: [],
                              notUndoneCount: 0, archive: "2026-10-18T10-00-00.000Z")
    let store = SyncStore(client: ScriptedSyncClient([.undone(stopped)]), pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    await viewModel.refresh()
    #expect(viewModel.shortStatus == "Undo stopped")

    let started = Date(timeIntervalSince1970: 1_000)
    let cancelled = RunRecord(started: started, finished: started, trigger: .undo, end: .cancelled,
                              copied: 0, updated: 0, archived: 1, recovered: 2, conflicts: 0,
                              failures: 0, message: nil)
    #expect(PairSyncViewModel.summary(cancelled) == "Undo stopped: put back 2, archived 1.")
}

@MainActor
@Test
func syncsThatChangedNothingOfferNoUndo() async {
    let nothing = RunSummary(cancelled: false, copied: 0, updated: 0, archived: 0, recovered: 0,
                             conflicts: 0, failures: [], failureCount: 0,
                             archive: "2026-10-18T09-30-00.123Z")
    let client = ScriptedSyncClient([.finished(nothing)])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)

    await viewModel.refresh()
    #expect(!viewModel.canUndo)
    await viewModel.undo()
    #expect(await client.undone == 0)
}

@MainActor
@Test
func previewedChangesCanBeCopiedTheOtherWayOrSkippedOnce() async {
    // The fixture copies photos/a.jpg to B and archives a file in A.
    let shown = preview(archivesInA: 1, needsApproval: false)
    let client = ScriptedSyncClient([.previewed(shown), .previewed(shown), .previewed(shown),
                                     .previewed(shown), .previewed(shown)])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    await viewModel.preview()
    let rows = viewModel.rows
    #expect(rows.map(\.otherWay) == [.b, .a])
    #expect(rows.map(\.otherWayTitle) == ["Archive in Folder A", "Restore to Folder B"])
    #expect(rows.allSatisfy { $0.item != nil })

    // Copying the other way keeps the folder the change would have changed.
    await viewModel.copyOtherWay(rows[1])
    #expect(await client.kept == [.a])
    #expect(await client.keptTokens == [shown.reviewToken])

    await viewModel.skipOnce(viewModel.rows[0])
    #expect(await client.skipped == [Data("photos/a.jpg".utf8)])
    #expect(await client.skipTokens == [shown.reviewToken])
}

@MainActor
@Test
func workingJobsShowLiveCountsWithoutCountingAsNewStates() async {
    let comparing = WorkProgress(listed: 200, checked: 50)
    let applying = WorkProgress(listed: 200, checked: 200, changes: 4, applied: 1,
                                bytes: 4_000_000, copied: 1_000_000)
    let client = DeferredSyncClient()
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)

    // The refresh keeps polling while the job works; each reply is held.
    let refresh = Task { await viewModel.refresh() }
    await client.waitForStatusCalls(1)
    await client.reply(.working(.run, cancelling: false, progress: comparing))
    await client.waitForStatusCalls(2)
    #expect(viewModel.statusText == "Comparing folders… 50 of 200 items checked")
    #expect(viewModel.progressFraction == 0.25)

    // Another Mac is syncing one of the folders; nothing to measure yet.
    let waiting = WorkProgress(listed: 200, checked: 200, changes: 4, waitingFor: "Studio")
    await client.reply(.working(.run, cancelling: false, progress: waiting))
    await client.waitForStatusCalls(3)
    #expect(viewModel.statusText == "Waiting for “Studio” to finish syncing these folders…")
    #expect(viewModel.progressFraction == nil)

    await client.reply(.working(.run, cancelling: false, progress: applying))
    await client.waitForStatusCalls(4)
    #expect(viewModel.statusText.hasPrefix("Syncing… 1 of 4 changes · "))
    #expect(viewModel.progressFraction == 0.25)
    // New counts are the same state: no new history, listing or alert.
    #expect(PairSyncState.working(.run, cancelling: false, progress: comparing)
        == .working(.run, cancelling: false, progress: applying))

    await client.reply(.idle)
    await refresh.value
    #expect(viewModel.progressFraction == nil)
}

@Test
func quickLookShowsTheFileAChangeWouldCopyOnlyWhereItIsOnThisMac() {
    let local = PairSettings(name: "Docs", left: SelectedFolder(bytes: Data("/Users/a/Docs".utf8)),
                             right: SelectedFolder(bytes: Data("sftp://nas/docs".utf8)),
                             mode: pair().settings.mode, options: pair().settings.options)
    let toServer = PlannedChange(path: Data("a/report.pdf".utf8), action: .copy(to: .b))
    #expect(PairSyncViewModel.lookURL(toServer, settings: local)?.path == "/Users/a/Docs/a/report.pdf")
    // The file a copy from the server would bring is not on this Mac yet.
    let fromServer = PlannedChange(path: Data("b.txt".utf8), action: .copy(to: .a))
    #expect(PairSyncViewModel.lookURL(fromServer, settings: local) == nil)
    #expect(PairSyncViewModel.lookURL(PlannedChange(path: Data("c".utf8), action: .archive(on: .a)),
                                      settings: local)?.lastPathComponent == "c")
    #expect(PairSyncViewModel.lookURL(PlannedChange(path: Data("d".utf8), action: .blocked),
                                      settings: local) == nil)
}

@MainActor
@Test
func searchNarrowsTheListedChangesWithoutRefetching() async {
    let summary = PreviewSummary(
        changes: [PlannedChange(path: Data("docs/report.txt".utf8), action: .copy(to: .b)),
                  PlannedChange(path: Data("Photos/IMG_1.RAW".utf8), action: .copy(to: .b)),
                  PlannedChange(path: Data("photos/notes.txt".utf8), action: .archive(on: .a))],
        copies: 2, updates: 0, archives: 4, conflicts: 0, blocked: 0, kept: 0,
        a: DeletionCheck(deletions: 4, paths: 10, needsApproval: false),
        b: DeletionCheck(deletions: 0, paths: 10, needsApproval: false),
        reviewToken: reviewTokenA, pairRevision: 1
    )
    let client = ScriptedSyncClient([.previewed(summary)])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    await viewModel.preview()
    #expect(viewModel.rows.count == 3)
    #expect(viewModel.showsSearch)

    viewModel.searchText = "photos"
    #expect(viewModel.searchText == "photos")
    #expect(viewModel.rows.map(\.path) == ["Photos/IMG_1.RAW", "photos/notes.txt"])
    // The hidden-rows text still counts unfetched changes, not searched-away rows.
    #expect(viewModel.hiddenRowsText == "and 3 more")
    // Nothing is refetched: only the preview request happened.
    #expect(await client.changeTokens.isEmpty)

    viewModel.searchText = "DOCS"
    #expect(viewModel.rows.map(\.path) == ["docs/report.txt"])
    viewModel.searchText = "zzz"
    #expect(viewModel.rows.isEmpty)
    #expect(viewModel.showsSearch)
    viewModel.searchText = "  "
    #expect(viewModel.rows.count == 3)
}

/// Pages differ by the kind filter, so a search's stacking on it shows.
private actor KindPageClient: SyncClient {
    private let reply: PairSyncState
    private(set) var filters: [ChangeFilter?] = []

    init(_ reply: PairSyncState) { self.reply = reply }

    func preview(_ id: String) async throws -> PairSyncState { reply }
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64,
             reviewToken: String?) async throws -> PairSyncState { reply }
    func status(_ id: String) async throws -> PairSyncState { reply }
    func cancel(_ id: String) async throws -> PairSyncState { .idle }
    func undo(_ id: String) async throws -> PairSyncState { .idle }
    func resolve(_ id: String, path: Data, keep: FolderSide,
                 reviewToken: String) async throws -> PairSyncState { .idle }
    func keepBoth(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState { .idle }
    func skipOnce(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState { .idle }
    func history(_ id: String) async throws -> [RunRecord] { [] }

    func changes(_ id: String, after: Data?, filter: ChangeFilter?,
                 reviewToken: String) async throws -> ChangePage {
        filters.append(filter)
        let page: [PlannedChange] = switch filter {
        case .copies:
            [PlannedChange(path: Data("alpha.txt".utf8), action: .copy(to: .b)),
             PlannedChange(path: Data("beta.txt".utf8), action: .copy(to: .b))]
        case .conflicts:
            [PlannedChange(path: Data("alpha.txt".utf8), action: .conflict(.bothChanged)),
             PlannedChange(path: Data("gamma.txt".utf8), action: .conflict(.bothChanged))]
        default:
            []
        }
        return ChangePage(changes: page, more: false)
    }
}

@MainActor
@Test
func searchStacksOnTheKindFilter() async {
    let summary = preview(archivesInA: 0, needsApproval: false)
    let client = KindPageClient(.previewed(summary))
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    await viewModel.preview()

    await viewModel.showChanges(.copies)
    #expect(viewModel.rows.map(\.path) == ["alpha.txt", "beta.txt"])
    viewModel.searchText = "alpha"
    #expect(viewModel.rows.map(\.path) == ["alpha.txt"])

    // A new kind listing narrows the same way.
    await viewModel.showChanges(.conflicts)
    #expect(viewModel.changeFilter == .conflicts)
    #expect(viewModel.rows.map(\.path) == ["alpha.txt"])
    viewModel.searchText = ""
    #expect(viewModel.rows.map(\.path) == ["alpha.txt", "gamma.txt"])
}

@MainActor
@Test
func aFinishedSyncWithFollowupsShowsTheCompletionCard() async throws {
    let conflicted = preview(archivesInA: 0, needsApproval: false)
    let result = RunSummary(cancelled: false, copied: 2, updated: 0, archived: 1, recovered: 0,
                            conflicts: 3, failures: [], failureCount: 0, archive: "run")
    let client = ScriptedSyncClient([.finished(result), .previewed(conflicted)])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)
    await viewModel.sync()

    let card = try #require(viewModel.completionCard)
    #expect(card.summary == viewModel.statusText)
    #expect(card.conflicts == 3)
    #expect(card.undoable)
    #expect(viewModel.canUndo)

    // Review previews again and shows only the conflicts.
    await viewModel.reviewConflicts()
    #expect(store.state(for: pair().id) == .previewed(conflicted))
    #expect(viewModel.changeFilter == .conflicts)
    #expect(await client.changeTokens == [conflicted.reviewToken])
    #expect(viewModel.completionCard == nil)
}

@MainActor
@Test
func theCompletionCardAppearsOnlyWhenARunLeavesWorkToReview() async throws {
    let bare = RunSummary(cancelled: false, copied: 0, updated: 2, archived: 0, recovered: 0,
                          conflicts: 0, failures: [], failureCount: 0, archive: "run")
    let undoable = RunSummary(cancelled: false, copied: 1, updated: 0, archived: 0, recovered: 0,
                              conflicts: 0, failures: [], failureCount: 0, archive: "run")
    let conflictsOnly = RunSummary(cancelled: false, copied: 0, updated: 0, archived: 0, recovered: 0,
                                   conflicts: 2, failures: [], failureCount: 0, archive: "run")
    let undone = UndoSummary(cancelled: false, removed: 1, restored: 2, beside: 0,
                             notUndone: [], notUndoneCount: 0, archive: "undo")
    let client = ScriptedSyncClient([.finished(bare), .finished(undoable),
                                     .finished(conflictsOnly), .undone(undone)])
    let store = SyncStore(client: client, pollInterval: .milliseconds(1))
    let viewModel = PairSyncViewModel(pair: pair(), store: store, pairs: catalog(), agentConnected: true)

    // Metadata-only runs have nothing to review or undo.
    await viewModel.refresh()
    #expect(viewModel.completionCard == nil)

    // An undoable run offers Undo without a Review button.
    await viewModel.refresh()
    let undoableCard = try #require(viewModel.completionCard)
    #expect(undoableCard.conflicts == 0)
    #expect(undoableCard.undoable)

    // A conflicted run offers Review without Undo.
    await viewModel.refresh()
    let conflictedCard = try #require(viewModel.completionCard)
    #expect(conflictedCard.conflicts == 2)
    #expect(!conflictedCard.undoable)
    #expect(!viewModel.canUndo)

    // An undone run is behind it.
    await viewModel.refresh()
    #expect(viewModel.completionCard == nil)
}
