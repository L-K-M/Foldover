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

protocol ArchiveClient: Sendable {
    func runs(_ id: String, side: FolderSide) async throws -> ArchiveRuns
    func items(_ id: String, side: FolderSide, run: String, after: Data?) async throws -> ArchivePage
    /// Returns where the item went.
    func putBack(_ id: String, side: FolderSide, run: String, path: Data) async throws -> Data
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

    private func coreSide(_ side: FolderSide) -> SyncSide {
        switch side {
        case .a: .left
        case .b: .right
        }
    }
}

/// Browses one side's archive at a time and puts items back. Responses
/// for a pair, side or run the user has left are dropped.
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

    init(client: any ArchiveClient = CoreArchiveClient()) {
        self.client = client
    }

    /// Show `side`'s runs and the newest run's items.
    func open(_ pairID: String, side: FolderSide) async {
        self.pairID = pairID
        self.side = side
        runs = []
        hasOlderRuns = false
        run = nil
        items = []
        hasMoreItems = false
        error = nil
        placed = nil
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
        items = []
        hasMoreItems = false
        placed = nil
        await loadItems(after: nil)
    }

    func loadMore() async {
        guard hasMoreItems, let last = items.last else { return }
        await loadItems(after: last.path)
    }

    func putBack(_ item: ArchiveItem) async {
        guard let pairID, let run else { return }
        let side = side
        error = nil
        placed = nil
        do {
            let path = try await client.putBack(pairID, side: side, run: run, path: item.path)
            guard self.pairID == pairID, self.side == side, self.run == run else { return }
            items.removeAll { $0 == item }
            placed = path
        } catch {
            self.error = PairStore.message(for: error)
        }
    }

    private func loadItems(after: Data?) async {
        guard let pairID, let run else { return }
        let side = side
        error = nil
        isLoading = true
        defer { isLoading = false }
        do {
            let page = try await client.items(pairID, side: side, run: run, after: after)
            guard self.pairID == pairID, self.side == side, self.run == run else { return }
            items += page.items
            hasMoreItems = page.more
        } catch {
            self.error = PairStore.message(for: error)
        }
    }
}
