// The view model is the only source of the strings the views render;
// the version text must carry the store's version.

import Foundation
import Testing
@testable import Foldover

@MainActor
@Test
func textsIncludeTheCoreVersion() {
    let store = AppInfoStore()
    let viewModel = MainViewModel(appInfo: store)

    #expect(viewModel.coreVersionText == "Core \(store.coreVersion)")
    #expect(!viewModel.emptyState.isEmpty)
    #expect(!viewModel.statusLine.isEmpty)
    #expect(!viewModel.agentStatusText.isEmpty)
    #expect(!viewModel.agentNeedsApproval)
}

private let docsID = "33333333333333333333333333333333"

private actor OnePairClient: PairClient {
    private let pair = SyncPair(id: docsID, revision: 1, settings: PairSettings(
        name: "Docs", left: SelectedFolder(bytes: Data("/a".utf8)),
        right: SelectedFolder(bytes: Data("/b".utf8)), mode: .sync
    ))

    func page(after: String?) async throws -> PairCatalogPage {
        PairCatalogPage(pairs: after == nil ? [pair] : [], next: nil)
    }
    func create(_ settings: PairSettings) async throws -> SyncPair { pair }
    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair { pair }
    func remove(_ pair: SyncPair) async throws -> SyncPair { pair }
}

/// Every run finishes at once having copied two files.
private struct FinishingSyncClient: SyncClient {
    private let result = RunSummary(cancelled: false, copied: 2, updated: 0, archived: 0, recovered: 0,
                                    conflicts: 0, failures: [], failureCount: 0, archive: "run")

    func preview(_ id: String) async throws -> PairSyncState { .idle }
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64) async throws -> PairSyncState {
        .finished(result)
    }
    func status(_ id: String) async throws -> PairSyncState { .finished(result) }
    func cancel(_ id: String) async throws -> PairSyncState { .idle }
    func resolve(_ id: String, path: Data, keep: FolderSide) async throws -> PairSyncState { .idle }
    func keepBoth(_ id: String, path: Data) async throws -> PairSyncState { .idle }
    func history(_ id: String) async throws -> [RunRecord] { [] }
    func changes(_ id: String, after: Data?, filter: ChangeFilter?) async throws -> ChangePage {
        ChangePage(changes: [], more: false)
    }
}

@MainActor
@Test
func shortcutsLoadPairsThenSyncAndSayWhatChanged() async {
    let viewModel = MainViewModel(
        appInfo: AppInfoStore(), agent: AgentStore(statusRequest: { "1.0" }),
        pairs: PairStore(client: OnePairClient()), sync: SyncStore(client: FinishingSyncClient())
    )
    #expect(await viewModel.intentPairs() == [IntentPair(id: docsID, name: "Docs")])
    #expect(await viewModel.perform(.sync, onPairWithID: docsID).hasPrefix("Synced: copied 2"))
    #expect(await viewModel.perform(.sync, onPairWithID: "missing") == viewModel.agentStatusText)
}

@MainActor
@Test
func setupGuidesFromAgentToFirstPair() async {
    let offline = MainViewModel(appInfo: AppInfoStore(), agent: AgentStore(statusRequest: { throw SetupFixtureError.off }))
    #expect(offline.showsSetup)
    #expect(offline.setupSteps.map(\.progress) == [.current, .upcoming, .upcoming])
    #expect(offline.setupSteps[0].action == .enableAgent)

    let empty = MainViewModel(
        appInfo: AppInfoStore(), agent: AgentStore(statusRequest: { "1.0" }),
        pairs: PairStore(client: EmptyCatalogClient())
    )
    await empty.refreshAgent()
    #expect(empty.setupSteps.map(\.progress) == [.done, .current, .upcoming])
    #expect(empty.setupSteps[1].action == .newPair)
    await empty.perform(.newPair)
    #expect(empty.editor != nil)
}

private enum SetupFixtureError: Error { case off }

private actor EmptyCatalogClient: PairClient {
    func page(after: String?) async throws -> PairCatalogPage { PairCatalogPage(pairs: [], next: nil) }
    func create(_ settings: PairSettings) async throws -> SyncPair {
        SyncPair(id: docsID, revision: 1, settings: settings)
    }
    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair { pair }
    func remove(_ pair: SyncPair) async throws -> SyncPair { pair }
}

private enum MonitorFixtureError: Error { case unreachable }

/// Counts calls (and tags them) and wakes waiters when a count or tag
/// arrives, so tests synchronize on events instead of sleeping. A
/// bounded wait returns `false` rather than hanging when the event a
/// bug suppresses never comes.
private actor CallProbe {
    enum Target {
        case calls(Int)
        case tag(String)
    }

    private(set) var count = 0
    private var tags: [String] = []
    private var waiters: [(id: UUID, target: Target, then: CheckedContinuation<Bool, Never>)] = []

    func hit(_ tag: String? = nil) {
        count += 1
        if let tag { tags.append(tag) }
        let ready = waiters.filter { reached($0.target) }
        waiters.removeAll { reached($0.target) }
        for waiter in ready { waiter.then.resume(returning: true) }
    }

    @discardableResult
    func wait(for target: Target, timeout: Duration = .seconds(3)) async -> Bool {
        if reached(target) { return true }
        let id = UUID()
        return await withCheckedContinuation { continuation in
            waiters.append((id, target, continuation))
            Task {
                try? await Task.sleep(for: timeout)
                self.expire(id)
            }
        }
    }

    private func reached(_ target: Target) -> Bool {
        switch target {
        case .calls(let count): self.count >= count
        case .tag(let tag): tags.contains(tag)
        }
    }

    private func expire(_ id: UUID) {
        guard let index = waiters.firstIndex(where: { $0.id == id }) else { return }
        waiters.remove(at: index).then.resume(returning: false)
    }
}

/// Agent health replies the test can flip between a version and an error.
private actor AgentProbe {
    private let probe = CallProbe()
    private var result: Result<String, Error> = .success("1.0.0")

    func status() async throws -> String {
        await probe.hit()
        return try result.get()
    }

    func fail() { result = .failure(MonitorFixtureError.unreachable) }
    func recover(_ version: String) { result = .success(version) }
    func calls() async -> Int { await probe.count }
}

/// A pair catalog the test can rewrite behind the store's back.
private actor CatalogProbe: PairClient {
    private let probe = CallProbe()
    private var pairs: [SyncPair]

    init(_ pairs: [SyncPair]) { self.pairs = pairs }
    func set(_ pairs: [SyncPair]) { self.pairs = pairs }

    func page(after: String?) async throws -> PairCatalogPage {
        await probe.hit()
        return PairCatalogPage(pairs: pairs, next: nil)
    }

    func create(_ settings: PairSettings) async throws -> SyncPair {
        throw MonitorFixtureError.unreachable
    }
    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair {
        throw MonitorFixtureError.unreachable
    }
    func remove(_ pair: SyncPair) async throws -> SyncPair {
        throw MonitorFixtureError.unreachable
    }

    func calls() async -> Int { await probe.count }
}

/// Replies `.idle` and records which pairs the monitor keeps checking.
private actor SyncProbe: SyncClient {
    private let probe = CallProbe()

    func status(_ id: String) async throws -> PairSyncState {
        await probe.hit(id)
        return .idle
    }
    func preview(_ id: String) async throws -> PairSyncState { .idle }
    func run(_ id: String, approvedA: UInt64, approvedB: UInt64) async throws -> PairSyncState { .idle }
    func cancel(_ id: String) async throws -> PairSyncState { .idle }
    func resolve(_ id: String, path: Data, keep: FolderSide) async throws -> PairSyncState { .idle }
    func keepBoth(_ id: String, path: Data) async throws -> PairSyncState { .idle }
    func history(_ id: String) async throws -> [RunRecord] { [] }
    func changes(_ id: String, after: Data?, filter: ChangeFilter?) async throws -> ChangePage {
        ChangePage(changes: [], more: false)
    }

    func calls() async -> Int { await probe.count }

    @discardableResult
    func waitForCalls(_ count: Int) async -> Bool { await probe.wait(for: .calls(count)) }
    @discardableResult
    func waitForPair(_ id: String) async -> Bool { await probe.wait(for: .tag(id)) }
}

private func monitorPair(_ id: String, _ name: String) -> SyncPair {
    SyncPair(id: id, revision: 1, settings: PairSettings(
        name: name, left: SelectedFolder(bytes: Data("/a".utf8)),
        right: SelectedFolder(bytes: Data("/b".utf8)), mode: .sync
    ))
}

/// Polls a MainActor condition with a deadline, so an effect a bug
/// suppresses fails the test instead of hanging it.
@MainActor
private func eventually(_ condition: @MainActor () -> Bool,
                        timeout: Duration = .seconds(3)) async -> Bool {
    let deadline = ContinuousClock.now + timeout
    while !condition() {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(1))
    }
    return true
}

// Regression for PR57: monitor refreshed the agent and the catalog only
// while `!hasLoaded`, so after the first load a dead or restarted agent
// and pairs changed outside the app stayed stale forever.
@MainActor
@Test
func monitorRefreshesTheAgentAndCatalogAfterTheFirstLoad() async {
    let first = monitorPair("11111111111111111111111111111111", "Photos")
    let added = monitorPair("22222222222222222222222222222222", "Archives")
    let agent = AgentProbe()
    let catalog = CatalogProbe([first])
    let sync = SyncProbe()
    let viewModel = MainViewModel(
        appInfo: AppInfoStore(),
        agent: AgentStore(statusRequest: { try await agent.status() }),
        pairs: PairStore(client: catalog),
        sync: SyncStore(client: sync, pollInterval: .milliseconds(1))
    )

    let monitor = Task { await viewModel.monitor(every: .milliseconds(1)) }

    // Several checkAll ticks have run: the pair was polled each time.
    #expect(await sync.waitForCalls(3))
    // Health and the catalog must keep refreshing past the first load.
    #expect(await agent.calls() >= 2)
    #expect(await catalog.calls() >= 2)

    // A pair added outside the app reaches the next catalog refresh.
    await catalog.set([first, added])
    #expect(await sync.waitForPair(added.id))
    #expect(viewModel.pairs.count == 2)

    // A dead agent surfaces on the next health check.
    await agent.fail()
    #expect(await eventually { viewModel.agentStatusText.contains("unavailable") })

    // A restart reconnects and resumes checking the pairs.
    let stalled = await sync.calls()
    await agent.recover("1.0.0")
    #expect(await eventually { viewModel.agentStatusText.contains("1.0.0 connected") })
    #expect(await sync.waitForCalls(stalled + 1))

    monitor.cancel()
    await monitor.value
}
