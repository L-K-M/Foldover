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
