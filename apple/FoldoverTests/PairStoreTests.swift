import Foundation
import Testing
@testable import Foldover

private enum SaveOutcome: Sendable { case succeeds, fails }
private enum PairFixtureError: Error { case changedElsewhere }

private func pairFixture(id: String = "11111111111111111111111111111111") -> SyncPair {
    SyncPair(id: id, revision: 1, settings: PairSettings(
        name: "Photos", left: SelectedFolder(bytes: Data([47, 97, 47, 255])),
        right: SelectedFolder(bytes: Data("/b".utf8)), mode: .backupLeft
    ))
}

private actor PairFixtureClient: PairClient {
    private let catalog: [SyncPair]
    private let outcome: SaveOutcome
    private(set) var updatedRevision: UInt64?
    private(set) var savedSettings: PairSettings?
    private(set) var removedRevision: UInt64?

    init(catalog: [SyncPair] = [], outcome: SaveOutcome = .succeeds) {
        self.catalog = catalog
        self.outcome = outcome
    }

    func page(after: String?) async throws -> PairCatalogPage {
        let remaining = catalog.filter { pair in after.map { pair.id > $0 } ?? true }
        return PairCatalogPage(pairs: Array(remaining.prefix(1)), next: remaining.count > 1 ? remaining.first?.id : nil)
    }

    func create(_ settings: PairSettings) async throws -> SyncPair {
        savedSettings = settings
        if case .fails = outcome { throw PairFixtureError.changedElsewhere }
        return SyncPair(id: pairFixture().id, revision: 1, settings: settings)
    }

    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair {
        updatedRevision = pair.revision
        savedSettings = settings
        if case .fails = outcome { throw PairFixtureError.changedElsewhere }
        return SyncPair(id: pair.id, revision: pair.revision + 1, settings: settings)
    }

    func remove(_ pair: SyncPair) async throws -> SyncPair {
        removedRevision = pair.revision
        if case .fails = outcome { throw PairFixtureError.changedElsewhere }
        return pair
    }
}

@MainActor
@Test
func pairCatalogLoadsAllPagesAndPreservesRawPaths() async {
    let first = pairFixture()
    let second = pairFixture(id: "22222222222222222222222222222222")
    let store = PairStore(client: PairFixtureClient(catalog: [first, second]))
    await store.refresh()
    #expect(store.pairs == [first, second])
    #expect(store.hasLoaded)
    #expect(!store.isLoading)
    #expect(store.loadError == nil)
    #expect(store.pairs.first?.settings.left.bytes.last == 255)
}

private actor DeferredPairClient: PairClient {
    private var calls = 0
    private var first: CheckedContinuation<PairCatalogPage, Error>?
    private var started: CheckedContinuation<Void, Never>?

    func page(after: String?) async throws -> PairCatalogPage {
        calls += 1
        if calls > 1 { return PairCatalogPage(pairs: [pairFixture(id: "22222222222222222222222222222222")], next: nil) }
        return try await withCheckedThrowingContinuation { continuation in
            first = continuation
            started?.resume()
            started = nil
        }
    }

    func waitUntilStarted() async {
        if first != nil { return }
        await withCheckedContinuation { started = $0 }
    }

    func finishFirst() {
        first?.resume(returning: PairCatalogPage(pairs: [pairFixture()], next: nil))
        first = nil
    }

    func create(_ settings: PairSettings) async throws -> SyncPair { pairFixture() }
    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair { pair }
    func remove(_ pair: SyncPair) async throws -> SyncPair { pair }
}

@MainActor
@Test
func stalePairRefreshCannotReplaceANewerCatalog() async {
    let client = DeferredPairClient()
    let store = PairStore(client: client)
    let older = Task { await store.refresh() }
    await client.waitUntilStarted()
    await store.refresh()
    await client.finishFirst()
    await older.value
    #expect(store.pairs.map(\.id) == ["22222222222222222222222222222222"])
    #expect(!store.isLoading)
}

private actor FailingPairPageClient: PairClient {
    func page(after: String?) async throws -> PairCatalogPage {
        if after != nil { throw PairFixtureError.changedElsewhere }
        return PairCatalogPage(pairs: [pairFixture()], next: pairFixture().id)
    }

    func create(_ settings: PairSettings) async throws -> SyncPair { throw PairFixtureError.changedElsewhere }
    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair { throw PairFixtureError.changedElsewhere }
    func remove(_ pair: SyncPair) async throws -> SyncPair { throw PairFixtureError.changedElsewhere }
}

@MainActor
@Test
func incompleteCatalogIsNotPublishedOrReportedAsEmpty() async {
    let store = PairStore(client: FailingPairPageClient())
    let viewModel = MainViewModel(appInfo: AppInfoStore(), agent: AgentStore(statusRequest: { "test" }), pairs: store)
    await viewModel.refreshAgent()
    #expect(store.pairs.isEmpty)
    #expect(!store.hasLoaded)
    #expect(store.loadError != nil)
    #expect(!viewModel.canConfigurePairs)
    #expect(viewModel.detailTitle != viewModel.emptyState)
    #expect(viewModel.statusLine != viewModel.emptyState)
}

@MainActor
@Test
func pairEditorRequiresTwoFoldersAndCreatesTheSelectedDirection() async {
    let client = PairFixtureClient()
    let store = PairStore(client: client)
    let editor = PairEditorViewModel(store: store)
    editor.name = "Photos"
    #expect(!editor.canSave)
    editor.chooseFolder(.success([URL(fileURLWithPath: "/a")]), side: .left)
    #expect(!editor.canSave)
    editor.chooseFolder(.success([URL(fileURLWithPath: "/b")]), side: .right)
    editor.mode = .mirrorRight
    #expect(editor.canSave)
    #expect(await editor.save())
    #expect(store.pairs.first?.settings.mode == .mirrorRight)
    #expect(await client.savedSettings?.left.bytes == Data("/a".utf8))
    #expect(editor.savedPairID == store.pairs.first?.id)
}

@MainActor
@Test
func failedPairUpdateKeepsTheDraftRevisionAndRawPaths() async {
    let original = pairFixture()
    let client = PairFixtureClient(catalog: [original], outcome: .fails)
    let store = PairStore(client: client)
    await store.refresh()
    let editor = PairEditorViewModel(store: store, editing: original)
    editor.name = "Renamed"
    editor.mode = .backupRight
    #expect(!(await editor.save()))
    #expect(editor.errorMessage != nil)
    #expect(editor.name == "Renamed")
    #expect(editor.mode == .backupRight)
    #expect(editor.left == original.settings.left)
    #expect(await client.updatedRevision == original.revision)
    #expect(store.pairs == [original])
    #expect(!editor.isSaving)
    #expect(!store.isSaving)
}

@MainActor
@Test
func mainViewSelectsTheSavedPairAfterClosingItsEditor() async {
    let client = PairFixtureClient()
    let store = PairStore(client: client)
    let viewModel = MainViewModel(appInfo: AppInfoStore(), agent: AgentStore(statusRequest: { "test" }), pairs: store)
    await viewModel.refreshAgent()
    viewModel.newPair()
    let editor = viewModel.editor!
    editor.name = "Photos"
    editor.chooseFolder(.success([URL(fileURLWithPath: "/a")]), side: .left)
    editor.chooseFolder(.success([URL(fileURLWithPath: "/b")]), side: .right)
    #expect(await editor.save())
    viewModel.dismissEditor()
    #expect(viewModel.selectedPair?.settings.name == "Photos")
    #expect(viewModel.editor == nil)
    #expect(viewModel.statusLine.contains("1"))
}

@MainActor
@Test
func removalUsesTheConfirmedRevisionAndClearsOnlyThatPair() async {
    let original = pairFixture()
    let client = PairFixtureClient(catalog: [original])
    let store = PairStore(client: client)
    let viewModel = MainViewModel(appInfo: AppInfoStore(), agent: AgentStore(statusRequest: { "test" }), pairs: store)
    await viewModel.refreshAgent()
    viewModel.selectedPairID = original.id
    viewModel.requestRemoval()
    let confirmed = viewModel.removalTarget!
    viewModel.cancelRemoval()
    await viewModel.removePair(confirmed)
    #expect(await client.removedRevision == original.revision)
    #expect(store.pairs.isEmpty)
    #expect(viewModel.selectedPairID == nil)
    #expect(viewModel.operationError == nil)
}

@MainActor
@Test
func rejectedRemovalKeepsTheCachedPair() async {
    let original = pairFixture()
    let store = PairStore(client: PairFixtureClient(catalog: [original], outcome: .fails))
    await store.refresh()
    do {
        try await store.remove(original)
        Issue.record("Removal should fail")
    } catch {
        #expect(store.pairs == [original])
        #expect(!store.isSaving)
    }
}

@Test
func pairOptionDefaultsComeFromTheEngine() {
    let defaults = PairOptions.defaults
    #expect(defaults.syncModificationTimes)
    #expect(defaults.syncExtendedAttributes)
    #expect(!defaults.syncPermissions)
    #expect(defaults.durability == .standard)
    #expect(defaults.schedule == .manual)
    #expect(defaults.canary == nil)
}

@MainActor
@Test
func scheduleChoicesKeepAnIntervalSetElsewhere() {
    let store = PairStore(client: PairFixtureClient())
    let editor = PairEditorViewModel(store: store)
    #expect(editor.scheduleChoices.count == 6)
    editor.options.schedule = .every(minutes: 30)
    #expect(editor.scheduleChoices.last == .every(minutes: 30))
    #expect(PairEditorViewModel.scheduleTitle(.manual) == "Only when I click Sync Now")
    #expect(PairEditorViewModel.scheduleTitle(.onChange) == "When files change")
    #expect(PairEditorViewModel.scheduleTitle(.every(minutes: 30)) == "Every 30 minutes")
}

@MainActor
@Test
func editedOptionsAreSavedAndOnlyMetadataChangesResetHistory() async {
    let original = pairFixture()
    let client = PairFixtureClient(catalog: [original])
    let store = PairStore(client: client)
    await store.refresh()
    let editor = PairEditorViewModel(store: store, editing: original)
    #expect(!editor.resetsSyncHistory)
    editor.options.durability = .full
    #expect(!editor.resetsSyncHistory)
    editor.options.syncPermissions.toggle()
    #expect(editor.resetsSyncHistory)
    #expect(await editor.save())
    #expect(await client.savedSettings?.options == editor.options)
}

@MainActor
@Test
func filterTextKeepsOneRulePerLine() {
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()))
    #expect(editor.filterText.isEmpty)
    editor.filterText = "*.tmp\nbuild/"
    #expect(editor.options.filters == ["*.tmp", "build/"])
    #expect(editor.filterText == "*.tmp\nbuild/")
}

@MainActor
@Test
func sizeLimitsAreWholeMegabytes() {
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()))
    #expect(!editor.limitsSize)
    editor.limitsSize = true
    #expect(editor.options.sizeLimit == 1_000_000_000)
    editor.sizeLimitMegabytes = 0
    #expect(editor.options.sizeLimit == 1_000_000)
    editor.sizeLimitMegabytes = 250
    #expect(editor.sizeLimitMegabytes == 250)
    editor.sizeLimitMegabytes = .max
    #expect(editor.options.sizeLimit! > 0)
    editor.limitsSize = false
    #expect(editor.options.sizeLimit == nil)
}

@MainActor
@Test
func ageLimitsAreWholeDaysUpToACentury() {
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()))
    #expect(!editor.limitsAge)
    editor.limitsAge = true
    #expect(editor.options.ageLimit == 365)
    editor.ageLimitDays = 0
    #expect(editor.options.ageLimit == 1)
    editor.ageLimitDays = .max
    #expect(editor.options.ageLimit == 36_500)
    editor.limitsAge = false
    #expect(editor.options.ageLimit == nil)
}

@MainActor
@Test
func aCanaryIsAForgivingRelativePathOutsideFoldover() {
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()))
    #expect(editor.canary == "")
    #expect(editor.options.canary == nil)
    #expect(editor.canaryWarning == nil)
    editor.canary = "  docs/canary.txt  "
    #expect(editor.options.canary == Data("docs/canary.txt".utf8))
    #expect(editor.canary == "docs/canary.txt")
    #expect(editor.canaryWarning == nil)
    editor.canary = "/canary.txt"
    #expect(editor.options.canary == Data("canary.txt".utf8))
    editor.canary = "   "
    #expect(editor.options.canary == nil)
    #expect(editor.canaryWarning == nil)
}

@MainActor
@Test
func anInvalidCanaryShowsAWarningAndBlocksSaving() {
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()))
    editor.name = "Photos"
    editor.chooseFolder(.success([URL(fileURLWithPath: "/a")]), side: .left)
    editor.chooseFolder(.success([URL(fileURLWithPath: "/b")]), side: .right)
    #expect(editor.canSave)
    for path in [".foldover/id", ".foldover", "a//b", "a/../b", "./a", "b/"] {
        editor.canary = path
        #expect(editor.canaryWarning != nil, "expected a warning for \(path)")
        #expect(!editor.canSave)
    }
    editor.canary = "docs/canary.txt"
    #expect(editor.canaryWarning == nil)
    #expect(editor.canSave)
}

@MainActor
@Test
func aSavedCanarySurvivesTheRoundTripThroughTheStore() async {
    let client = PairFixtureClient()
    let store = PairStore(client: client)
    let editor = PairEditorViewModel(store: store)
    editor.name = "Photos"
    editor.chooseFolder(.success([URL(fileURLWithPath: "/a")]), side: .left)
    editor.chooseFolder(.success([URL(fileURLWithPath: "/b")]), side: .right)
    editor.canary = "docs/canary.txt"
    #expect(await editor.save())
    #expect(await client.savedSettings?.options.canary == Data("docs/canary.txt".utf8))
    #expect(store.pairs.first?.settings.options.canary == Data("docs/canary.txt".utf8))
    let reloaded = PairEditorViewModel(store: store, editing: store.pairs.first)
    #expect(reloaded.canary == "docs/canary.txt")
}

@MainActor
@Test
func sftpFoldersAreWrittenAsLocators() {
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()))
    editor.chooseServer("  sftp://alice@nas.local/~/Photos ", side: .right)
    #expect(editor.folderText(.right) == "sftp://alice@nas.local/~/Photos")
    #expect(editor.errorMessage == nil)
    editor.chooseServer("nas.local:/Photos", side: .left)
    #expect(editor.errorMessage != nil)
    #expect(editor.folderText(.left) == "Choose a folder")
}

@MainActor
@Test
func serverWarningShowsForEitherSideAndFollowsFolderChanges() {
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()))
    #expect(editor.serverWarning == nil)
    #expect(!editor.usesServerFolder)
    editor.chooseServer("sftp://nas.local/~/Photos", side: .left)
    #expect(editor.serverWarning != nil)
    editor.chooseFolder(.success([URL(fileURLWithPath: "/a")]), side: .left)
    #expect(editor.serverWarning == nil)
    editor.chooseServer("sftp://nas.local/srv/photos", side: .right)
    #expect(editor.serverWarning != nil)
    editor.chooseFolder(.success([URL(fileURLWithPath: "/b")]), side: .right)
    #expect(editor.serverWarning == nil)
}

@MainActor
@Test
func editingAServerPairShowsTheWarning() {
    let serverPair = SyncPair(id: "33333333333333333333333333333333", revision: 1,
                              settings: PairSettings(
                                name: "Photos", left: SelectedFolder(bytes: Data("/a".utf8)),
                                right: SelectedFolder(bytes: Data("sftp://nas.local/~/Photos".utf8)),
                                mode: .backupLeft))
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()), editing: serverPair)
    #expect(editor.usesServerFolder)
    #expect(editor.serverWarning != nil)
}

@MainActor
@Test
func serverPairsExplainTheirMetadataWithoutChangingOptions() {
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()))
    #expect(editor.metadataNote.contains("Resource forks always sync"))
    let options = editor.options
    editor.chooseServer("sftp://nas.local/~/Photos", side: .right)
    #expect(!editor.metadataNote.contains("always sync"))
    #expect(editor.metadataNote.contains("resource-fork"))
    #expect(editor.options == options)
}

@MainActor
@Test
func serverFoldersOfferOnlyStandardDurability() {
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()))
    #expect(editor.durabilityChoices == PairDurability.allCases)
    editor.options.durability = .full
    #expect(editor.durabilityWarning == nil)
    #expect(editor.durabilityExplanation == PairDurability.full.explanation)
    editor.chooseServer("sftp://nas.local/~/Photos", side: .right)
    // The stored Full keeps its picker tag until the user repairs it.
    #expect(editor.durabilityChoices == PairDurability.allCases)
    #expect(editor.durabilityExplanation.contains("server"))
    #expect(editor.durabilityWarning != nil)
    #expect(editor.options.durability == .full)
    editor.options.durability = .standard
    #expect(editor.durabilityChoices == [.standard])
    #expect(editor.durabilityWarning == nil)
}

@MainActor
@Test
func anExistingFullPairGainingAServerRootMustChooseStandard() {
    var options = PairOptions.defaults
    options.durability = .full
    let fullPair = SyncPair(id: "44444444444444444444444444444444", revision: 1,
                            settings: PairSettings(
                                name: "Photos", left: SelectedFolder(bytes: Data("/a".utf8)),
                                right: SelectedFolder(bytes: Data("/b".utf8)),
                                mode: .backupLeft, options: options))
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()), editing: fullPair)
    #expect(editor.durabilityWarning == nil)
    #expect(editor.canSave)
    editor.chooseServer("sftp://nas.local/srv/photos", side: .right)
    #expect(editor.durabilityWarning != nil)
    #expect(editor.durabilityChoices.contains(.full))
    #expect(editor.options.durability == .full)
    #expect(!editor.canSave)
    editor.options.durability = .standard
    #expect(editor.durabilityWarning == nil)
    #expect(editor.durabilityChoices == [.standard])
    #expect(editor.canSave)
}
