import Foundation

/// The preview a change row belongs to. Review choices echo its token
/// to the agent, and edits that must not outrun it compare its pair
/// revision.
struct ReviewContext: Equatable, Sendable {
    let token: String
    let pairRevision: UInt64
}

/// One planned or failed change, ready to render.
struct ChangeRow: Identifiable, Equatable {
    let id: Int
    let path: String
    let description: String
    /// Raw path bytes of a conflict a person can resolve by keeping a side.
    let resolvable: Data?
    /// Both versions can be kept: the conflict changed a file in both folders.
    var keepsBoth = false
    /// Raw path bytes, for skipping this item from now on.
    var skippable: Data?
    /// Raw path bytes of a previewed change, for Skip Once.
    var item: Data?
    /// The side to keep to copy this change the other way: the folder it
    /// would have changed.
    var otherWay: FolderSide?
    /// The menu title for that choice, e.g. "Restore to Folder A".
    var otherWayTitle: String?
    /// The preview the row came from; nil for run results, which no
    /// review can act on.
    var review: ReviewContext?
}

/// One finished run, ready to render.
struct HistoryRow: Identifiable, Equatable {
    let id: Int
    let when: String
    let summary: String
}

/// Everything the approval dialog was presented with: which pair, which
/// exact plan the deletion counts belong to, and the wording shown.
/// Approving sends this capture back, so a state change between
/// presentation and tap can never approve a different plan.
struct ApprovalRequest: Equatable, Sendable, Identifiable {
    let pairID: String
    let pairRevision: UInt64
    let reviewToken: String
    let approvedA: UInt64
    let approvedB: UInt64
    let message: String
    var id: String { "\(pairID)-\(reviewToken)" }
}

/// Preview and run controls for the selected pair: strings and actions only.
@MainActor
struct PairSyncViewModel {
    let pair: SyncPair
    let store: SyncStore
    let pairs: PairStore
    let agentConnected: Bool

    private var state: PairSyncState { store.state(for: pair.id) }

    /// The shown plan still describes this pair's configuration. An
    /// older preview is information, not something to review: its folder
    /// labels and review tokens would act on roots that changed since.
    private var reviewable: PreviewSummary? {
        switch state {
        case .previewed(let preview), .needsApproval(let preview):
            preview.pairRevision == pair.revision ? preview : nil
        default: nil
        }
    }

    var isWorking: Bool {
        if case .working = state { return true }
        return false
    }
    /// A conflict choice is on the wire; the pair is busy but the agent
    /// runs no job a Cancel button could stop.
    var isResolving: Bool { store.isResolving(pair.id) }
    var canStart: Bool { agentConnected && !isWorking && !store.isBusy(pair.id) }
    /// Whether Sync Now can go. From a shown preview it is the reviewed
    /// run of that plan, so the preview must still match the pair's
    /// revision; an obsolete one must be previewed again. No preview at
    /// all is a deliberate unreviewed run under the approval fuse.
    var canSync: Bool {
        guard canStart else { return false }
        switch state {
        case .previewed, .needsApproval: return reviewable != nil
        default: return true
        }
    }
    /// The shown result is a sync that copied or archived something, which
    /// Undo can reverse while the agent still holds its journal; metadata
    /// updates alone cannot be undone.
    var canUndo: Bool {
        guard canStart, case .finished(let result) = state else { return false }
        return result.copied + result.archived > 0
    }
    var canCancel: Bool {
        if case .working(_, let cancelling, _) = state { return !cancelling }
        return false
    }
    var requestError: String? { store.requestError(for: pair.id) }

    /// One or two words for the menu bar.
    var shortStatus: String {
        if isResolving { return String(localized: "Resolving") }
        return switch state {
        case .idle: String(localized: "Ready")
        case .working(.preview, _, _): String(localized: "Comparing…")
        case .working(.run, _, _): String(localized: "Syncing…")
        case .working(.undo, _, _): String(localized: "Undoing…")
        case .previewed(let preview):
            reviewable == nil ? String(localized: "Preview again")
                : preview.total == 0 ? String(localized: "In sync") : String(localized: "\(preview.total) changes")
        case .needsApproval:
            reviewable == nil ? String(localized: "Preview again") : String(localized: "Needs approval")
        case .finished(let result) where result.cancelled: String(localized: "Cancelled")
        case .finished(let result) where result.conflicts > 0: String(localized: "\(result.conflicts) conflicts")
        case .finished(let result) where result.failureCount > 0: String(localized: "\(result.failureCount) failed")
        case .finished: String(localized: "In sync")
        case .undone(let result) where result.cancelled: String(localized: "Undo stopped")
        case .undone(let result) where result.notUndoneCount > 0: String(localized: "Partly undone")
        case .undone: String(localized: "Undone")
        case .failed: String(localized: "Stopped")
        case .waiting: String(localized: "Waiting for folder")
        }
    }

    var statusText: String {
        if isResolving { return String(localized: "Resolving the conflict…") }
        switch state {
        case .idle:
            return String(localized: "Preview to see what a sync would change. Nothing is written until you sync.")
        case .working(_, cancelling: true, _):
            return String(localized: "Stopping…")
        case .working(.preview, _, let progress):
            return Self.comparing(progress)
        case .working(.run, _, let progress) where progress.changes > 0:
            return Self.applying(progress)
        case .working(.run, _, let progress):
            return Self.comparing(progress)
        case .working(.undo, _, _):
            return String(localized: "Undoing the last sync…")
        case .previewed(let preview):
            guard reviewable != nil else { return Self.outdated }
            return preview.total == 0
                ? String(localized: "Folders are in sync.")
                : Self.summary(preview)
        case .needsApproval(let preview):
            guard reviewable != nil else { return Self.outdated }
            return String(localized: "Nothing was changed. Review this sync's changes before it runs:")
                + " " + Self.summary(preview)
                + " " + Self.archiveCounts(preview)
        case .finished(let result):
            return Self.summary(result)
        case .undone(let result):
            return Self.summary(result)
        case .failed(let message):
            return String(localized: "Sync stopped: \(message)")
        case .waiting(let side):
            return String(localized: "\(Self.folderName(side)) is not connected. Automatic syncs resume when its disk or share is back.")
        }
    }

    /// How far a running job is, from 0 to 1, once there is something to
    /// measure: bytes when the plan copies any, else changes; while
    /// comparing, entries checked of those found so far.
    var progressFraction: Double? {
        guard case .working(_, _, let progress) = state else { return nil }
        if progress.bytes > 0 {
            return Double(progress.copied) / Double(progress.bytes)
        }
        if progress.changes > 0 {
            return Double(progress.applied) / Double(progress.changes)
        }
        guard progress.listed > 0 else { return nil }
        return Double(progress.checked) / Double(progress.listed)
    }

    static func comparing(_ progress: WorkProgress) -> String {
        guard progress.listed > 0 else { return String(localized: "Comparing folders…") }
        return String(localized: "Comparing folders… \(progress.checked) of \(progress.listed) items checked")
    }

    static func applying(_ progress: WorkProgress) -> String {
        var text = String(localized: "Syncing… \(progress.applied) of \(progress.changes) changes")
        if progress.bytes > 0 {
            text += " · " + String(localized: "\(byteText(progress.copied)) of \(byteText(progress.bytes))")
        }
        return text
    }

    /// A plan made for another pair revision is shown as information,
    /// never offered for review.
    private static var outdated: String {
        String(localized: "The pair changed since that preview; preview again.")
    }

    /// The approval to request when a run needs review, captured with
    /// the plan it names. Hidden while any command is in flight, so the
    /// dialog cannot re-present, and when the plan's pair revision is
    /// obsolete.
    var approvalRequest: ApprovalRequest? {
        guard case .needsApproval(let preview) = state,
              preview.pairRevision == pair.revision,
              !store.isBusy(pair.id) else { return nil }
        return ApprovalRequest(
            pairID: pair.id, pairRevision: preview.pairRevision,
            reviewToken: preview.reviewToken,
            approvedA: preview.a.deletions, approvedB: preview.b.deletions,
            message: String(localized: "Approve this sync?")
                + " " + Self.summary(preview)
                + " " + Self.archiveCounts(preview)
                + " " + String(localized: "Archived items stay in .foldover/archive for 30 days.")
        )
    }

    /// The kind of changes listed; nil lists all.
    var changeFilter: ChangeFilter? { store.listing(for: pair.id)?.filter }
    var showsChangeFilter: Bool { reviewable.map { $0.total > 0 } ?? false }
    var canLoadMoreChanges: Bool {
        store.listing(for: pair.id)?.more ?? (hiddenRowsText != nil && reviewable != nil)
    }

    func showChanges(_ filter: ChangeFilter?) async { await store.showChanges(pair.id, filter: filter) }

    func loadMoreChanges() async {
        if store.listing(for: pair.id) == nil {
            await store.showChanges(pair.id, filter: nil)
        }
        await store.loadMoreChanges(pair.id)
    }

    static func title(_ filter: ChangeFilter?) -> String {
        switch filter {
        case nil: String(localized: "All")
        case .copies: String(localized: "Copies")
        case .updates: String(localized: "Updates")
        case .archives: String(localized: "Archives")
        case .conflicts: String(localized: "Conflicts")
        }
    }

    var rows: [ChangeRow] {
        if let preview = reviewable {
            let review = ReviewContext(token: preview.reviewToken,
                                       pairRevision: preview.pairRevision)
            return (store.listing(for: pair.id)?.changes ?? preview.changes)
                .enumerated().map { index, change in
                    ChangeRow(id: index, path: Self.pathText(change.path),
                              description: Self.description(change),
                              resolvable: Self.resolvable(change.action) ? change.path : nil,
                              keepsBoth: change.action == .conflict(.bothChanged),
                              skippable: PairStore.skipRule(for: change.path) == nil ? nil : change.path,
                              item: Self.choosable(change.action) ? change.path : nil,
                              otherWay: Self.otherWay(change.action),
                              otherWayTitle: Self.otherWayTitle(change.action),
                              review: review)
                }
        }
        let failures: [RunFailure] = switch state {
        case .finished(let result): result.failures
        case .undone(let result): result.notUndone
        default: []
        }
        return failures.enumerated().map { index, failure in
            ChangeRow(id: index, path: Self.pathText(failure.path), description: failure.message,
                      resolvable: nil)
        }
    }

    /// Rows not shown because responses carry only the first changes.
    var hiddenRowsText: String? {
        if let listing = store.listing(for: pair.id) {
            return listing.more ? String(localized: "More changes follow") : nil
        }
        let hidden: UInt64
        if let preview = reviewable {
            hidden = preview.total - UInt64(preview.changes.count)
        } else if case .finished(let result) = state {
            hidden = result.failureCount - UInt64(result.failures.count)
        } else if case .undone(let result) = state {
            hidden = result.notUndoneCount - UInt64(result.notUndone.count)
        } else {
            hidden = 0
        }
        return hidden == 0 ? nil : String(localized: "and \(hidden) more")
    }

    /// The newest finished runs.
    var historyRows: [HistoryRow] {
        store.history(for: pair.id).prefix(Self.historyRowLimit).enumerated().map { index, run in
            let when = run.started.formatted(date: .abbreviated, time: .shortened)
            let trigger = switch run.trigger {
            case .scheduled: String(localized: "automatic")
            case .manual: String(localized: "manual")
            case .undo: String(localized: "undo")
            }
            return HistoryRow(id: index, when: "\(when) · \(trigger)", summary: Self.summary(run))
        }
    }

    private static let historyRowLimit = 10

    var archiveNote: String? {
        guard case .finished(let result) = state, result.archived + result.copied + result.updated > 0 else { return nil }
        return String(localized: "Replaced and removed items are in .foldover/archive/\(result.archive) inside each folder.")
    }

    func preview() async { await store.preview(pair.id) }
    /// Run the shown plan, or an unreviewed sync when none is shown. A
    /// preview made for another pair revision never falls back to a run
    /// the changed pair did not ask for; preview again first.
    func sync() async {
        guard canSync else { return }
        await store.run(pair.id, reviewToken: reviewable?.reviewToken)
    }
    func cancel() async { await store.cancel(pair.id) }
    /// Reverse the shown sync. Automatic syncs then wait for a person.
    func undo() async {
        guard canUndo else { return }
        await store.undo(pair.id)
    }
    func refresh() async { await store.refresh(pair.id) }

    /// Approve the request the dialog was presented with: its pair, its
    /// plan's fingerprint and the deletion counts reviewed then.
    func approve(_ request: ApprovalRequest) async {
        await store.run(request.pairID, approvedA: request.approvedA,
                        approvedB: request.approvedB, reviewToken: request.reviewToken)
    }

    func declineApproval() { store.declineApproval(pair.id) }

    /// Keep this folder's version; the other is archived by the next sync.
    /// The choice echoes the token of the preview the row came from.
    func keep(_ side: FolderSide, for row: ChangeRow) async {
        guard let path = row.resolvable, let review = row.review else { return }
        await store.resolve(pair.id, path: path, keep: side, reviewToken: review.token)
    }

    /// Copy a previewed change the other way: keep the version in the
    /// folder it would have changed (PLAN.md §3.8 per-item override).
    func copyOtherWay(_ row: ChangeRow) async {
        guard let path = row.item, let side = row.otherWay, let review = row.review else { return }
        await store.resolve(pair.id, path: path, keep: side, reviewToken: review.token)
    }

    /// Leave a previewed change alone in the next sync.
    func skipOnce(_ row: ChangeRow) async {
        guard let path = row.item, let review = row.review else { return }
        await store.skipOnce(pair.id, path: path, reviewToken: review.token)
    }

    /// The menu title for copying a change the other way: an archive
    /// becomes a restore into the folder it went missing from, and a new
    /// item's copy an archive where it was created.
    static func otherWayTitle(_ action: ChangeAction) -> String? {
        switch action {
        case .archive(let on): String(localized: "Restore to \(folderName(on == .a ? .b : .a))")
        case .copy(let to): String(localized: "Archive in \(folderName(to == .a ? .b : .a))")
        case .update: String(localized: "Copy the Other Way")
        case .conflict, .blocked, .keep: nil
        }
    }

    /// Changes a person can steer; blocked items wait for the conflict
    /// above them, kept ones change nothing, and a name clash needs a
    /// rename.
    static func choosable(_ action: ChangeAction) -> Bool {
        switch action {
        case .blocked, .keep, .conflict(.nameClash): false
        default: true
        }
    }

    /// The folder a copy, update or archive would change.
    static func otherWay(_ action: ChangeAction) -> FolderSide? {
        switch action {
        case .copy(let to), .update(let to): to
        case .archive(let on): on
        case .conflict, .blocked, .keep: nil
        }
    }

    /// Skip this item from now on with a rule in the pair's filters, then
    /// preview again. The edit applies only if the pair still has the
    /// revision the preview was made for.
    func skipAlways(_ row: ChangeRow) async {
        guard let path = row.skippable, let review = row.review else { return }
        do {
            try await pairs.skip(path, inPairWithID: pair.id,
                                 expectedRevision: review.pairRevision)
        } catch {
            store.report(PairStore.message(for: error), for: pair.id)
            return
        }
        await store.preview(pair.id)
    }

    /// Keep both versions; Folder B's is saved beside the item.
    func keepBoth(for row: ChangeRow) async {
        guard row.keepsBoth, let path = row.resolvable, let review = row.review else { return }
        await store.keepBoth(pair.id, path: path, reviewToken: review.token)
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
        if preview.copies > 0 {
            parts.append(preview.copyBytes > 0
                ? String(localized: "\(preview.copies) to copy (\(byteText(preview.copyBytes)))")
                : String(localized: "\(preview.copies) to copy"))
        }
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

    static func summary(_ result: UndoSummary) -> String {
        var text = result.cancelled
            ? String(localized: "Undo stopped after putting back \(result.restored) and archiving \(result.removed) items.")
            : String(localized: "Undid the last sync: put back \(result.restored), archived \(result.removed) it had created.")
        if result.beside > 0 {
            text += " " + String(localized: "\(result.beside) went beside newer versions as \"(restored)\".")
        }
        if result.notUndoneCount > 0 {
            text += " " + String(localized: "\(result.notUndoneCount) items were left as they are.")
        }
        text += " " + String(localized: "Automatic syncs wait until you sync again; preview to see what syncing would redo.")
        return text
    }

    static func summary(_ run: RunRecord) -> String {
        if run.trigger == .undo {
            switch run.end {
            case .failed:
                return String(localized: "Undo stopped: \(run.message ?? String(localized: "unknown error"))")
            case .cancelled:
                return String(localized: "Undo stopped: put back \(run.recovered), archived \(run.archived).")
            default:
                return String(localized: "Undo: put back \(run.recovered), archived \(run.archived).")
            }
        }
        switch run.end {
        case .cancelled:
            return String(localized: "Cancelled after copying \(run.copied), updating \(run.updated) and archiving \(run.archived).")
        case .needsApproval:
            return String(localized: "Stopped before changing anything: too many items to archive without approval.")
        case .failed:
            return String(localized: "Stopped: \(run.message ?? String(localized: "unknown error"))")
        case .completed:
            break
        }
        var text = run.copied + run.updated + run.archived == 0
            ? String(localized: "No changes.")
            : String(localized: "Copied \(run.copied), updated \(run.updated), archived \(run.archived).")
        if run.conflicts > 0 { text += " " + String(localized: "\(run.conflicts) conflicts.") }
        if run.failures > 0 { text += " " + String(localized: "\(run.failures) failed.") }
        return text
    }

    static func description(_ change: PlannedChange) -> String {
        let action = description(change.action)
        return change.bytes > 0 ? "\(action) · \(byteText(change.bytes))" : action
    }

    static func byteText(_ bytes: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file)
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
        case .conflict(.nameClash): String(localized: "Conflict: another item's name differs only in case or accents; rename one")
        case .blocked: String(localized: "Waiting on a conflict in an enclosing folder")
        case .keep: String(localized: "Left as it is")
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
