import FoldoverCore
import FoldoverSystem
import Foundation
import Observation

enum FolderSide: Sendable {
    case a
    case b
}

enum ConflictReason: Sendable {
    case bothChanged
    case deletedVersusEdited
    case typeMismatch
    case metadataOnly
    /// Names one folder would store as one file; a rename resolves it.
    case nameClash
}

enum ChangeAction: Equatable, Sendable {
    case copy(to: FolderSide)
    case update(to: FolderSide)
    case archive(on: FolderSide)
    case conflict(ConflictReason)
    case blocked
    case keep
    /// Renamed in `on` from `PlannedChange.movedFrom`: the other folder
    /// moved it, so nothing is copied.
    case move(on: FolderSide)
}

struct PlannedChange: Equatable, Sendable {
    let path: Data
    let action: ChangeAction
    /// File data the change copies; 0 for everything else.
    var bytes: UInt64 = 0
    /// For a move, where it comes from.
    var movedFrom: Data? = nil
}

struct DeletionCheck: Equatable, Sendable {
    let deletions: UInt64
    let paths: UInt64
    let needsApproval: Bool
}

struct PreviewSummary: Equatable, Sendable {
    /// The first changes in path order; the counts cover all of them.
    let changes: [PlannedChange]
    let copies: UInt64
    let updates: UInt64
    let archives: UInt64
    let conflicts: UInt64
    let blocked: UInt64
    let kept: UInt64
    let a: DeletionCheck
    let b: DeletionCheck
    /// File data all copies move.
    var copyBytes: UInt64 = 0
    /// Fingerprint of the exact plan shown; approvals, conflict choices
    /// and change pages present it back to the agent.
    let reviewToken: String
    /// The pair revision this plan was made for; a different current
    /// revision makes the plan obsolete.
    let pairRevision: UInt64
    var moves: UInt64 = 0
    var failures: [RunFailure] = []
    var failureCount: UInt64 = 0

    var total: UInt64 { copies + updates + archives + conflicts + blocked + kept + moves }
}

struct RunFailure: Equatable, Sendable {
    let path: Data
    let message: String
}

struct RunSummary: Equatable, Sendable {
    let cancelled: Bool
    let copied: UInt64
    let updated: UInt64
    let archived: UInt64
    let recovered: UInt64
    let conflicts: UInt64
    let failures: [RunFailure]
    let failureCount: UInt64
    let archive: String
    /// Files and links renamed for moves the other folder made.
    var moved: UInt64 = 0
}

/// What undoing a pair's newest sync did.
struct UndoSummary: Equatable, Sendable {
    let cancelled: Bool
    /// Entries the sync created, now in this undo's archive folder.
    let removed: UInt64
    /// Entries it replaced or archived, back at their paths.
    let restored: UInt64
    /// Put back beside a newer entry at their path.
    let beside: UInt64
    /// The first entries left as they were, with why.
    let notUndone: [RunFailure]
    let notUndoneCount: UInt64
    let archive: String
}

/// Which planned changes to list.
enum ChangeFilter: CaseIterable, Hashable, Sendable {
    case copies
    case updates
    case archives
    case conflicts
    case moves
}

struct ChangePage: Equatable, Sendable {
    /// In path order.
    let changes: [PlannedChange]
    let more: Bool
}

/// The changes listed for a pair's latest preview, beyond the first page
/// its status carries.
struct ChangeListing: Equatable, Sendable {
    let filter: ChangeFilter?
    var changes: [PlannedChange]
    var more: Bool
}

enum RunTrigger: Sendable {
    case manual
    case scheduled
    /// An undo of the sync before it.
    case undo
}

enum RunEnd: Sendable {
    case completed
    case cancelled
    /// Stopped before writing: too many deletions to make without asking.
    case needsApproval
    case failed
}

/// One finished run in a pair's history.
struct RunRecord: Equatable, Sendable {
    let started: Date
    let finished: Date
    let trigger: RunTrigger
    let end: RunEnd
    let copied: UInt64
    let updated: UInt64
    let archived: UInt64
    /// Leftovers recovered; for an undo, what came back.
    let recovered: UInt64
    let conflicts: UInt64
    let failures: UInt64
    let message: String?
    /// Files and links renamed for moves the other folder made.
    var moved: UInt64 = 0
}

enum SyncJob: Sendable {
    case preview
    case run
    case undo
}

/// How far a working job has got (PLAN.md §3.2: live counters).
/// `changes` and `bytes` stay 0 until the plan is ready. This is display
/// data: two working states are the same state however far along, so new
/// counts never look like a new result.
struct WorkProgress: Equatable, Sendable {
    var listed: UInt64 = 0
    var checked: UInt64 = 0
    var changes: UInt64 = 0
    var applied: UInt64 = 0
    var bytes: UInt64 = 0
    var copied: UInt64 = 0
    /// Copy throughput so far, in bytes per second; 0 before the first
    /// copy lands.
    var rate: UInt64 = 0
    /// The Mac whose sync of a folder this job waits for, e.g. "Studio".
    var waitingFor: String?

    static let none = WorkProgress()

    static func == (_: WorkProgress, _: WorkProgress) -> Bool { true }
}

enum PairSyncState: Equatable, Sendable {
    case idle
    case working(SyncJob, cancelling: Bool, progress: WorkProgress = .none)
    case previewed(PreviewSummary)
    /// Too many deletions for this run; nothing was written.
    case needsApproval(PreviewSummary)
    case finished(RunSummary)
    /// The newest sync was undone; automatic syncs wait until a person
    /// syncs, so they do not redo it unseen.
    case undone(UndoSummary)
    case failed(String)
    /// An automatic sync waits for this folder, e.g. an unmounted disk.
    case waiting(FolderSide)
}

protocol SyncClient: Sendable {
    func preview(_ id: String) async throws -> PairSyncState
    /// `reviewToken` names the preview a person approved; nil only for
    /// runs that need no review.
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64,
             reviewToken: String?) async throws -> PairSyncState
    func status(_ id: String) async throws -> PairSyncState
    func cancel(_ id: String) async throws -> PairSyncState
    /// Undo the newest sync that changed anything.
    func undo(_ id: String) async throws -> PairSyncState
    /// `reviewToken` must be the token of the preview the choice was
    /// made from.
    func resolve(_ id: String, path: Data, keep: FolderSide,
                 reviewToken: String) async throws -> PairSyncState
    /// Save B's version of a both-changed conflict beside it; A's wins.
    func keepBoth(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState
    /// Leave a previewed change, and everything inside it, alone in the
    /// next run.
    func skipOnce(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState
    /// Finished runs, newest first.
    func history(_ id: String) async throws -> [RunRecord]
    /// A page of the latest preview's changes after `after`.
    /// `reviewToken` must be that preview's.
    func changes(_ id: String, after: Data?, filter: ChangeFilter?,
                 reviewToken: String) async throws -> ChangePage
}

// Keep generated async values inside one actor; only native Sendable records leave it.
private actor CoreSyncClient: SyncClient {
    func preview(_ id: String) async throws -> PairSyncState {
        state(try await FoldoverCore.syncPreview(id: id))
    }

    func run(_ id: String, approvedA: UInt64, approvedB: UInt64,
             reviewToken: String?) async throws -> PairSyncState {
        state(try await FoldoverCore.syncRun(
            id: id, approvedLeftDeletions: approvedA, approvedRightDeletions: approvedB,
            reviewToken: reviewToken
        ))
    }

    func status(_ id: String) async throws -> PairSyncState {
        state(try await FoldoverCore.syncStatus(id: id))
    }

    func cancel(_ id: String) async throws -> PairSyncState {
        state(try await FoldoverCore.syncCancel(id: id))
    }

    func undo(_ id: String) async throws -> PairSyncState {
        state(try await FoldoverCore.syncUndo(id: id))
    }

    func resolve(_ id: String, path: Data, keep: FolderSide,
                 reviewToken: String) async throws -> PairSyncState {
        state(try await FoldoverCore.syncResolve(
            id: id, path: path, keep: keep == .a ? .left : .right, reviewToken: reviewToken
        ))
    }

    func keepBoth(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState {
        state(try await FoldoverCore.syncKeepBoth(id: id, path: path, reviewToken: reviewToken))
    }

    func skipOnce(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState {
        state(try await FoldoverCore.syncSkipOnce(id: id, path: path, reviewToken: reviewToken))
    }

    func changes(_ id: String, after: Data?, filter: ChangeFilter?,
                 reviewToken: String) async throws -> ChangePage {
        let core: SyncChangeFilter? = switch filter {
        case .copies: .copy
        case .updates: .update
        case .archives: .archive
        case .conflicts: .conflict
        case .moves: .move
        case nil: nil
        }
        let page = try await FoldoverCore.syncChanges(
            id: id, after: after, filter: core, reviewToken: reviewToken
        )
        return ChangePage(
            changes: page.changes.map {
                PlannedChange(path: $0.path, action: action($0.kind), bytes: $0.bytes,
                              movedFrom: $0.movedFrom)
            },
            more: page.more
        )
    }

    func history(_ id: String) async throws -> [RunRecord] {
        try await FoldoverCore.pairHistory(id: id).map { entry in
            RunRecord(
                started: date(entry.startedMs), finished: date(entry.finishedMs),
                trigger: trigger(entry.trigger), end: end(entry.outcome),
                copied: entry.copied, updated: entry.updated, archived: entry.archived,
                recovered: entry.recovered, conflicts: entry.conflicts, failures: entry.failures,
                message: entry.message, moved: entry.moved
            )
        }
    }

    private func date(_ milliseconds: UInt64) -> Date {
        Date(timeIntervalSince1970: Double(milliseconds) / 1000)
    }

    private func trigger(_ trigger: RunTriggerKind) -> RunTrigger {
        switch trigger {
        case .manual: .manual
        case .scheduled: .scheduled
        case .undo: .undo
        }
    }

    private func end(_ outcome: RunOutcomeKind) -> RunEnd {
        switch outcome {
        case .completed: .completed
        case .cancelled: .cancelled
        case .needsApproval: .needsApproval
        case .failed: .failed
        }
    }

    private func state(_ state: SyncJobState) -> PairSyncState {
        switch state {
        case .idle: .idle
        case .working(let job, let cancelling, let progress):
            .working(self.job(job), cancelling: cancelling, progress: WorkProgress(
                listed: progress.listed, checked: progress.checked, changes: progress.changes,
                applied: progress.applied, bytes: progress.bytes, copied: progress.copied,
                rate: progress.rate, waitingFor: progress.waitingFor
            ))
        case .previewed(let preview): .previewed(summary(preview))
        case .needsApproval(let preview): .needsApproval(summary(preview))
        case .finished(let result): .finished(summary(result))
        case .undone(let result): .undone(summary(result))
        case .failed(let message): .failed(message)
        case .waiting(let folder): .waiting(side(folder))
        }
    }

    private func job(_ job: SyncJobKind) -> SyncJob {
        switch job {
        case .preview: .preview
        case .run: .run
        case .undo: .undo
        }
    }

    private func summary(_ result: SyncUndoSummary) -> UndoSummary {
        UndoSummary(
            cancelled: result.cancelled, removed: result.removed, restored: result.restored,
            beside: result.beside,
            notUndone: result.notUndone.map { RunFailure(path: $0.path, message: $0.message) },
            notUndoneCount: result.notUndoneCount, archive: result.archive
        )
    }

    private func summary(_ preview: SyncPreviewSummary) -> PreviewSummary {
        PreviewSummary(
            changes: preview.changes.map {
                PlannedChange(path: $0.path, action: action($0.kind), bytes: $0.bytes,
                              movedFrom: $0.movedFrom)
            },
            copies: preview.copies, updates: preview.updates, archives: preview.archives,
            conflicts: preview.conflicts, blocked: preview.blocked, kept: preview.kept,
            a: check(preview.left), b: check(preview.right), copyBytes: preview.copyBytes,
            reviewToken: preview.reviewToken, pairRevision: preview.pairRevision,
            moves: preview.moves,
            failures: preview.failures.map { RunFailure(path: $0.path, message: $0.message) },
            failureCount: preview.failureCount
        )
    }

    private func summary(_ result: SyncRunSummary) -> RunSummary {
        RunSummary(
            cancelled: result.cancelled, copied: result.copied, updated: result.updated,
            archived: result.archived, recovered: result.recovered, conflicts: result.conflicts,
            failures: result.failures.map { RunFailure(path: $0.path, message: $0.message) },
            failureCount: result.failureCount, archive: result.archive, moved: result.moved
        )
    }

    private func check(_ deletions: SyncSideDeletions) -> DeletionCheck {
        DeletionCheck(deletions: deletions.deletions, paths: deletions.paths,
                      needsApproval: deletions.needsApproval)
    }

    private func action(_ kind: SyncChangeKind) -> ChangeAction {
        switch kind {
        case .copy(let to): .copy(to: side(to))
        case .update(let to): .update(to: side(to))
        case .archive(let on): .archive(on: side(on))
        case .conflict(let reason): .conflict(conflict(reason))
        case .blocked: .blocked
        case .keep: .keep
        case .move(let on): .move(on: side(on))
        }
    }

    private func side(_ side: SyncSide) -> FolderSide {
        switch side {
        case .left: .a
        case .right: .b
        }
    }

    private func conflict(_ kind: SyncConflictKind) -> ConflictReason {
        switch kind {
        case .bothChanged: .bothChanged
        case .deletedVersusEdited: .deletedVersusEdited
        case .typeMismatch: .typeMismatch
        case .metadataOnly: .metadataOnly
        case .nameClash: .nameClash
        }
    }
}

/// A notification about an automatic sync that needs a person.
struct SyncAlert: Equatable, Sendable {
    let pairID: String
    let title: String
    let body: String
}

/// Starts previews and runs in the agent and polls each pair while its
/// job works. Results stay until the pair's next job.
@Observable
@MainActor
final class SyncStore {
    private let client: any SyncClient
    private let pollInterval: Duration
    private let alert: @MainActor (SyncAlert) async -> Void
    /// The pair's live poller, owned by token: a newer caller replaces
    /// the owner instead of skipping, and an old owner's reply or defer
    /// cannot touch the successor.
    private var polling: [String: Int] = [:]
    private var pollSequence = 0
    /// Writes a pair has seen. A reply requested before the latest write
    /// is stale and must not overwrite it, even if the state cycles back
    /// to the same value.
    private var generations: [String: Int] = [:]
    /// A foreground command the agent has not answered yet. One counts
    /// as a local write — replies requested before it are stale — keeps
    /// the pair busy for `canStart`, and refuses a second command
    /// without touching the accepted one's freshness.
    private enum PendingOperation {
        /// An explicit preview or run.
        case start
        /// A conflict choice; an accepted answer hands the pair to its
        /// own fresh preview without releasing ownership in between.
        case choice
    }
    private var pendingOperations: [String: PendingOperation] = [:]
    /// Approval results the user declined; the same result stays declined
    /// until an explicit job or a different reviewable result.
    private var declined: [String: PreviewSummary] = [:]
    /// The pair's latest change-list request, owned by token like the
    /// poller: only the newest request may publish a page or an error,
    /// and a new state — which starts a new list — retires every
    /// request still in flight.
    private var listingRequests: [String: Int] = [:]
    private var listingSequence = 0
    private(set) var states: [String: PairSyncState] = [:]
    private(set) var requestErrors: [String: String] = [:]
    private(set) var histories: [String: [RunRecord]] = [:]
    private(set) var listings: [String: ChangeListing] = [:]
    /// The text narrowing each pair's listed changes; view state held
    /// here because view models are recreated on every render.
    private(set) var searches: [String: String] = [:]

    init(client: any SyncClient = CoreSyncClient(), pollInterval: Duration = .milliseconds(500),
         alert: @escaping @MainActor (SyncAlert) async -> Void = SyncStore.postAlert) {
        self.client = client
        self.pollInterval = pollInterval
        self.alert = alert
    }

    private static let alerts = UserAlerts()

    private static func postAlert(_ alert: SyncAlert) async {
        await alerts.post(identifier: alert.pairID, title: alert.title, body: alert.body)
    }

    /// Check every pair once. Automatic runs change state without the app;
    /// outcomes that need a person become notifications. A pair's first
    /// observation never alerts, so launching the app does not replay them.
    func checkAll(_ pairs: [SyncPair]) async {
        for pair in pairs where polling[pair.id] == nil && pendingOperations[pair.id] == nil {
            let previous = states[pair.id]
            let generation = generations[pair.id] ?? 0
            guard var state = try? await client.status(pair.id) else { continue }
            // A local write while the request was in flight is newer.
            guard (generations[pair.id] ?? 0) == generation else { continue }
            state = normalized(pair.id, state)
            set(pair.id, state)
            if previous != state { await refreshHistory(pair.id) }
            guard let previous, previous != state, let alert = Self.alert(for: pair, state: state) else {
                continue
            }
            await self.alert(alert)
        }
    }

    nonisolated static func alert(for pair: SyncPair, state: PairSyncState) -> SyncAlert? {
        let name = pair.settings.name
        let body: String
        switch state {
        case .needsApproval(let preview):
            // A plan made for another pair revision is obsolete; the
            // next preview reports what the pair actually needs.
            guard preview.pairRevision == pair.revision else { return nil }
            body = String(localized: "An automatic sync stopped before archiving \(preview.archives) items. Open Foldover to review and approve it.")
        case .failed(let message):
            body = String(localized: "An automatic sync stopped: \(message)")
        case .finished(let result) where result.conflicts > 0:
            body = String(localized: "\(result.conflicts) conflicts need your decision.")
        case .finished(let result) where result.failureCount > 0:
            body = String(localized: "\(result.failureCount) items could not be synced. They will be retried.")
        default:
            return nil
        }
        return SyncAlert(pairID: pair.id, title: name, body: body)
    }

    func state(for id: String) -> PairSyncState { states[id] ?? .idle }
    func listing(for id: String) -> ChangeListing? { listings[id] }

    /// The pair's change-list text filter; it narrows the fetched rows
    /// and never refetches (PLAN.md §3.8).
    func searchText(for id: String) -> String { searches[id] ?? "" }
    func setSearchText(_ text: String, for id: String) {
        searches[id] = text.isEmpty ? nil : text
    }

    /// List the latest preview's changes of one kind, or all of them.
    func showChanges(_ id: String, filter: ChangeFilter?) async {
        await loadChanges(id, after: nil, filter: filter)
    }

    func loadMoreChanges(_ id: String) async {
        guard let listing = listings[id], listing.more, let last = listing.changes.last else { return }
        await loadChanges(id, after: last.path, filter: listing.filter)
    }

    /// The latest tap wins: a newer request — or a state change, which
    /// retired the list in `set` — drops this request's page and error
    /// instead of letting the last response to arrive overwrite them.
    private func loadChanges(_ id: String, after: Data?, filter: ChangeFilter?) async {
        // Pages echo the fingerprint of the preview they list; a state
        // without one has no review the agent could page.
        guard let reviewToken = reviewToken(for: id) else { return }
        listingSequence += 1
        let ticket = listingSequence
        listingRequests[id] = ticket
        requestErrors[id] = nil
        do {
            let page = try await client.changes(
                id, after: after, filter: filter, reviewToken: reviewToken)
            guard listingRequests[id] == ticket, !Task.isCancelled else { return }
            var listing = after == nil ? ChangeListing(filter: filter, changes: [], more: false) : listings[id]
            listing?.changes += page.changes
            listing?.more = page.more
            listings[id] = listing
        } catch {
            guard listingRequests[id] == ticket, !Task.isCancelled else { return }
            requestErrors[id] = PairStore.message(for: error)
        }
    }

    /// The review token of the preview the pair currently shows.
    private func reviewToken(for id: String) -> String? {
        switch states[id] {
        case .previewed(let preview), .needsApproval(let preview): preview.reviewToken
        default: nil
        }
    }

    func history(for id: String) -> [RunRecord] { histories[id] ?? [] }
    func requestError(for id: String) -> String? { requestErrors[id] }
    /// A foreground command, a job start or a conflict choice, is on
    /// the wire and unanswered.
    func isBusy(_ id: String) -> Bool { pendingOperations[id] != nil }
    /// A conflict choice specifically is on the wire and unanswered.
    func isResolving(_ id: String) -> Bool { pendingOperations[id] == .choice }

    /// Wait until the pair's job ends, whichever poller follows it. A
    /// refresh takes over the poller of the request that started the
    /// job, and that request then returns while the job still works.
    /// When nothing follows the job any more (a poll failed, or the
    /// caller that took the poller over went away), follow it here; if
    /// the agent does not answer, stop waiting instead of spinning until
    /// it is back.
    func waitWhileWorking(_ id: String) async {
        while case .working = states[id], !Task.isCancelled {
            if polling[id] == nil, pendingOperations[id] == nil {
                await refresh(id)
                if requestErrors[id] != nil { return }
                guard case .working = states[id] else { return }
            }
            // Every pass that still reads working sleeps, so a refresh
            // whose reply was dropped cannot make this spin.
            try? await Task.sleep(for: pollInterval)
        }
    }

    func refresh(_ id: String) async {
        // A pending command's own reply is fresher; asking again would
        // race it.
        guard pendingOperations[id] == nil else { return }
        let client = client
        await perform(id, .other) { try await client.status(id) }
        await refreshHistory(id)
    }

    /// History is informational: a failed request keeps the last list.
    func refreshHistory(_ id: String) async {
        guard let runs = try? await client.history(id) else { return }
        histories[id] = runs
    }

    func preview(_ id: String) async {
        guard beginOperation(id, .start) else { return }
        let client = client
        await perform(id, .start) { try await client.preview(id) }
    }

    /// Approvals are deletion counts the user accepted for each folder;
    /// `reviewToken` names the preview they were read from.
    func run(_ id: String, approvedA: UInt64 = 0, approvedB: UInt64 = 0,
             reviewToken: String? = nil) async {
        guard beginOperation(id, .start) else { return }
        let client = client
        await perform(id, .start) {
            try await client.run(id, approvedA: approvedA, approvedB: approvedB,
                                 reviewToken: reviewToken)
        }
    }

    /// Undo the pair's newest sync that changed anything.
    func undo(_ id: String) async {
        guard beginOperation(id, .start) else { return }
        let client = client
        await perform(id, .start) { try await client.undo(id) }
    }

    func cancel(_ id: String) async {
        // A pending command's answer is fresher than anything cancel
        // could ask for; the job to stop appears when it lands.
        guard pendingOperations[id] == nil else { return }
        let client = client
        await perform(id, .other) { try await client.cancel(id) }
    }

    /// Keep one folder's version of a previewed conflict, then preview again
    /// so the list shows what the choice will do. `reviewToken` names the
    /// preview the choice was made from.
    func resolve(_ id: String, path: Data, keep: FolderSide, reviewToken: String) async {
        let client = client
        await choose(id) {
            try await client.resolve(id, path: path, keep: keep, reviewToken: reviewToken)
        }
    }

    /// Keep both versions: Folder B's is saved beside the item and Folder
    /// A's wins; the next sync copies both. Previews again afterwards.
    func keepBoth(_ id: String, path: Data, reviewToken: String) async {
        let client = client
        await choose(id) { try await client.keepBoth(id, path: path, reviewToken: reviewToken) }
    }

    /// Leave a previewed change alone in the next sync, then preview again.
    func skipOnce(_ id: String, path: Data, reviewToken: String) async {
        let client = client
        await choose(id) { try await client.skipOnce(id, path: path, reviewToken: reviewToken) }
    }

    /// A conflict choice owns the pair like a start does: reads and
    /// declines skip it, a second command is refused, and its accepted
    /// answer keeps ownership for its own fresh preview.
    private func choose(_ id: String, _ request: @Sendable () async throws -> PairSyncState) async {
        guard beginOperation(id, .choice) else { return }
        requestErrors[id] = nil
        let generation = generations[id] ?? 0
        do {
            _ = try await request()
        } catch {
            pendingOperations[id] = nil
            guard !Task.isCancelled, (generations[id] ?? 0) == generation else { return }
            requestErrors[id] = PairStore.message(for: error)
            return
        }
        // A cancelled or superseded choice releases the pair instead of
        // previewing over whatever came next.
        guard !Task.isCancelled, (generations[id] ?? 0) == generation else {
            pendingOperations[id] = nil
            return
        }
        pendingOperations[id] = .start
        let client = client
        await perform(id, .start) { try await client.preview(id) }
    }

    /// Show a failed request made elsewhere for this pair, e.g. a skip rule.
    func report(_ message: String, for id: String) {
        requestErrors[id] = message
    }

    /// Keep the reviewed changes visible after declining an approval.
    /// The decline holds for that result until an explicit job runs.
    /// Hiding a pending command's dialog is not a new decline.
    func declineApproval(_ id: String) {
        guard pendingOperations[id] == nil,
              case .needsApproval(let preview) = states[id] else { return }
        declined[id] = preview
        set(id, .previewed(preview))
    }

    /// Whether a foreground request is an explicit job start. A start's
    /// answer releases the pair's pending command and — when its reply is
    /// the newest — spends the declined approval, so the job resurfaces
    /// reviewable results; every other request leaves both alone.
    private enum RequestKind {
        case start
        case other
    }

    /// Registration invalidates older replies and relinquishes the old
    /// poller. A refused command changes nothing: it cannot retire the
    /// operation the pair already accepted. A choice reviews a shown
    /// plan, so a job the agent is still working supersedes it.
    private func beginOperation(_ id: String, _ operation: PendingOperation) -> Bool {
        guard pendingOperations[id] == nil else { return false }
        if operation == .choice, polling[id] != nil { return false }
        pendingOperations[id] = operation
        generations[id, default: 0] += 1
        polling[id] = nil
        return true
    }

    /// Runs one request and publishes its outcome. A reply or error that
    /// a newer local write superseded — or that belongs to a cancelled
    /// caller — is dropped instead of landing on the pair's current job.
    private func perform(_ id: String, _ kind: RequestKind,
                         _ request: @Sendable () async throws -> PairSyncState) async {
        requestErrors[id] = nil
        let generation = generations[id] ?? 0
        let reply: PairSyncState
        do {
            reply = try await request()
        } catch {
            if kind == .start { pendingOperations[id] = nil }
            guard !Task.isCancelled, (generations[id] ?? 0) == generation else { return }
            requestErrors[id] = PairStore.message(for: error)
            return
        }
        if kind == .start { pendingOperations[id] = nil }
        guard !Task.isCancelled, (generations[id] ?? 0) == generation else { return }
        // An explicit job that ran re-arms approval requests for the pair.
        if kind == .start { declined[id] = nil }
        set(id, normalized(id, reply))
        await pollWhileWorking(id)
    }

    /// The latest caller owns the pair's poller by token. A reply an old
    /// poller awaited across a cancel or a new job is stale: it is
    /// dropped and only that poller ends, while the successor keeps
    /// polling. A cancelled caller stops polling without cancelling the
    /// agent's job.
    private func pollWhileWorking(_ id: String) async {
        guard case .working = states[id] else { return }
        pollSequence += 1
        let owner = pollSequence
        polling[id] = owner
        defer {
            if polling[id] == owner { polling[id] = nil }
        }
        while case .working = states[id], polling[id] == owner, !Task.isCancelled {
            do {
                try await Task.sleep(for: pollInterval)
            } catch {
                return
            }
            guard polling[id] == owner, !Task.isCancelled else { return }
            let generation = generations[id] ?? 0
            let reply: PairSyncState
            do {
                reply = try await client.status(id)
            } catch {
                guard polling[id] == owner, !Task.isCancelled,
                      (generations[id] ?? 0) == generation else { return }
                requestErrors[id] = PairStore.message(for: error)
                return
            }
            guard polling[id] == owner,
                  (generations[id] ?? 0) == generation else { return }
            set(id, normalized(id, reply))
        }
        if polling[id] == owner { polling[id] = nil }
        await refreshHistory(id)
    }

    /// A declined approval keeps the identical result from resurfacing
    /// until an explicit job or a changed result; normalize before the
    /// state is published or alerted on.
    private func normalized(_ id: String, _ state: PairSyncState) -> PairSyncState {
        if case .needsApproval(let preview) = state, declined[id] == preview {
            return .previewed(preview)
        }
        return state
    }

    /// Every write advances the pair's generation; a new state starts a
    /// new change list and retires the requests still fetching the old
    /// one.
    private func set(_ id: String, _ state: PairSyncState) {
        generations[id, default: 0] += 1
        if states[id] != state {
            listings[id] = nil
            listingRequests[id] = nil
        }
        states[id] = state
    }
}
