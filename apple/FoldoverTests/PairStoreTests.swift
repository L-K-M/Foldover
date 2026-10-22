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
    private let warnings: [PairReadinessWarning]
    private let servers: [NearbyServer]
    private let listings: [String: ServerListing]
    private(set) var browsed: [String] = []
    private(set) var updatedRevision: UInt64?
    private(set) var savedSettings: PairSettings?
    private(set) var removedRevision: UInt64?

    init(catalog: [SyncPair] = [], outcome: SaveOutcome = .succeeds,
         warnings: [PairReadinessWarning] = [], servers: [NearbyServer] = [],
         listings: [String: ServerListing] = [:]) {
        self.catalog = catalog
        self.outcome = outcome
        self.warnings = warnings
        self.servers = servers
        self.listings = listings
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

    func check(_ settings: PairSettings) async throws -> [PairReadinessWarning] { warnings }

    func servers() async throws -> [NearbyServer] { servers }

    func browse(_ locator: String) async throws -> ServerListing {
        browsed.append(locator)
        guard let listing = listings[locator] else { throw PairFixtureError.changedElsewhere }
        return listing
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
    func check(_ settings: PairSettings) async throws -> [PairReadinessWarning] { [] }
    func servers() async throws -> [NearbyServer] { [] }
    func browse(_ locator: String) async throws -> ServerListing { throw PairFixtureError.changedElsewhere }
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
    func check(_ settings: PairSettings) async throws -> [PairReadinessWarning] { throw PairFixtureError.changedElsewhere }
    func servers() async throws -> [NearbyServer] { throw PairFixtureError.changedElsewhere }
    func browse(_ locator: String) async throws -> ServerListing { throw PairFixtureError.changedElsewhere }
}

@MainActor
@Test
func incompleteCatalogIsNotPublishedOrReportedAsEmpty() async {
    let store = PairStore(client: FailingPairPageClient())
    let viewModel = MainViewModel(appInfo: AppInfoStore(), agent: AgentStore(statusRequest: { AgentReport(version: "test") }), pairs: store)
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
    let viewModel = MainViewModel(appInfo: AppInfoStore(), agent: AgentStore(statusRequest: { AgentReport(version: "test") }), pairs: store)
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
    let viewModel = MainViewModel(appInfo: AppInfoStore(), agent: AgentStore(statusRequest: { AgentReport(version: "test") }), pairs: store)
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
    #expect(editor.scheduleChoices.count == 7)
    editor.options.schedule = .every(minutes: 30)
    #expect(editor.scheduleChoices.last == .every(minutes: 30))
    #expect(PairEditorViewModel.scheduleTitle(.manual) == "Only when I click Sync Now")
    #expect(PairEditorViewModel.scheduleTitle(.onChange) == "When files change")
    #expect(PairEditorViewModel.scheduleTitle(.every(minutes: 30)) == "Every 30 minutes")
}

@MainActor
@Test
func onlyChangeTriggeredSchedulesSayHowChangesAreNoticed() {
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()))
    editor.options.schedule = .every(minutes: 30)
    #expect(editor.scheduleNote == nil)
    editor.options.schedule = .onChange
    #expect(editor.scheduleNote?.contains("every 5 minutes") == true)
}

@MainActor
@Test
func dailyScheduleIsEditableInPlace() {
    let store = PairStore(client: PairFixtureClient())
    let editor = PairEditorViewModel(store: store)
    #expect(editor.scheduleChoices.contains(.daily(minutes: 120, days: Set(SyncDay.allCases))))
    #expect(!editor.showsDailySettings)

    editor.options.schedule = .daily(minutes: 120, days: Set(SyncDay.allCases))
    #expect(editor.showsDailySettings)
    #expect(editor.scheduleChoices.last == editor.options.schedule)
    #expect(editor.dailyWarning == nil)

    editor.dailyMinutes = 18 * 60
    editor.dailyDays = [.monday, .tuesday, .wednesday, .thursday, .friday]
    #expect(editor.options.schedule == .daily(minutes: 18 * 60, days: [.monday, .tuesday, .wednesday, .thursday, .friday]))
    #expect(editor.scheduleChoices.contains(editor.options.schedule))

    editor.name = "Photos"
    editor.chooseFolder(.success([URL(filePath: "/a")]), side: .left)
    editor.chooseFolder(.success([URL(filePath: "/b")]), side: .right)
    #expect(editor.canSave)

    editor.dailyDays = []
    #expect(editor.dailyWarning != nil)
    #expect(!editor.canSave)

    #expect(PairEditorViewModel.scheduleTitle(.daily(minutes: 150, days: Set(SyncDay.allCases))) == "Every day at 02:30")
    #expect(PairEditorViewModel.scheduleTitle(.daily(minutes: 18 * 60, days: [.monday, .tuesday, .wednesday, .thursday, .friday])) == "Weekdays at 18:00")
    #expect(PairEditorViewModel.scheduleTitle(.daily(minutes: 7 * 60 + 15, days: [.monday, .wednesday, .friday])) == "Mon, Wed, Fri at 07:15")
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
func bandwidthLimitsAreWholeMegabytesPerSecond() {
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()))
    #expect(!editor.limitsBandwidth)
    editor.limitsBandwidth = true
    #expect(editor.options.bandwidthLimit == 10_000_000)
    editor.bandwidthLimitMegabytesPerSecond = 0
    #expect(editor.options.bandwidthLimit == 1_000_000)
    editor.bandwidthLimitMegabytesPerSecond = 25
    #expect(editor.bandwidthLimitMegabytesPerSecond == 25)
    editor.bandwidthLimitMegabytesPerSecond = .max
    #expect(editor.options.bandwidthLimit! > 0)
    editor.limitsBandwidth = false
    #expect(editor.options.bandwidthLimit == nil)
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
func readinessWarningsNameTheSidesInEnglish() async {
    let client = PairFixtureClient(warnings: [
        PairReadinessWarning(side: .a, kind: .cloudManaged(provider: "iCloud Drive")),
        PairReadinessWarning(side: .b, kind: .metadataUnsupported),
        PairReadinessWarning(side: .a, kind: .namesDiffer),
    ])
    let editor = PairEditorViewModel(store: PairStore(client: client))
    #expect(editor.readinessWarnings.isEmpty)
    editor.chooseFolder(.success([URL(fileURLWithPath: "/a")]), side: .left)
    editor.chooseServer("sftp://nas.local/srv/photos", side: .right)
    await editor.awaitReadinessCheck()
    #expect(editor.readinessWarnings == [
        "Folder A is in iCloud Drive, which also syncs it. Two sync engines on one folder can undo each other's changes; prefer a folder outside it.",
        "Folder B is on an SFTP server, which keeps no tags or other extended attributes; they will not sync to it.",
        "Folder A ignores the case of names and Folder B does not: names that differ only in case become conflicts to rename.",
    ])
}

@MainActor
@Test
func readinessWarningsRefreshWhenMetadataChoicesChange() async {
    let unsupported = PairReadinessWarning(side: .b, kind: .metadataUnsupported)
    let client = WarningOnAttributesClient(warning: unsupported)
    let editor = PairEditorViewModel(store: PairStore(client: client))
    editor.chooseFolder(.success([URL(fileURLWithPath: "/a")]), side: .left)
    editor.chooseFolder(.success([URL(fileURLWithPath: "/b")]), side: .right)
    await editor.awaitReadinessCheck()
    // The defaults sync extended attributes, so the warning is up.
    #expect(editor.readinessWarnings == [
        "Folder B cannot keep tags or other extended attributes; they will not sync to it.",
    ])
    editor.options.syncExtendedAttributes = false
    await editor.awaitReadinessCheck()
    #expect(editor.readinessWarnings.isEmpty)
    // Other option edits do not re-check.
    editor.options.durability = .full
    #expect(editor.readinessWarnings.isEmpty)
}

private actor WarningOnAttributesClient: PairClient {
    let warning: PairReadinessWarning
    init(warning: PairReadinessWarning) { self.warning = warning }

    func page(after: String?) async throws -> PairCatalogPage { PairCatalogPage(pairs: [], next: nil) }
    func create(_ settings: PairSettings) async throws -> SyncPair { throw PairFixtureError.changedElsewhere }
    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair { throw PairFixtureError.changedElsewhere }
    func remove(_ pair: SyncPair) async throws -> SyncPair { throw PairFixtureError.changedElsewhere }
    func check(_ settings: PairSettings) async throws -> [PairReadinessWarning] {
        settings.options.syncExtendedAttributes ? [warning] : []
    }
    func servers() async throws -> [NearbyServer] { [] }
    func browse(_ locator: String) async throws -> ServerListing {
        ServerListing(locator: locator, parent: nil, folders: [])
    }
}

@MainActor
@Test
func aFolderThatRefusesRecordsCanOnlyBeABackupTargetKeepingNothing() async {
    let editor = PairEditorViewModel(store: PairStore(client: RecordsRefusingClient()))
    editor.name = "Archive"
    editor.chooseFolder(.success([URL(fileURLWithPath: "/a")]), side: .left)
    editor.chooseFolder(.success([URL(fileURLWithPath: "/b")]), side: .right)
    await editor.awaitReadinessCheck()
    #expect(editor.readinessWarnings.isEmpty)
    #expect(editor.recordsWarning == "Folder B can't hold Foldover's .foldover folder, so it can only be a Backup's target. Choose a Backup to Folder B, then keep nothing in it.")
    #expect(!editor.offersKeepingNothing)
    #expect(!editor.canSave)

    editor.mode = .backupLeft
    await editor.awaitReadinessCheck()
    #expect(editor.offersKeepingNothing)
    #expect(editor.recordsWarning == "Folder B can't hold Foldover's .foldover folder. To back up to it, turn on “Keep nothing in Folder B”.")
    #expect(!editor.canSave)
    editor.keepsNothingInTarget = true
    await editor.awaitReadinessCheck()
    #expect(editor.recordsWarning == nil)
    #expect(editor.offersKeepingNothing)
    #expect(editor.canSave)
    #expect(editor.planText.contains("files replaced in Folder B are deleted, not archived"))

    // Leaving Backup keeps records in both folders again.
    editor.mode = .sync
    #expect(editor.options.records == .both)
    await editor.awaitReadinessCheck()
    #expect(!editor.canSave)
}

/// Review of #112: Save waits for a check still running, so a folder that
/// refuses records cannot slip through before the answer.
@MainActor
@Test
func savingWaitsForTheRecordsCheck() async {
    let editor = PairEditorViewModel(store: PairStore(client: RecordsRefusingClient()))
    editor.name = "Archive"
    editor.chooseFolder(.success([URL(fileURLWithPath: "/a")]), side: .left)
    editor.chooseFolder(.success([URL(fileURLWithPath: "/b")]), side: .right)
    #expect(await editor.save() == false)
    #expect(editor.recordsWarning != nil)
}

/// Refuses records in Folder B, and like the engine does not probe a
/// Backup target that already keeps none.
private actor RecordsRefusingClient: PairClient {
    func page(after: String?) async throws -> PairCatalogPage { PairCatalogPage(pairs: [], next: nil) }
    func create(_ settings: PairSettings) async throws -> SyncPair { throw PairFixtureError.changedElsewhere }
    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair { throw PairFixtureError.changedElsewhere }
    func remove(_ pair: SyncPair) async throws -> SyncPair { throw PairFixtureError.changedElsewhere }
    func check(_ settings: PairSettings) async throws -> [PairReadinessWarning] {
        let spared = settings.mode == .backupLeft && settings.options.records == .sourceOnly
        return spared ? [] : [PairReadinessWarning(side: .b, kind: .recordsUnsupported)]
    }
    func servers() async throws -> [NearbyServer] { [] }
    func browse(_ locator: String) async throws -> ServerListing { throw PairFixtureError.changedElsewhere }
}

@MainActor
@Test
func aFailedReadinessCheckKeepsTheLastAnswer() async {
    let client = WarningOnAttributesClient(warning: PairReadinessWarning(side: .a, kind: .namesDiffer))
    let editor = PairEditorViewModel(store: PairStore(client: client))
    editor.chooseFolder(.success([URL(fileURLWithPath: "/a")]), side: .left)
    editor.chooseFolder(.success([URL(fileURLWithPath: "/b")]), side: .right)
    await editor.awaitReadinessCheck()
    #expect(editor.readinessWarnings.count == 1)
    let failing = PairEditorViewModel(store: PairStore(client: FailingPairPageClient()))
    failing.chooseFolder(.success([URL(fileURLWithPath: "/a")]), side: .left)
    failing.chooseFolder(.success([URL(fileURLWithPath: "/b")]), side: .right)
    await failing.awaitReadinessCheck()
    #expect(failing.readinessWarnings.isEmpty)
    #expect(failing.errorMessage == nil)
}

@MainActor
@Test
func pairEditorListsServersFoundNearbyAndBuildsTheirConnectURLs() async {
    let nas = NearbyServer(name: "NAS", host: "nas.local", port: 445)
    let old = NearbyServer(name: "Old", host: "old.local", port: 139)
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient(servers: [nas, old])))
    await editor.awaitNearbyServers()
    #expect(editor.nearbyServers == [nas, old])
    #expect(editor.serverURL(nas)?.absoluteString == "smb://nas.local")
    #expect(editor.serverURL(old)?.absoluteString == "smb://old.local:139")
}

@MainActor
@Test
func pairEditorWalksServerFoldersAndChoosesOne() async throws {
    let home = ServerListing(
        locator: "sftp://nas/home/alice", parent: "sftp://nas/home",
        folders: [.init(name: "Photos", locator: "sftp://nas/home/alice/Photos")]
    )
    let photos = ServerListing(locator: "sftp://nas/home/alice/Photos", parent: home.locator, folders: [])
    let client = PairFixtureClient(listings: ["sftp://nas/~": home, photos.locator: photos])
    let editor = PairEditorViewModel(store: PairStore(client: client))

    editor.serverLocator = "nas.local"
    await editor.browseServer()
    #expect(editor.serverBrowseError != nil)
    #expect(await client.browsed.isEmpty)

    editor.serverLocator = " sftp://nas/~ "
    await editor.browseServer()
    #expect(editor.serverListing == home)
    #expect(editor.serverBrowseError == nil)
    #expect(!editor.isBrowsingServer)
    #expect(await client.browsed == ["sftp://nas/~"])

    let folder = try #require(editor.serverListing?.folders.first)
    await editor.openServerFolder(folder.locator)
    #expect(editor.serverListing == photos)
    #expect(editor.useServerFolder(side: .right))
    #expect(editor.right == SelectedFolder(bytes: Data("sftp://nas/home/alice/Photos".utf8)))

    // A server that does not answer leaves the last listing up.
    await editor.openServerFolder("sftp://nas/gone")
    #expect(editor.serverBrowseError != nil)
    #expect(editor.serverListing == photos)

    editor.resetServerBrowser()
    #expect(editor.serverListing == nil && editor.serverLocator.isEmpty && editor.serverBrowseError == nil)
    #expect(!editor.useServerFolder(side: .left))
    #expect(editor.serverBrowseError != nil)
    #expect(editor.left == nil)
}

@MainActor
@Test
func aFailedServerBrowseKeepsTheLastAnswer() async {
    let nas = NearbyServer(name: "NAS", host: "nas.local", port: 445)
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient(servers: [nas])))
    await editor.awaitNearbyServers()
    #expect(editor.nearbyServers == [nas])

    let failing = PairEditorViewModel(store: PairStore(client: FailingPairPageClient()))
    await failing.awaitNearbyServers()
    #expect(failing.nearbyServers.isEmpty)
    #expect(failing.errorMessage == nil)
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

@MainActor
@Test
func planSentenceSaysWhatThePairWillDo() {
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()))
    #expect(editor.planText == "Changes in either folder go to the other. A file deleted in one folder is archived in the other. Syncs only when you click Sync Now.")

    editor.options.schedule = .onChange
    #expect(editor.planText == "Changes in either folder go to the other. A file deleted in one folder is archived in the other. Syncs when files change.")

    editor.mode = .backupLeft
    editor.options.schedule = .daily(minutes: 120, days: Set(SyncDay.allCases))
    #expect(editor.planText == "Folder A is copied to Folder B. Files you delete in A stay in B. Syncs every day at 02:00.")

    editor.mode = .mirrorLeft
    #expect(editor.planText == "Folder B is made identical to A. Files you delete in A are archived in B, never deleted. Syncs every day at 02:00.")

    editor.mode = .mirrorRight
    editor.options.schedule = .daily(minutes: 18 * 60, days: [.monday, .tuesday, .wednesday, .thursday, .friday])
    #expect(editor.planText == "Folder A is made identical to B. Files you delete in B are archived in A, never deleted. Syncs weekdays at 18:00.")

    editor.mode = .backupRight
    editor.options.schedule = .daily(minutes: 7 * 60 + 15, days: [.monday, .wednesday, .friday])
    #expect(editor.planText == "Folder B is copied to Folder A. Files you delete in B stay in A. Syncs Mon, Wed, Fri at 07:15.")

    editor.options.schedule = .every(minutes: 15)
    #expect(editor.planText.hasSuffix("Syncs every 15 minutes."))
    editor.options.schedule = .every(minutes: 60)
    #expect(editor.planText.hasSuffix("Syncs every hour."))
}

@MainActor
@Test
func planSentenceListsOnlyUnusualChoices() {
    let editor = PairEditorViewModel(store: PairStore(client: PairFixtureClient()))
    // Defaults add nothing to the second sentence.
    #expect(!editor.planText.contains(";"))

    editor.options.syncPermissions = true
    #expect(editor.planText.contains("archived in the other; permissions and access lists sync."))

    editor.options.filters = ["*.tmp", "", "build/"]
    editor.options.sizeLimit = 1_000_000_000
    editor.options.ageLimit = 30
    editor.options.canary = Data("docs/canary.txt".utf8)
    #expect(editor.planText.contains("skipped: 2 rules, files larger than 1 GB, files unchanged for more than 30 days"))
    #expect(editor.planText.contains("the canary file docs/canary.txt must stay unchanged in both folders"))
    // Three sentences at most, however much is unusual.
    #expect(editor.planText.split(separator: ". ").count <= 3)

    editor.options.syncExtendedAttributes = false
    #expect(editor.planText.contains("tags and other extended attributes don't sync"))
    editor.options.syncModificationTimes = false
    #expect(editor.planText.contains("modification dates don't sync"))
}
