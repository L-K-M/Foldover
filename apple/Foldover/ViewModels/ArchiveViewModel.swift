import Foundation

struct ArchiveRunRow: Identifiable, Equatable {
    let id: String
    let title: String
}

struct ArchiveItemRow: Identifiable, Equatable {
    let id: Data
    let path: String
    let size: String
}

/// The archive browser for one pair: strings and actions only.
@MainActor
struct ArchiveViewModel {
    let pair: SyncPair
    let store: ArchiveStore
    let agentConnected: Bool

    var title: String { String(localized: "Archive of \(pair.settings.name)") }
    var pairName: String { pair.settings.name }

    static func sideTitle(_ side: FolderSide) -> String { PairSyncViewModel.folderName(side) }
    var explanation: String {
        String(localized: "Items a sync replaced or removed stay in each folder's archive for 30 days. Put Back returns one to where it was; if something is there now, it goes beside it with \"(restored)\" in its name. The next sync copies it to the other folder.")
    }

    var side: FolderSide { store.side }
    var selectedRun: String? { store.run }
    var isLoading: Bool { store.isLoading }
    /// One put-back batch at a time; the next waits until it ends.
    var canPutBack: Bool { agentConnected && !store.isLoading && !store.isPuttingBack }
    var canLoadMore: Bool { store.hasMoreItems && !store.isLoading }
    var error: String? { store.error }

    var runRows: [ArchiveRunRow] {
        store.runs.map {
            ArchiveRunRow(id: $0.name, title: Self.runTitle($0.started))
        }
    }

    /// Relative for recent runs, with the clock time so two runs the
    /// same day stay distinct; absolute once the day is past.
    nonisolated static func runTitle(_ started: Date, now: Date = Date()) -> String {
        let age = now.timeIntervalSince(started)
        if age >= 0, age < 86_400 {
            let relative = started.formatted(.relative(presentation: .named))
            let clock = started.formatted(date: .omitted, time: .shortened)
            return "\(relative), \(clock)"
        }
        return started.formatted(date: .abbreviated, time: .standard)
    }

    var itemRows: [ArchiveItemRow] {
        store.matching.map {
            ArchiveItemRow(id: $0.path, path: PairSyncViewModel.pathText($0.path),
                           size: PairSyncViewModel.byteText($0.size))
        }
    }

    /// The text narrowing the listed items, as the change table's does; it
    /// never refetches a page that is already loaded.
    var searchText: String { store.searchText }
    /// A search is loading pages looking for a match.
    var isSearching: Bool { store.isSearching }

    /// Whether a search is narrowing the list, so an empty table is its
    /// result rather than an empty run.
    var hasSearch: Bool { !searchText.trimmingCharacters(in: .whitespaces).isEmpty }

    func search(_ text: String) async { await store.search(text) }

    /// Why a list is empty, once loading finished.
    var emptyText: String? {
        guard !store.isLoading, store.error == nil else { return nil }
        if store.runs.isEmpty {
            return String(localized: "\(PairSyncViewModel.folderName(store.side)) has nothing archived.")
        }
        if store.run != nil, store.items.isEmpty {
            return String(localized: "Nothing from this sync is left in the archive.")
        }
        return nil
    }

    var olderRunsNote: String? {
        store.hasOlderRuns ? String(localized: "Older syncs are not listed.") : nil
    }

    var canPutBackAll: Bool { canPutBack && !store.items.isEmpty }

    var putBackAllQuestion: String {
        String(localized: "Put back everything this sync archived in \(PairSyncViewModel.folderName(store.side))? Nothing is replaced: items whose place is taken go beside it with \"(restored)\" in their name.")
    }

    var restoreText: String? {
        guard let restore = store.restore else { return nil }
        var text = String(localized: "Put back \(restore.restored + restore.beside) items.")
        if restore.beside > 0 {
            text += " " + String(localized: "\(restore.beside) went beside newer versions.")
        }
        if restore.failures > 0 {
            text += " " + String(localized: "\(restore.failures) could not be put back and stay in the archive.")
        }
        return text
    }

    func putBackAll() async { await store.putBackAll() }

    var placedText: String? {
        guard let placed = store.placed else { return nil }
        return String(localized: "Put back as \(PairSyncViewModel.pathText(placed)). The next sync copies it to the other folder.")
    }

    /// What the last put-back did, for the window's status line: a
    /// whole run, several items, or where one item went.
    var statusText: String? {
        if store.isPuttingBack { return String(localized: "Putting items back…") }
        if let restoreText { return restoreText }
        guard store.placedCount > 1 else { return placedText }
        return String(localized: "Put back \(store.placedCount) items. The next sync copies them to the other folder.")
    }

    func load() async { await store.open(pair.id, side: store.side) }
    func show(_ side: FolderSide) async { await store.open(pair.id, side: side) }
    func select(_ run: String) async { await store.select(run) }
    func loadMore() async { await store.loadMore() }

    func putBack(_ row: ArchiveItemRow) async { await putBack([row]) }

    /// Put back the rows' items as one batch in the shown run; see
    /// `ArchiveStore.putBack(_:)` for where it stops.
    func putBack(_ rows: [ArchiveItemRow]) async {
        let items = rows.compactMap { row in store.items.first { $0.path == row.id } }
        await store.putBack(items)
    }
}
