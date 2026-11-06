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
    func runs(_ id: String, revision: UInt64, side: FolderSide) async throws -> ArchiveRuns
    func items(_ id: String, revision: UInt64, side: FolderSide, run: String,
               after: Data?) async throws -> ArchivePage
    /// Returns where the item went.
    func putBack(_ id: String, revision: UInt64, side: FolderSide, run: String,
                 path: Data) async throws -> Data
    func putBackAll(_ id: String, revision: UInt64, side: FolderSide,
                    run: String) async throws -> RunRestore
}

// Keep generated async values inside one actor; only native Sendable records leave it.
private actor CoreArchiveClient: ArchiveClient {
    func runs(_ id: String, revision: UInt64, side: FolderSide) async throws -> ArchiveRuns {
        let list = try await FoldoverCore.archiveRuns(id: id, revision: revision, side: coreSide(side))
        return ArchiveRuns(
            runs: list.runs.map {
                ArchiveRun(name: $0.name, started: Date(timeIntervalSince1970: Double($0.startedMs) / 1000))
            },
            more: list.more
        )
    }

    func items(_ id: String, revision: UInt64, side: FolderSide, run: String,
               after: Data?) async throws -> ArchivePage {
        let page = try await FoldoverCore.archiveItems(
            id: id, revision: revision, side: coreSide(side), run: run, after: after
        )
        return ArchivePage(items: page.items.map { ArchiveItem(path: $0.path, size: $0.size) }, more: page.more)
    }

    func putBack(_ id: String, revision: UInt64, side: FolderSide, run: String,
                 path: Data) async throws -> Data {
        try await FoldoverCore.archivePutBack(
            id: id, revision: revision, side: coreSide(side), run: run, path: path
        )
    }

    func putBackAll(_ id: String, revision: UInt64, side: FolderSide,
                    run: String) async throws -> RunRestore {
        let summary = try await FoldoverCore.archivePutBackAll(
            id: id, revision: revision, side: coreSide(side), run: run
        )
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
    private var pairRevision: UInt64?
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
    /// The last returned path survives removal of restored rows.
    private var nextPageAfter: Data?
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
    func open(_ pairID: String, revision: UInt64, side: FolderSide) async {
        self.pairID = pairID
        pairRevision = revision
        self.side = side
        resetListing()
        runs = []
        hasOlderRuns = false
        run = nil
        let ticket = listing
        loadingListing = ticket
        isLoading = true
        defer {
            if loadingListing == ticket {
                loadingListing = nil
                isLoading = false
            }
        }
        do {
            let list = try await client.runs(pairID, revision: revision, side: side)
            guard ticket == listing else { return }
            runs = list.runs
            hasOlderRuns = list.more
        } catch {
            guard ticket == listing else { return }
            self.error = PairStore.message(for: error)
            return
        }
        if let newest = runs.first { await select(newest.name) }
    }

    func select(_ run: String) async {
        self.run = run
        resetListing()
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
        guard !isSearching else { return }
        let ticket = listing
        isSearching = true
        defer {
            if ticket == listing { isSearching = false }
        }

        while !Task.isCancelled, ticket == listing {
            let walked = searchText
            for _ in 0..<Self.searchPages
                where !searchText.trimmingCharacters(in: .whitespaces).isEmpty
                    && matching.isEmpty {
                // Wait only for this listing; a navigation starts its own walk.
                while fetchingPage, !Task.isCancelled, ticket == listing {
                    try? await Task.sleep(for: .milliseconds(10))
                }
                guard !Task.isCancelled, ticket == listing, hasMoreItems else { break }
                await loadItems(after: nextPageAfter)
                // A failed page ends the walk and leaves its error available.
                guard ticket == listing, error == nil else { break }
            }
            guard ticket == listing else { return }
            // A keystroke during the walk must still get its own search.
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
        guard !isPuttingBack, let pairID, let pairRevision, let run else { return }
        let side = side
        let ticket = listing
        isPuttingBack = true
        defer { isPuttingBack = false }
        error = nil
        do {
            let restore = try await client.putBackAll(
                pairID, revision: pairRevision, side: side, run: run
            )
            guard ticket == listing else { return }
            resetListing()
            let reload = listing
            await loadItems(after: nil)
            guard reload == listing else { return }
            self.restore = restore
        } catch {
            guard ticket == listing else { return }
            self.error = PairStore.message(for: error)
        }
    }

    func loadMore() async {
        guard hasMoreItems, !fetchingPage else { return }
        await loadItems(after: nextPageAfter)
    }

    func putBack(_ item: ArchiveItem) async { await putBack([item]) }

    /// Put back `items` from the selected run, one at a time. The batch
    /// belongs to the pair, side and run it started in: it stops as soon
    /// as another is chosen, since a later item's path may name another
    /// run's copy, and at the first failure, so its error stays shown.
    func putBack(_ items: [ArchiveItem]) async {
        guard !isPuttingBack, !items.isEmpty, let pairID, let pairRevision, let run else { return }
        let side = side
        let ticket = listing
        isPuttingBack = true
        defer { isPuttingBack = false }
        error = nil
        placed = nil
        placedCount = 0
        for item in items {
            guard ticket == listing else { return }
            do {
                let path = try await client.putBack(
                    pairID, revision: pairRevision, side: side, run: run, path: item.path
                )
                guard ticket == listing else { return }
                self.items.removeAll { $0 == item }
                placed = path
                placedCount += 1
            } catch {
                guard ticket == listing else { return }
                self.error = PairStore.message(for: error)
                return
            }
        }
    }

    /// Invalidate every prior visit, including leaving and returning to one run.
    private func resetListing() {
        listing += 1
        loadingListing = nil
        fetchingPage = false
        isLoading = false
        isSearching = false
        items = []
        hasMoreItems = false
        nextPageAfter = nil
        searchText = ""
        error = nil
        placed = nil
        placedCount = 0
        restore = nil
    }

    private func loadItems(after: Data?) async {
        guard let pairID, let pairRevision, let run else { return }
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
            let page = try await client.items(
                pairID, revision: pairRevision, side: side, run: run, after: after
            )
            guard ticket == listing else { return }
            items += page.items
            nextPageAfter = page.items.last?.path ?? after
            hasMoreItems = page.more
        } catch {
            guard ticket == listing else { return }
            self.error = PairStore.message(for: error)
        }
    }
}
