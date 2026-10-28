import Foundation
import Testing
@testable import Foldover

/// One side holds a run with three items served two per page.
private actor ArchiveFixtureClient: ArchiveClient {
    private var stored = [Data("a".utf8), Data("b".utf8), Data("c".utf8)]
    private(set) var sides: [FolderSide] = []
    /// Items put back one by one, in request order.
    private(set) var putBacks: [Data] = []
    /// Requests for later pages, and the next one to hold until released.
    private(set) var pageRequests = 0
    private var holdsNextPage = false
    private var held: CheckedContinuation<Void, Never>?
    private(set) var restoreRequests = 0
    private var holdsRestore = false
    private var heldRestore: CheckedContinuation<Void, Never>?
    private var refusesRestore = false
    private var refusesNextPage = false

    func holdRestore() { holdsRestore = true }
    func refuseRestore() { refusesRestore = true }
    func refuseNextPage() { refusesNextPage = true }

    func waitForHeldRestore() async -> Bool {
        let deadline = ContinuousClock.now + .seconds(3)
        while heldRestore == nil {
            if ContinuousClock.now >= deadline { return false }
            try? await Task.sleep(for: .milliseconds(10))
        }
        return true
    }

    func releaseRestore() {
        heldRestore?.resume()
        heldRestore = nil
    }

    func holdNextPage() { holdsNextPage = true }

    func waitForHeldPage() async {
        while held == nil { await Task.yield() }
    }

    func releasePage() {
        held?.resume()
        held = nil
    }

    func runs(_ id: String, side: FolderSide) async throws -> ArchiveRuns {
        sides.append(side)
        guard side == .b else { return ArchiveRuns(runs: [], more: false) }
        return ArchiveRuns(runs: [ArchiveRun(name: "new", started: Date(timeIntervalSince1970: 2)),
                                  ArchiveRun(name: "old", started: Date(timeIntervalSince1970: 1))],
                           more: true)
    }

    func items(_ id: String, side: FolderSide, run: String, after: Data?) async throws -> ArchivePage {
        let rest = stored.filter { path in after.map { $0.lexicographicallyPrecedes(path) } ?? true }
        // Answered as of the request, even when held.
        let page = ArchivePage(items: rest.prefix(2).map { ArchiveItem(path: $0, size: 1) }, more: rest.count > 2)
        guard after != nil else { return page }
        pageRequests += 1
        if refusesNextPage { throw TwoRunFixtureError.refused }
        if holdsNextPage {
            holdsNextPage = false
            await withCheckedContinuation { held = $0 }
        }
        return page
    }

    func putBack(_ id: String, side: FolderSide, run: String, path: Data) async throws -> Data {
        putBacks.append(path)
        stored.removeAll { $0 == path }
        return path + Data(" (restored)".utf8)
    }

    func putBackAll(_ id: String, side: FolderSide, run: String) async throws -> RunRestore {
        restoreRequests += 1
        if holdsRestore {
            holdsRestore = false
            await withCheckedContinuation { heldRestore = $0 }
        }
        if refusesRestore { throw TwoRunFixtureError.refused }
        let count = UInt64(stored.count)
        stored.removeAll()
        return RunRestore(restored: count > 0 ? count - 1 : 0, beside: count > 0 ? 1 : 0, failures: 0)
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

@MainActor
@Test
func aWholeRunRestoreReservesBothRestoreCommands() async throws {
    let client = ArchiveFixtureClient()
    let store = ArchiveStore(client: client)
    let viewModel = ArchiveViewModel(pair: docs(), store: store, agentConnected: true)
    await viewModel.show(.b)
    let row = try #require(viewModel.itemRows.first)

    await client.holdRestore()
    let restoring = Task { await viewModel.putBackAll() }
    try #require(await client.waitForHeldRestore(), "Restore never reached the fixture")
    #expect(!viewModel.canPutBack)
    #expect(!viewModel.canPutBackAll)
    #expect(viewModel.statusText == "Putting items back…")
    #expect(!viewModel.itemRows.isEmpty)

    await viewModel.putBackAll()
    await viewModel.putBack(row)
    #expect(await client.restoreRequests == 1)
    #expect(await client.putBacks.isEmpty)
    await client.releaseRestore()
    await restoring.value

    #expect(!store.isPuttingBack)
    #expect(viewModel.itemRows.isEmpty)
    #expect(viewModel.restoreText == "Put back 3 items. 1 went beside newer versions.")
}

@MainActor
@Test
func aFailedWholeRunRestoreReleasesItsReservation() async {
    let client = ArchiveFixtureClient()
    let store = ArchiveStore(client: client)
    let viewModel = ArchiveViewModel(pair: docs(), store: store, agentConnected: true)
    await viewModel.show(.b)
    await client.refuseRestore()

    await viewModel.putBackAll()
    #expect(store.error != nil)
    #expect(!store.isPuttingBack)
    #expect(viewModel.canPutBack)
    #expect(viewModel.canPutBackAll)
    #expect(viewModel.itemRows.map(\.path) == ["a", "b"])
}

@MainActor
@Test
func aPageAskedForBeforePutBackAllNeverListsRestoredItems() async {
    let client = ArchiveFixtureClient()
    let store = ArchiveStore(client: client)
    let viewModel = ArchiveViewModel(pair: docs(), store: store, agentConnected: true)
    await viewModel.show(.b)

    await client.holdNextPage()
    let more = Task { await viewModel.loadMore() }
    await client.waitForHeldPage()
    await viewModel.putBackAll()
    await client.releasePage()
    await more.value

    #expect(viewModel.itemRows.isEmpty)
    #expect(!viewModel.canPutBackAll)
}

@MainActor
@Test
func showMoreWhileAPageLoadsAsksOnce() async {
    let client = ArchiveFixtureClient()
    let store = ArchiveStore(client: client)
    let viewModel = ArchiveViewModel(pair: docs(), store: store, agentConnected: true)
    await viewModel.show(.b)

    await client.holdNextPage()
    let first = Task { await viewModel.loadMore() }
    await client.waitForHeldPage()
    await viewModel.loadMore()
    await client.releasePage()
    await first.value

    #expect(viewModel.itemRows.map(\.path) == ["a", "b", "c"])
    #expect(await client.pageRequests == 1)
}

@MainActor
@Test
func puttingBackSeveralRowsGoesOneAtATime() async {
    let client = ArchiveFixtureClient()
    let viewModel = ArchiveViewModel(pair: docs(), store: ArchiveStore(client: client), agentConnected: true)
    await viewModel.show(.b)

    await viewModel.putBack(viewModel.itemRows)
    #expect(await client.putBacks == [Data("a".utf8), Data("b".utf8)])
    #expect(viewModel.itemRows.isEmpty)
    #expect(viewModel.placedText?.hasPrefix("Put back as b (restored).") == true)
    // The status line counts the batch instead of naming its last item.
    #expect(viewModel.statusText == "Put back 2 items. The next sync copies them to the other folder.")
    #expect(viewModel.pairName == "Docs")
    #expect(ArchiveViewModel.sideTitle(.a) == "Folder A")
    #expect(ArchiveViewModel.sideTitle(.b) == "Folder B")
}

/// The archive has no search, so a known name is found by pressing Show More
/// once per page. A search loads pages until one matches.
@MainActor
@Test
func searchingTheArchiveLoadsPagesUntilSomethingMatches() async {
    let viewModel = ArchiveViewModel(pair: docs(), store: ArchiveStore(client: ArchiveFixtureClient()),
                                     agentConnected: true)
    await viewModel.show(.b)
    #expect(viewModel.itemRows.map(\.path) == ["a", "b"])

    // The first page holds no "c", so the search walks on to the next one.
    viewModel.searchText = "c"
    await viewModel.search()
    #expect(viewModel.itemRows.map(\.path) == ["c"])
    #expect(viewModel.isSearching == false)

    // A name that is not there stops at the end of the run's items.
    viewModel.searchText = "zzz"
    await viewModel.search()
    #expect(viewModel.itemRows.isEmpty)
    #expect(viewModel.hasSearch)
    // The run is exhausted, so there is nothing left to keep searching.
    #expect(!viewModel.canKeepSearching)

    // Clearing the search lists everything loaded again.
    viewModel.searchText = ""
    await viewModel.search()
    #expect(viewModel.itemRows.map(\.path) == ["a", "b", "c"])
}

/// A search that starts while a page is already loading must wait for it
/// rather than give up on the query and report a run it never searched.
@MainActor
@Test
func aSearchWaitsForAPageAlreadyLoading() async {
    let client = ArchiveFixtureClient()
    let viewModel = ArchiveViewModel(pair: docs(), store: ArchiveStore(client: client),
                                     agentConnected: true)
    await viewModel.show(.b)
    #expect(viewModel.itemRows.map(\.path) == ["a", "b"])

    // A Show More is in flight when the person types.
    await client.holdNextPage()
    let more = Task { await viewModel.loadMore() }
    await client.waitForHeldPage()

    viewModel.searchText = "c"
    let searching = Task { await viewModel.search() }
    // Let the search reach the in-flight page before it lands.
    try? await Task.sleep(for: .milliseconds(30))

    await client.releasePage()
    await more.value
    await searching.value
    #expect(viewModel.itemRows.map(\.path) == ["c"])
}

/// A walk that is waiting on a page when the text changes must answer the
/// newest text, not the one it started with.
@MainActor
@Test
func aSearchThatLandsMidWalkAnswersTheNewestText() async {
    let client = ArchiveFixtureClient()
    let viewModel = ArchiveViewModel(pair: docs(), store: ArchiveStore(client: client),
                                     agentConnected: true)
    await viewModel.show(.b)
    #expect(viewModel.itemRows.map(\.path) == ["a", "b"])

    // The walk waits on the run's second page, as a Show More holds it.
    await client.holdNextPage()
    let more = Task { await viewModel.loadMore() }
    await client.waitForHeldPage()

    viewModel.searchText = "zzz"
    let searching = Task { await viewModel.search() }
    try? await Task.sleep(for: .milliseconds(30))
    viewModel.searchText = "c"

    await client.releasePage()
    await more.value
    await searching.value
    #expect(viewModel.itemRows.map(\.path) == ["c"])
}

/// A walk whose task is cancelled while it waits on a page must not fetch one.
@MainActor
@Test
func aCancelledSearchFetchesNoFurtherPage() async {
    let client = ArchiveFixtureClient()
    let viewModel = ArchiveViewModel(pair: docs(), store: ArchiveStore(client: client),
                                     agentConnected: true)
    await viewModel.show(.b)
    #expect(viewModel.itemRows.map(\.path) == ["a", "b"])

    await client.holdNextPage()
    let more = Task { await viewModel.loadMore() }
    await client.waitForHeldPage()
    viewModel.searchText = "c"
    let searching = Task { await viewModel.search() }
    try? await Task.sleep(for: .milliseconds(30))

    searching.cancel()
    try? await Task.sleep(for: .milliseconds(30))
    let requested = await client.pageRequests

    await client.releasePage()
    await more.value
    await searching.value
    #expect(await client.pageRequests == requested)
}

/// A next page that fails ends the walk: what was listed stays listed,
/// the error shows, and the same page is not asked for fifty times.
@MainActor
@Test
func aFailedPageStopsTheSearchAndKeepsItsItems() async {
    let client = ArchiveFixtureClient()
    let store = ArchiveStore(client: client)
    let viewModel = ArchiveViewModel(pair: docs(), store: store, agentConnected: true)
    await viewModel.show(.b)
    #expect(viewModel.itemRows.map(\.path) == ["a", "b"])
    await client.refuseNextPage()

    viewModel.searchText = "zzz"
    await viewModel.search()

    #expect(await client.pageRequests == 1)
    #expect(store.items.map(\.path) == [Data("a".utf8), Data("b".utf8)])
    #expect(viewModel.error != nil)
    #expect(!viewModel.isSearching)
}

@MainActor
@Test
func aWholeRunPutBackOutranksTheLastItemInTheStatusLine() async {
    let viewModel = ArchiveViewModel(pair: docs(), store: ArchiveStore(client: ArchiveFixtureClient()),
                                     agentConnected: true)
    await viewModel.show(.b)
    #expect(viewModel.statusText == nil)

    await viewModel.putBackAll()
    #expect(viewModel.statusText == viewModel.restoreText)
    #expect(viewModel.statusText != nil)
}

@Test
func recentRunTitlesKeepTheClockTime() {
    let now = Date(timeIntervalSince1970: 1_761_300_000)
    // Three hours old: relative phrase plus the start time, so two runs
    // the same day stay distinguishable.
    let recent = now.addingTimeInterval(-3 * 3600)
    let recentTitle = ArchiveViewModel.runTitle(recent, now: now)
    #expect(recentTitle.contains(recent.formatted(date: .omitted, time: .shortened)))
    #expect(recentTitle != recent.formatted(date: .abbreviated, time: .standard))

    // Past a day: absolute date and time only.
    let old = now.addingTimeInterval(-50 * 3600)
    #expect(ArchiveViewModel.runTitle(old, now: now)
        == old.formatted(date: .abbreviated, time: .standard))
}

@MainActor
@Test
func siblingStoresShareTheClientButNotTheState() async {
    let client = ArchiveFixtureClient()
    let store = ArchiveStore(client: client)
    await store.open(docs().id, side: .b)

    let sibling = store.sibling()
    #expect(sibling.runs.isEmpty)
    #expect(sibling.side == .a)
    #expect(sibling.run == nil)

    await sibling.open(docs().id, side: .b)
    #expect(sibling.runs.map(\.name) == ["new", "old"])
    #expect(store.run == "new")
    #expect(await client.sides == [.b, .b])
}

private enum TwoRunFixtureError: Error { case refused }

/// Two runs of one side that both archived b.txt, as a file edited
/// between syncs is; a put-back can be held, or refused by name.
private actor TwoRunArchiveClient: ArchiveClient {
    /// "run/name" per put-back, in request order.
    private(set) var putBacks: [String] = []
    private(set) var restoreRequests = 0
    private var refused: Set<String> = []
    private var holdsNext = false
    private var held: CheckedContinuation<Void, Never>?

    func refusePutBack(of name: String) { refused.insert(name) }
    func holdNextPutBack() { holdsNext = true }

    func waitForHeldPutBack() async {
        while held == nil { await Task.yield() }
    }

    func releasePutBack() {
        held?.resume()
        held = nil
    }

    func runs(_ id: String, side: FolderSide) async throws -> ArchiveRuns {
        ArchiveRuns(runs: [ArchiveRun(name: "new", started: Date(timeIntervalSince1970: 2)),
                           ArchiveRun(name: "old", started: Date(timeIntervalSince1970: 1))],
                    more: false)
    }

    func items(_ id: String, side: FolderSide, run: String, after: Data?) async throws -> ArchivePage {
        let names = run == "new" ? ["a.txt", "b.txt", "c.txt"] : ["b.txt", "c.txt"]
        return ArchivePage(items: names.map { ArchiveItem(path: Data($0.utf8), size: 1) }, more: false)
    }

    func putBack(_ id: String, side: FolderSide, run: String, path: Data) async throws -> Data {
        let name = String(decoding: path, as: UTF8.self)
        putBacks.append("\(run)/\(name)")
        if holdsNext {
            holdsNext = false
            await withCheckedContinuation { held = $0 }
        }
        if refused.contains(name) { throw TwoRunFixtureError.refused }
        return path
    }

    func putBackAll(_ id: String, side: FolderSide, run: String) async throws -> RunRestore {
        restoreRequests += 1
        return RunRestore(restored: 0, beside: 0, failures: 0)
    }
}

// A batch belongs to the run it started in. Choosing another run while
// it works used to continue there, putting back that run's b.txt, which
// nobody selected; Put Back now also waits until the batch ends.
@MainActor
@Test
func aPutBackBatchStaysInTheRunItStartedIn() async {
    let client = TwoRunArchiveClient()
    let viewModel = ArchiveViewModel(pair: docs(), store: ArchiveStore(client: client), agentConnected: true)
    await viewModel.show(.b)
    let rows = viewModel.itemRows

    await client.holdNextPutBack()
    let putting = Task { await viewModel.putBack(rows) }
    await client.waitForHeldPutBack()
    #expect(!viewModel.canPutBack)
    #expect(!viewModel.canPutBackAll)

    await viewModel.putBackAll()
    #expect(await client.restoreRequests == 0)

    await viewModel.select("old")
    await client.releasePutBack()
    await putting.value
    #expect(await client.putBacks == ["new/a.txt"])
    #expect(viewModel.itemRows.map(\.path) == ["b.txt", "c.txt"])
    #expect(viewModel.canPutBack)
}

// The first failure ends the batch, so its error stays shown.
@MainActor
@Test
func aPutBackBatchStopsAtTheFirstFailure() async {
    let client = TwoRunArchiveClient()
    await client.refusePutBack(of: "b.txt")
    let viewModel = ArchiveViewModel(pair: docs(), store: ArchiveStore(client: client), agentConnected: true)
    await viewModel.show(.b)

    await viewModel.putBack(viewModel.itemRows)
    #expect(await client.putBacks == ["new/a.txt", "new/b.txt"])
    #expect(viewModel.error != nil)
    #expect(viewModel.itemRows.map(\.path) == ["b.txt", "c.txt"])
    // One item went back before the failure.
    #expect(viewModel.statusText == viewModel.placedText)
    #expect(viewModel.canPutBack)
}
