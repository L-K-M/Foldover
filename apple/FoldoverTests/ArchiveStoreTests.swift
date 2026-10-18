import Foundation
import Testing
@testable import Foldover

/// One side holds a run with three items served two per page.
private actor ArchiveFixtureClient: ArchiveClient {
    private var stored = [Data("a".utf8), Data("b".utf8), Data("c".utf8)]
    private(set) var sides: [FolderSide] = []

    func runs(_ id: String, side: FolderSide) async throws -> ArchiveRuns {
        sides.append(side)
        guard side == .b else { return ArchiveRuns(runs: [], more: false) }
        return ArchiveRuns(runs: [ArchiveRun(name: "new", started: Date(timeIntervalSince1970: 2)),
                                  ArchiveRun(name: "old", started: Date(timeIntervalSince1970: 1))],
                           more: true)
    }

    func items(_ id: String, side: FolderSide, run: String, after: Data?) async throws -> ArchivePage {
        let rest = stored.filter { path in after.map { $0.lexicographicallyPrecedes(path) } ?? true }
        return ArchivePage(items: rest.prefix(2).map { ArchiveItem(path: $0, size: 1) }, more: rest.count > 2)
    }

    func putBack(_ id: String, side: FolderSide, run: String, path: Data) async throws -> Data {
        stored.removeAll { $0 == path }
        return path + Data(" (restored)".utf8)
    }

    func putBackAll(_ id: String, side: FolderSide, run: String) async throws -> RunRestore {
        let count = UInt64(stored.count)
        stored.removeAll()
        return RunRestore(restored: count - 1, beside: 1, failures: 0)
    }
}

private func docs() -> SyncPair {
    SyncPair(id: "11111111111111111111111111111111", revision: 1, settings: PairSettings(
        name: "Docs", left: SelectedFolder(bytes: Data("/a".utf8)),
        right: SelectedFolder(bytes: Data("/b".utf8)), mode: .sync
    ))
}

@MainActor
@Test
func archiveListsTheNewestRunAndPagesItsItems() async {
    let store = ArchiveStore(client: ArchiveFixtureClient())
    let viewModel = ArchiveViewModel(pair: docs(), store: store, agentConnected: true)

    await viewModel.load()
    #expect(viewModel.emptyText == "Folder A has nothing archived.")

    await viewModel.show(.b)
    #expect(viewModel.runRows.map(\.id) == ["new", "old"])
    #expect(viewModel.selectedRun == "new")
    #expect(viewModel.olderRunsNote != nil)
    #expect(viewModel.itemRows.map(\.path) == ["a", "b"])
    #expect(viewModel.canLoadMore)

    await viewModel.loadMore()
    #expect(viewModel.itemRows.map(\.path) == ["a", "b", "c"])
    #expect(!viewModel.canLoadMore)
}

@MainActor
@Test
func putBackRemovesTheRowAndSaysWhereItWent() async throws {
    let store = ArchiveStore(client: ArchiveFixtureClient())
    let viewModel = ArchiveViewModel(pair: docs(), store: store, agentConnected: true)
    await viewModel.show(.b)

    let row = try #require(viewModel.itemRows.first)
    await viewModel.putBack(row)
    #expect(viewModel.itemRows.map(\.path) == ["b"])
    #expect(viewModel.placedText?.hasPrefix("Put back as a (restored).") == true)
}

@MainActor
@Test
func putBackAllEmptiesTheRunAndSummarizes() async {
    let store = ArchiveStore(client: ArchiveFixtureClient())
    let viewModel = ArchiveViewModel(pair: docs(), store: store, agentConnected: true)
    await viewModel.show(.b)
    #expect(viewModel.canPutBackAll)

    await viewModel.putBackAll()
    #expect(viewModel.itemRows.isEmpty)
    #expect(viewModel.restoreText == "Put back 3 items. 1 went beside newer versions.")
    #expect(!viewModel.canPutBackAll)
}
