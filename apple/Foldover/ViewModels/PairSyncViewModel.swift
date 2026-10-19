import Foundation

/// One planned or failed change, ready to render.
struct ChangeRow: Identifiable, Equatable {
    let id: Int
    let path: String
    let description: String
    /// Raw path bytes of a conflict a person can resolve by keeping a side.
    let resolvable: Data?
}

/// Deletion counts the user accepted for each folder.
struct DeletionApproval: Equatable, Sendable {
    let a: UInt64
    let b: UInt64
}

/// Preview and run controls for the selected pair: strings and actions only.
@MainActor
struct PairSyncViewModel {
    let pair: SyncPair
    let store: SyncStore
    let agentConnected: Bool

    private var state: PairSyncState { store.state(for: pair.id) }

    var isWorking: Bool {
        if case .working = state { return true }
        return false
    }
    var canStart: Bool { agentConnected && !isWorking && !store.isStarting(pair.id) }
    var canCancel: Bool {
        if case .working(_, let cancelling) = state { return !cancelling }
        return false
    }
    var requestError: String? { store.requestError(for: pair.id) }

    /// One or two words for the menu bar.
    var shortStatus: String {
        switch state {
        case .idle: String(localized: "Ready")
        case .working(.preview, _): String(localized: "Comparing…")
        case .working(.run, _): String(localized: "Syncing…")
        case .previewed(let preview):
            preview.total == 0 ? String(localized: "In sync") : String(localized: "\(preview.total) changes")
        case .needsApproval: String(localized: "Needs approval")
        case .finished(let result) where result.cancelled: String(localized: "Cancelled")
        case .finished(let result) where result.conflicts > 0: String(localized: "\(result.conflicts) conflicts")
        case .finished(let result) where result.failureCount > 0: String(localized: "\(result.failureCount) failed")
        case .finished: String(localized: "In sync")
        case .failed: String(localized: "Stopped")
        }
    }

    var statusText: String {
        switch state {
        case .idle:
            String(localized: "Preview to see what a sync would change. Nothing is written until you sync.")
        case .working(_, cancelling: true):
            String(localized: "Stopping…")
        case .working(.preview, _):
            String(localized: "Comparing folders…")
        case .working(.run, _):
            String(localized: "Syncing…")
        case .previewed(let preview):
            preview.total == 0
                ? String(localized: "Folders are in sync.")
                : Self.summary(preview)
        case .needsApproval(let preview):
            String(localized: "Nothing was changed. This sync needs approval before it replaces or removes items:")
                + " " + Self.summary(preview)
                + " " + Self.archiveCounts(preview)
        case .finished(let result):
            Self.summary(result)
        case .failed(let message):
            String(localized: "Sync stopped: \(message)")
        }
    }

    /// What approving the current request would allow. Read it when the user
    /// confirms: dismissing the dialog declines the request.
    var approval: DeletionApproval? {
        guard case .needsApproval(let preview) = state else { return nil }
        return DeletionApproval(a: preview.a.deletions, b: preview.b.deletions)
    }

    /// The approval to request when a run needs review. Hidden while the
    /// approving start is in flight, so the dialog cannot re-present.
    var approvalRequest: String? {
        guard case .needsApproval(let preview) = state,
              !store.isStarting(pair.id) else { return nil }
        return String(localized: "Approve this sync?")
            + " " + Self.summary(preview)
            + " " + Self.archiveCounts(preview)
            + " " + String(localized: "Archived items stay in .foldover/archive for 30 days.")
    }

    var rows: [ChangeRow] {
        switch state {
        case .previewed(let preview), .needsApproval(let preview):
            preview.changes.enumerated().map { index, change in
                ChangeRow(id: index, path: Self.pathText(change.path),
                          description: Self.description(change.action),
                          resolvable: Self.resolvable(change.action) ? change.path : nil)
            }
        case .finished(let result):
            result.failures.enumerated().map { index, failure in
                ChangeRow(id: index, path: Self.pathText(failure.path), description: failure.message,
                          resolvable: nil)
            }
        default:
            []
        }
    }

    /// Rows not shown because responses carry only the first changes.
    var hiddenRowsText: String? {
        let hidden: UInt64 = switch state {
        case .previewed(let preview), .needsApproval(let preview): preview.total - UInt64(preview.changes.count)
        case .finished(let result): result.failureCount - UInt64(result.failures.count)
        default: 0
        }
        return hidden == 0 ? nil : String(localized: "and \(hidden) more")
    }

    var archiveNote: String? {
        guard case .finished(let result) = state, result.archived + result.copied + result.updated > 0 else { return nil }
        return String(localized: "Replaced and removed items are in .foldover/archive/\(result.archive) inside each folder.")
    }

    func preview() async { await store.preview(pair.id) }
    func sync() async { await store.run(pair.id) }
    func cancel() async { await store.cancel(pair.id) }
    func refresh() async { await store.refresh(pair.id) }

    func approve(_ approval: DeletionApproval) async {
        await store.run(pair.id, approvedA: approval.a, approvedB: approval.b)
    }

    func declineApproval() { store.declineApproval(pair.id) }

    /// Keep this folder's version; the other is archived by the next sync.
    func keep(_ side: FolderSide, for row: ChangeRow) async {
        guard let path = row.resolvable else { return }
        await store.resolve(pair.id, path: path, keep: side)
    }

    /// A file or link conflict; a file-versus-folder clash needs manual repair.
    static func resolvable(_ action: ChangeAction) -> Bool {
        switch action {
        case .conflict(.bothChanged), .conflict(.deletedVersusEdited), .conflict(.metadataOnly): true
        default: false
        }
    }

    /// The per-folder counts approval accepts.
    private static func archiveCounts(_ preview: PreviewSummary) -> String {
        String(localized: "Archive \(preview.a.deletions) items in \(folderName(.a)) and \(preview.b.deletions) in \(folderName(.b)).")
    }

    static func summary(_ preview: PreviewSummary) -> String {
        var parts = [String]()
        if preview.copies > 0 { parts.append(String(localized: "\(preview.copies) to copy")) }
        if preview.updates > 0 { parts.append(String(localized: "\(preview.updates) metadata updates")) }
        if preview.archives > 0 { parts.append(String(localized: "\(preview.archives) to archive")) }
        if preview.conflicts > 0 { parts.append(String(localized: "\(preview.conflicts) conflicts")) }
        if preview.blocked > 0 { parts.append(String(localized: "\(preview.blocked) waiting on conflicts")) }
        if preview.kept > 0 { parts.append(String(localized: "\(preview.kept) kept in the backup")) }
        return parts.joined(separator: ", ") + "."
    }

    static func summary(_ result: RunSummary) -> String {
        var text = result.cancelled
            ? String(localized: "Sync cancelled after copying \(result.copied), updating \(result.updated) and archiving \(result.archived).")
            : String(localized: "Synced: copied \(result.copied), updated \(result.updated), archived \(result.archived).")
        if result.conflicts > 0 {
            text += " " + String(localized: "\(result.conflicts) conflicts were left untouched.")
        }
        if result.failureCount > 0 {
            text += " " + String(localized: "\(result.failureCount) items failed and will be retried next time.")
        }
        if result.recovered > 0 {
            text += " " + String(localized: "Recovered \(result.recovered) items left by an interrupted sync.")
        }
        return text
    }

    static func description(_ action: ChangeAction) -> String {
        switch action {
        case .copy(let to): String(localized: "Copy to \(folderName(to))")
        case .update(let to): String(localized: "Update metadata in \(folderName(to))")
        case .archive(let on): String(localized: "Archive in \(folderName(on))")
        case .conflict(.bothChanged): String(localized: "Conflict: changed in both folders")
        case .conflict(.deletedVersusEdited): String(localized: "Conflict: deleted in one folder, changed in the other")
        case .conflict(.typeMismatch): String(localized: "Conflict: a file in one folder, a folder in the other")
        case .conflict(.metadataOnly): String(localized: "Conflict: different metadata in both folders")
        case .blocked: String(localized: "Waiting on a conflict in an enclosing folder")
        case .keep: String(localized: "Kept in the backup")
        }
    }

    static func folderName(_ side: FolderSide) -> String {
        switch side {
        case .a: String(localized: "Folder A")
        case .b: String(localized: "Folder B")
        }
    }

    static func pathText(_ path: Data) -> String {
        String(data: path, encoding: .utf8) ?? String(decoding: path, as: UTF8.self)
    }
}
