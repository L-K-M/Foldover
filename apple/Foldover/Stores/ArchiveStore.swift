import FoldoverCore
import Foundation
import Observation

/// One run's folder in a side's archive.
struct ArchiveRun: Equatable, Identifiable, Sendable {
    let name: String
    let started: Date

    var id: String { name }
}

/// A file or link a run archived, at its original path.
struct ArchiveItem: Equatable, Identifiable, Sendable {
    let path: Data
    let size: UInt64

    var id: Data { path }

    /// Whether the item's path contains `text`, case-insensitively.
    func matches(_ text: String) -> Bool {
        String(decoding: path, as: UTF8.self).range(of: text, options: .caseInsensitive) != nil
    }
}

struct ArchiveRuns: Equatable, Sendable {
    /// Newest first.
    let runs: [ArchiveRun]
    let more: Bool
}

struct ArchivePage: Equatable, Sendable {
    /// In path order.
    let items: [ArchiveItem]
    let more: Bool
}

/// What putting back a whole run did.
struct RunRestore: Equatable, Sendable {
    let restored: UInt64
    /// Put beside a newer entry as "(restored)".
    let beside: UInt64
    let failures: UInt64
}

protocol ArchiveClient: Sendable {
    func runs(_ id: String, side: FolderSide) async throws -> ArchiveRuns
    func items(_ id: String, side: FolderSide, run: String, after: Data?) async throws -> ArchivePage
    /// Returns where the item went.
    func putBack(_ id: String, side: FolderSide, run: String, path: Data) async throws -> Data
    func putBackAll(_ id: String, side: FolderSide, run: String) async throws -> RunRestore
}

// Keep generated async values inside one actor; only native Sendable records leave it.
private actor CoreArchiveClient: ArchiveClient {
    func runs(_ id: String, side: FolderSide) async throws -> ArchiveRuns {
        let list = try await FoldoverCore.archiveRuns(id: id, side: coreSide(side))
        return ArchiveRuns(
            runs: list.runs.map {
                ArchiveRun(name: $0.name, started: Date(timeIntervalSince1970: Double($0.startedMs) / 1000))
            },
            more: list.more
        )
    }

    func items(_ id: String, side: FolderSide, run: String, after: Data?) async throws -> ArchivePage {
        let page = try await FoldoverCore.archiveItems(id: id, side: coreSide(side), run: run, after: after)
        return ArchivePage(items: page.items.map { ArchiveItem(path: $0.path, size: $0.size) }, more: page.more)
    }

    func putBack(_ id: String, side: FolderSide, run: String, path: Data) async throws -> Data {
        try await FoldoverCore.archivePutBack(id: id, side: coreSide(side), run: run, path: path)
    }

    func putBackAll(_ id: String, side: FolderSide, run: String) async throws -> RunRestore {
        let summary = try await FoldoverCore.archivePutBackAll(id: id, side: coreSide(side), run: run)
        return RunRestore(restored: summary.restored, beside: summary.beside, failures: summary.failureCount)
    }

    private func coreSide(_ side: FolderSide) -> SyncSide {
        switch side {
        case .a: .left
        case .b: .right
        }
    }
}

/// Browses one side's archive at a time and puts items back. Responses
/// for a pair, side, run or item listing the user has left are dropped.
@Observable
@MainActor
final class ArchiveStore {
    private let client: any ArchiveClient
    private var pairID: String?
    private(set) var side: FolderSide = .a
    private(set) var runs: [ArchiveRun] = []
    private(set) var hasOlderRuns = false
    private(set) var run: String?
    private(set) var items: [ArchiveItem] = []
    private(set) var hasMoreItems = false
    private(set) var isLoading = false
    private(set) var error: String?
    /// Where the last put-back went.
    private(set) var placed: Data?
    /// How many items the last put-back batch returned; `placed` is the
    /// last one's new place.
    private(set) var placedCount = 0
    /// A put-back batch is under way. Another call returns at once
    /// rather than queueing; the view model disables Put Back meanwhile.
    private(set) var isPuttingBack = false
    /// What the last whole-run put-back did.
    private(set) var restore: RunRestore?
    /// The text narrowing the listed items. It never refetches a page that
    /// is already loaded.
    private(set) var searchText = ""
    /// A search is loading pages looking for a match.
    private(set) var isSearching = false
    /// Bumped whenever the item list starts over, so a page asked for
    /// before, say, Put Back All never lands in the fresh list.
    private var listing = 0
    /// The listing a page is loading for; one page at a time, so a second
    /// Show More cannot fetch the same page twice.
    private var loadingListing: Int?
    /// A page is being fetched for the current listing.
    private var fetchingPage = false
    /// Pages one search loads before it stops and leaves Keep Searching to
    /// finish, so a name that is not there costs a bounded walk.
    private static let searchPages = 50

    init(client: any ArchiveClient = CoreArchiveClient()) {
        self.client = client
    }

    /// A store on the same client with fresh state, so each archive
    /// window browses on its own.
    func sibling() -> ArchiveStore { ArchiveStore(client: client) }

    /// Show `side`'s runs and the newest run's items.
    func open(_ pairID: String, side: FolderSide) async {
        self.pairID = pairID
        self.side = side
        listing += 1
        runs = []
        hasOlderRuns = false
        run = nil
        items = []
        hasMoreItems = false
        searchText = ""
        error = nil
        placed = nil
        placedCount = 0
        isLoading = true
        defer { isLoading = false }
        do {
            let list = try await client.runs(pairID, side: side)
            guard self.pairID == pairID, self.side == side else { return }
            runs = list.runs
            hasOlderRuns = list.more
        } catch {
            self.error = PairStore.message(for: error)
            return
        }
        if let newest = runs.first { await select(newest.name) }
    }

    func select(_ run: String) async {
        self.run = run
        listing += 1
        items = []
        hasMoreItems = false
        searchText = ""
        placed = nil
        placedCount = 0
        restore = nil
        await loadItems(after: nil)
    }

    /// The loaded items the search narrows to; all of them when it is empty.
    var matching: [ArchiveItem] {
        let text = searchText.trimmingCharacters(in: .whitespaces)
        guard !text.isEmpty else { return items }
        return items.filter { $0.matches(text) }
    }

    /// Set the text narrowing the listed items. Kept apart from the walk so a
    /// keystroke updates what is shown in order, whichever task then loads
    /// the pages.
    func setSearchText(_ text: String) {
        searchText = text
    }

    /// Keep loading pages while nothing matches the current text. The items
    /// are in path order, so this costs the same as pressing Show More until
    /// something appears, and stops after `searchPages` pages so a name that
    /// is not there does not walk the whole run on its own. A text that
    /// changes under a walk is walked again, so the empty state always stands
    /// for the newest text.
    func search() async {
        while !Task.isCancelled {
            guard !isSearching else { return }
            isSearching = true
            let walked = searchText
            for _ in 0..<Self.searchPages
                where !searchText.trimmingCharacters(in: .whitespaces).isEmpty
                    && matching.isEmpty {
                // A page is already loading for this listing; wait for it and
                // re-check rather than giving up on the whole query and
                // reporting a match-free run that was never searched.
                while fetchingPage, !Task.isCancelled {
                    try? await Task.sleep(for: .milliseconds(10))
                }
                guard !Task.isCancelled, hasMoreItems, let last = items.last else { break }
                await loadItems(after: last.path)
                // A failed page ends the walk; asking again repeats the
                // same request while its error sits on screen.
                guard error == nil else { break }
            }
            isSearching = false
            // A keystroke that landed mid-walk was turned away by the guard;
            // walk again so the newest text is the one the empty state stands
            // for. A walk that found something, or whose text did not change,
            // is done.
            if !matching.isEmpty || searchText == walked { return }
        }
    }

    /// Whether the listed items have more pages a search could still walk,
    /// so "no match yet" is not the same as "not in this run".
    var canKeepSearching: Bool {
        let text = searchText.trimmingCharacters(in: .whitespaces)
        return !text.isEmpty && matching.isEmpty && hasMoreItems
    }

    /// Put back everything the selected run archived, then list what is left.
    /// Share the item-restore reservation so a run cannot be restored twice.
    func putBackAll() async {
        guard !isPuttingBack, let pairID, let run else { return }
        let side = side
        isPuttingBack = true
        defer { isPuttingBack = false }
        error = nil
        do {
            let restore = try await client.putBackAll(pairID, side: side, run: run)
            guard self.pairID == pairID, self.side == side, self.run == run else { return }
            await select(run)
            self.restore = restore
        } catch {
            self.error = PairStore.message(for: error)
        }
    }

    func loadMore() async {
        guard hasMoreItems, !fetchingPage, let last = items.last else { return }
        await loadItems(after: last.path)
    }

    func putBack(_ item: ArchiveItem) async { await putBack([item]) }

    /// Put back `items` from the selected run, one at a time. The batch
    /// belongs to the pair, side and run it started in: it stops as soon
    /// as another is chosen, since a later item's path may name another
    /// run's copy, and at the first failure, so its error stays shown.
    func putBack(_ items: [ArchiveItem]) async {
        guard !isPuttingBack, !items.isEmpty, let pairID, let run else { return }
        let side = side
        isPuttingBack = true
        defer { isPuttingBack = false }
        error = nil
        placed = nil
        placedCount = 0
        for item in items {
            guard self.pairID == pairID, self.side == side, self.run == run else { return }
            do {
                let path = try await client.putBack(pairID, side: side, run: run, path: item.path)
                guard self.pairID == pairID, self.side == side, self.run == run else { return }
                self.items.removeAll { $0 == item }
                placed = path
                placedCount += 1
            } catch {
                self.error = PairStore.message(for: error)
                return
            }
        }
    }

    private func loadItems(after: Data?) async {
        guard let pairID, let run else { return }
        let side = side
        let ticket = listing
        loadingListing = ticket
        fetchingPage = true
        error = nil
        isLoading = true
        defer {
            if loadingListing == ticket {
                loadingListing = nil
                fetchingPage = false
                isLoading = false
            }
        }
        do {
            let page = try await client.items(pairID, side: side, run: run, after: after)
            guard ticket == listing else { return }
            items += page.items
            hasMoreItems = page.more
        } catch {
            self.error = PairStore.message(for: error)
        }
    }
}
