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
}

enum ChangeAction: Equatable, Sendable {
    case copy(to: FolderSide)
    case update(to: FolderSide)
    case archive(on: FolderSide)
    case conflict(ConflictReason)
    case blocked
    case keep
}

struct PlannedChange: Equatable, Sendable {
    let path: Data
    let action: ChangeAction
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

    var total: UInt64 { copies + updates + archives + conflicts + blocked + kept }
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
}

enum SyncJob: Sendable {
    case preview
    case run
}

enum PairSyncState: Equatable, Sendable {
    case idle
    case working(SyncJob, cancelling: Bool)
    case previewed(PreviewSummary)
    /// Too many deletions for this run; nothing was written.
    case needsApproval(PreviewSummary)
    case finished(RunSummary)
    case failed(String)
}

protocol SyncClient: Sendable {
    func preview(_ id: String) async throws -> PairSyncState
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64) async throws -> PairSyncState
    func status(_ id: String) async throws -> PairSyncState
    func cancel(_ id: String) async throws -> PairSyncState
    func resolve(_ id: String, path: Data, keep: FolderSide) async throws -> PairSyncState
}

// Keep generated async values inside one actor; only native Sendable records leave it.
private actor CoreSyncClient: SyncClient {
    func preview(_ id: String) async throws -> PairSyncState {
        state(try await FoldoverCore.syncPreview(id: id))
    }

    func run(_ id: String, approvedA: UInt64, approvedB: UInt64) async throws -> PairSyncState {
        state(try await FoldoverCore.syncRun(
            id: id, approvedLeftDeletions: approvedA, approvedRightDeletions: approvedB
        ))
    }

    func status(_ id: String) async throws -> PairSyncState {
        state(try await FoldoverCore.syncStatus(id: id))
    }

    func cancel(_ id: String) async throws -> PairSyncState {
        state(try await FoldoverCore.syncCancel(id: id))
    }

    func resolve(_ id: String, path: Data, keep: FolderSide) async throws -> PairSyncState {
        state(try await FoldoverCore.syncResolve(id: id, path: path, keep: keep == .a ? .left : .right))
    }

    private func state(_ state: SyncJobState) -> PairSyncState {
        switch state {
        case .idle: .idle
        case .working(let job, let cancelling): .working(job == .preview ? .preview : .run, cancelling: cancelling)
        case .previewed(let preview): .previewed(summary(preview))
        case .needsApproval(let preview): .needsApproval(summary(preview))
        case .finished(let result): .finished(summary(result))
        case .failed(let message): .failed(message)
        }
    }

    private func summary(_ preview: SyncPreviewSummary) -> PreviewSummary {
        PreviewSummary(
            changes: preview.changes.map { PlannedChange(path: $0.path, action: action($0.kind)) },
            copies: preview.copies, updates: preview.updates, archives: preview.archives,
            conflicts: preview.conflicts, blocked: preview.blocked, kept: preview.kept,
            a: check(preview.left), b: check(preview.right)
        )
    }

    private func summary(_ result: SyncRunSummary) -> RunSummary {
        RunSummary(
            cancelled: result.cancelled, copied: result.copied, updated: result.updated,
            archived: result.archived, recovered: result.recovered, conflicts: result.conflicts,
            failures: result.failures.map { RunFailure(path: $0.path, message: $0.message) },
            failureCount: result.failureCount, archive: result.archive
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
    private var polling: Set<String> = []
    /// Writes a pair has seen. A reply requested before the latest write
    /// is stale and must not overwrite it, even if the state cycles back
    /// to the same value.
    private var generations: [String: Int] = [:]
    /// Approval results the user declined; the same result stays declined
    /// until an explicit job or a different reviewable result.
    private var declined: [String: PreviewSummary] = [:]
    private(set) var states: [String: PairSyncState] = [:]
    private(set) var requestErrors: [String: String] = [:]

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
        for pair in pairs where !polling.contains(pair.id) {
            let previous = states[pair.id]
            let generation = generations[pair.id] ?? 0
            guard var state = try? await client.status(pair.id) else { continue }
            // A local write while the request was in flight is newer.
            guard (generations[pair.id] ?? 0) == generation else { continue }
            if case .needsApproval(let preview) = state, declined[pair.id] == preview {
                state = .previewed(preview)
            }
            set(pair.id, state)
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
    func requestError(for id: String) -> String? { requestErrors[id] }

    func refresh(_ id: String) async {
        let client = client
        await perform(id) { try await client.status(id) }
    }

    func preview(_ id: String) async {
        let client = client
        await perform(id) { try await client.preview(id) }
    }

    /// Approvals are deletion counts the user accepted for each folder.
    func run(_ id: String, approvedA: UInt64 = 0, approvedB: UInt64 = 0) async {
        let client = client
        await perform(id) { try await client.run(id, approvedA: approvedA, approvedB: approvedB) }
    }

    func cancel(_ id: String) async {
        let client = client
        await perform(id) { try await client.cancel(id) }
    }

    /// Keep one folder's version of a previewed conflict, then preview again
    /// so the list shows what the choice will do.
    func resolve(_ id: String, path: Data, keep: FolderSide) async {
        let client = client
        requestErrors[id] = nil
        do {
            _ = try await client.resolve(id, path: path, keep: keep)
        } catch {
            requestErrors[id] = PairStore.message(for: error)
            return
        }
        await preview(id)
    }

    /// Keep the reviewed changes visible after declining an approval.
    /// The decline holds for that result until an explicit job runs.
    func declineApproval(_ id: String) {
        guard case .needsApproval(let preview) = states[id] else { return }
        declined[id] = preview
        set(id, .previewed(preview))
    }

    private func perform(_ id: String, _ request: @Sendable () async throws -> PairSyncState) async {
        requestErrors[id] = nil
        declined[id] = nil
        do {
            set(id, try await request())
        } catch {
            requestErrors[id] = PairStore.message(for: error)
            return
        }
        await pollWhileWorking(id)
    }

    /// The poller owns the pair until the job leaves .working, so its
    /// replies are never stale; only checkAll checks the generation.
    private func pollWhileWorking(_ id: String) async {
        guard !polling.contains(id) else { return }
        polling.insert(id)
        defer { polling.remove(id) }
        while case .working = states[id] {
            try? await Task.sleep(for: pollInterval)
            do {
                set(id, try await client.status(id))
            } catch {
                requestErrors[id] = PairStore.message(for: error)
                return
            }
        }
    }

    /// Every write advances the pair's generation.
    private func set(_ id: String, _ state: PairSyncState) {
        generations[id, default: 0] += 1
        states[id] = state
    }
}
