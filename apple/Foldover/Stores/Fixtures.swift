#if DEBUG
// Canned agent answers for UI tours and screenshots. A Debug build
// launched with `-foldover-fixtures` gives every store one of these
// clients instead of the agent's, so the app shows realistic pairs,
// previews, runs and archives with no agent running. Release builds
// never compile this file's contents.

import FoldoverSystem
import Foundation

enum Fixtures {
    static let launchArgument = "-foldover-fixtures"
    /// Shows the app in Dark Mode whatever the system uses.
    static let darkArgument = "-foldover-dark"
    /// With the launch argument above, starts from no saved preferences,
    /// as on a fresh install: AppKit keeps the window's size and the
    /// sidebar's state there, and a tour must not inherit them from an
    /// earlier run.
    static let resetArgument = "-foldover-reset-defaults"

    static var isEnabled: Bool { ProcessInfo.processInfo.arguments.contains(launchArgument) }

    static let photosID = "f0000000000000000000000000000001"
    static let projectsID = "f0000000000000000000000000000002"
    static let documentsID = "f0000000000000000000000000000003"
    static let musicID = "f0000000000000000000000000000004"

    /// Local folders the pairs point at, with a few real files so Quick
    /// Look and Show in Finder have something to show.
    static let root = FileManager.default.temporaryDirectory.appending(path: "Foldover Tour")

    /// Files a local side holds, relative to its folder.
    private static let files: [String: [String]] = [
        "Photos": ["2026/Trip/day2/IMG_2041.txt", "2026/Trip/notes.md", "2026/Trip/header.txt",
                   "2025/Family/IMG_0042.txt"],
        "Projects": ["Website/index.html", "Website/style.css", "Budget.txt"],
        "Documents": ["Letters/landlord.txt", "Taxes/2025/summary.txt"],
        "Music": ["Playlists/Running.txt"],
    ]

    /// Local folders a pair points at that start out empty.
    private static let emptyFolders = ["Backup SSD/Projects"]

    /// Create the tour's folders; existing files are left alone.
    static func prepareFolders() {
        let manager = FileManager.default
        for folder in emptyFolders {
            try? manager.createDirectory(at: root.appending(path: folder), withIntermediateDirectories: true)
        }
        for (folder, paths) in files {
            for path in paths {
                let url = root.appending(path: folder).appending(path: path)
                try? manager.createDirectory(at: url.deletingLastPathComponent(),
                                             withIntermediateDirectories: true)
                if !manager.fileExists(atPath: url.path(percentEncoded: false)) {
                    try? Data("\(path)\n\nA file from Foldover's UI tour.\n".utf8).write(to: url)
                }
            }
        }
    }

    static func local(_ folder: String) -> SelectedFolder {
        SelectedFolder(bytes: Data(root.appending(path: folder).path(percentEncoded: false).utf8))
    }

    static func remote(_ locator: String) -> SelectedFolder {
        SelectedFolder(bytes: Data(locator.utf8))
    }

    static func pairs() -> [SyncPair] {
        var photos = PairOptions.defaults
        photos.schedule = .daily(minutes: 18 * 60, days: Set(SyncDay.allCases))
        photos.filters = ["*.tmp", ".cache/"]
        var projects = PairOptions.defaults
        projects.schedule = .onChange
        var documents = PairOptions.defaults
        documents.schedule = .every(minutes: 15)
        var music = PairOptions.defaults
        music.schedule = .manual
        return [
            SyncPair(id: photosID, revision: 1, settings: PairSettings(
                name: "Photos", left: local("Photos"), right: remote("sftp://mynas.local/~/Photos"),
                mode: .sync, options: photos)),
            SyncPair(id: projectsID, revision: 1, settings: PairSettings(
                name: "Projects", left: local("Projects"), right: local("Backup SSD/Projects"),
                mode: .backupLeft, options: projects)),
            SyncPair(id: documentsID, revision: 1, settings: PairSettings(
                name: "Documents", left: local("Documents"), right: remote("peer://studio/share-1"),
                mode: .sync, options: documents)),
            SyncPair(id: musicID, revision: 1, settings: PairSettings(
                name: "Music", left: local("Music"), right: remote("/Volumes/Media/Music"),
                mode: .mirrorLeft, options: music)),
        ]
    }

    static func change(_ path: String, _ action: ChangeAction, bytes: UInt64 = 0,
                       movedFrom: String? = nil) -> PlannedChange {
        PlannedChange(path: Data(path.utf8), action: action, bytes: bytes,
                      movedFrom: movedFrom.map { Data($0.utf8) })
    }

    /// What a preview of each pair finds.
    static func changes(for id: String) -> [PlannedChange] {
        switch id {
        case photosID:
            [
                change("2026/Trip/header.txt", .conflict(.bothChanged), bytes: 96_200_000),
                change("2026/Trip/itinerary.pdf", .conflict(.deletedVersusEdited), bytes: 1_240_000),
                change("2026/Trip/day2", .conflict(.typeMismatch)),
                change("2026/Trip/day2/IMG_2041.txt", .copy(to: .b), bytes: 41_000_000),
                change("2026/Trip/day2/IMG_2042.RAW", .copy(to: .b), bytes: 39_500_000),
                change("2026/Trip/day2/IMG_2043.RAW", .copy(to: .b), bytes: 40_100_000),
                change("2026/Trip/notes.md", .copy(to: .a), bytes: 12_400),
                change("2026/Trip/day1/IMG_1990.HEIC", .move(on: .b), bytes: 3_100_000,
                       movedFrom: "2026/Inbox/IMG_1990.HEIC"),
                change("2025/Family/IMG_0042.txt", .update(to: .b)),
                change("2024/drafts/old-banner.psd", .archive(on: .b), bytes: 66_000_000),
                change("2024/drafts/banner-v2.psd", .archive(on: .b), bytes: 71_500_000),
                change("2026/Trip/day3", .blocked),
            ]
        case projectsID:
            [
                change("Website/index.html", .copy(to: .b), bytes: 18_300),
                change("Website/style.css", .copy(to: .b), bytes: 6_200),
                change("Budget.txt", .update(to: .b)),
                change("Website/old/index.html", .keep, bytes: 15_900),
            ]
        case documentsID:
            [change("Letters/landlord.txt", .copy(to: .b), bytes: 2_400)]
        default:
            []
        }
    }

    static func summary(_ changes: [PlannedChange], token: String, revision: UInt64) -> PreviewSummary {
        func count(_ matches: (ChangeAction) -> Bool) -> UInt64 {
            UInt64(changes.filter { matches($0.action) }.count)
        }
        let archivesA = count { $0 == .archive(on: .a) }
        let archivesB = count { $0 == .archive(on: .b) }
        let copyBytes = changes.reduce(UInt64(0)) { total, change in
            if case .copy = change.action { return total + change.bytes }
            return total
        }
        return PreviewSummary(
            changes: changes,
            copies: count { if case .copy = $0 { true } else { false } },
            updates: count { if case .update = $0 { true } else { false } },
            archives: archivesA + archivesB,
            conflicts: count { if case .conflict = $0 { true } else { false } },
            blocked: count { $0 == .blocked },
            kept: count { $0 == .keep },
            a: DeletionCheck(deletions: archivesA, paths: archivesA, needsApproval: false),
            b: DeletionCheck(deletions: archivesB, paths: archivesB, needsApproval: false),
            copyBytes: copyBytes,
            reviewToken: token,
            pairRevision: revision,
            moves: count { if case .move = $0 { true } else { false } }
        )
    }

    static func history(for id: String) -> [RunRecord] {
        let hour: TimeInterval = 3600
        func run(_ hoursAgo: Double, _ trigger: RunTrigger, _ end: RunEnd, copied: UInt64 = 0,
                 updated: UInt64 = 0, archived: UInt64 = 0, conflicts: UInt64 = 0,
                 failures: UInt64 = 0, message: String? = nil) -> RunRecord {
            let started = Date(timeIntervalSinceNow: -hoursAgo * hour)
            return RunRecord(started: started, finished: started.addingTimeInterval(45), trigger: trigger,
                             end: end, copied: copied, updated: updated, archived: archived, recovered: 0,
                             conflicts: conflicts, failures: failures, message: message)
        }
        switch id {
        case photosID:
            return [
                run(20, .scheduled, .completed, copied: 214, updated: 3, archived: 12, conflicts: 1),
                run(44, .scheduled, .completed),
                run(68, .manual, .completed, copied: 1_092, archived: 4),
                run(92, .scheduled, .failed, message: "mynas.local did not answer."),
            ]
        case projectsID:
            return [
                run(0.2, .manual, .completed, copied: 2, updated: 1),
                run(5, .scheduled, .completed, copied: 18),
                run(27, .scheduled, .needsApproval),
            ]
        default:
            return []
        }
    }
}

/// A launch state for the layout checks, from `-foldover-scenario
/// <name>`: states a real agent produces that the tour's instant,
/// healthy agent does not. Without one, the tour's usual state.
enum FixtureScenario: String {
    /// The agent answers after two seconds, as a real one does at launch,
    /// so the window starts out connecting.
    case slowAgent = "slow-agent"
    /// The agent stopped repeatedly and waits in crash-loop safe mode.
    case safeMode = "safe-mode"
    /// The agent cannot be reached.
    case agentFailed = "agent-failed"
    /// The catalog is empty, so the setup guide shows.
    case noPairs = "no-pairs"
    /// The catalog cannot load.
    case pairsFail = "pairs-fail"

    static var current: FixtureScenario? {
        UserDefaults.standard.string(forKey: "foldover-scenario").flatMap(FixtureScenario.init)
    }

    func agentReport() async throws -> AgentReport {
        switch self {
        case .slowAgent:
            try await Task.sleep(for: .seconds(2))
            return AgentReport(version: "0.1.0")
        case .safeMode:
            return AgentReport(version: "0.1.0", safeModeStops: 3)
        case .agentFailed:
            throw FixtureError.agentUnavailable
        case .noPairs, .pairsFail:
            return AgentReport(version: "0.1.0")
        }
    }
}

private enum FixtureError: LocalizedError {
    case missingPair
    case notAvailable
    case agentUnavailable
    case catalogUnavailable

    var errorDescription: String? {
        switch self {
        case .missingPair: "The tour has no such pair."
        case .notAvailable: "The tour cannot do that."
        case .agentUnavailable: "The agent’s socket refused the connection, so its answer about the pairs and their state is not available yet."
        case .catalogUnavailable: "The agent could not read its pair catalog: the database is locked by another process."
        }
    }
}

actor FixturePairClient: PairClient {
    private let scenario: FixtureScenario?
    private var pairs: [SyncPair]

    init(scenario: FixtureScenario? = nil) {
        self.scenario = scenario
        pairs = scenario == .noPairs ? [] : Fixtures.pairs()
    }

    func revision(of id: String) -> UInt64 {
        pairs.first { $0.id == id }?.revision ?? 0
    }

    func page(after: String?) async throws -> PairCatalogPage {
        if scenario == .pairsFail { throw FixtureError.catalogUnavailable }
        return PairCatalogPage(pairs: after == nil ? pairs : [], next: nil)
    }

    func create(_ settings: PairSettings) async throws -> SyncPair {
        let pair = SyncPair(id: "tour-\(UUID().uuidString)", revision: 1, settings: settings)
        pairs.append(pair)
        return pair
    }

    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair {
        guard let index = pairs.firstIndex(where: { $0.id == pair.id }) else { throw FixtureError.missingPair }
        let updated = SyncPair(id: pair.id, revision: pairs[index].revision + 1, settings: settings)
        pairs[index] = updated
        return updated
    }

    func remove(_ pair: SyncPair) async throws -> SyncPair {
        pairs.removeAll { $0.id == pair.id }
        return pair
    }

    func check(_ settings: PairSettings) async throws -> [PairReadinessWarning] { [] }

    func servers() async throws -> [NearbyServer] {
        [NearbyServer(name: "MyNAS", host: "mynas.local", port: 445)]
    }

    func browse(_ locator: String) async throws -> ServerListing {
        let base = locator.hasSuffix("/") ? String(locator.dropLast()) : locator
        return ServerListing(locator: locator, parent: "sftp://mynas.local/~", folders: [
            ServerListing.Folder(name: "Photos", locator: "\(base)/Photos"),
            ServerListing.Folder(name: "Backups", locator: "\(base)/Backups"),
            ServerListing.Folder(name: "Shared", locator: "\(base)/Shared"),
        ])
    }
}

/// Previews and runs that take a few polls, so progress and Stop show.
actor FixtureSyncClient: SyncClient {
    private enum Job {
        case preview
        case run
    }

    private let pairs: FixturePairClient
    private var states: [String: PairSyncState] = [
        Fixtures.projectsID: .finished(RunSummary(
            cancelled: false, copied: 2, updated: 1, archived: 0, recovered: 0, conflicts: 0,
            failures: [], failureCount: 0, archive: "2026-10-22T09-41-00Z")),
        Fixtures.musicID: .waiting(.b),
    ]
    private var histories: [String: [RunRecord]] = [
        Fixtures.photosID: Fixtures.history(for: Fixtures.photosID),
        Fixtures.projectsID: Fixtures.history(for: Fixtures.projectsID),
    ]
    private var excluded: [String: Set<Data>] = [:]
    private var jobs: [String: (job: Job, polls: Int)] = [:]
    private var tokens = 0
    /// Runs finished so far; each names its own archive folder, as the
    /// engine's millisecond run names do.
    private var runs = 0
    private static let polls = 6

    init(pairs: FixturePairClient) {
        self.pairs = pairs
    }

    private func plan(for id: String) async -> PreviewSummary {
        tokens += 1
        let skipped = excluded[id] ?? []
        let changes = Fixtures.changes(for: id).filter { !skipped.contains($0.path) }
        return Fixtures.summary(changes, token: "tour-\(tokens)", revision: await pairs.revision(of: id))
    }

    private func progress(_ job: Job, polls: Int, for id: String) -> WorkProgress {
        let done = UInt64(Self.polls - polls)
        let steps = UInt64(Self.polls)
        switch job {
        case .preview:
            return WorkProgress(listed: 48_211, checked: 48_211 * done / steps)
        case .run:
            let total = UInt64(Fixtures.changes(for: id).count)
            return WorkProgress(listed: 48_211, checked: 48_211, changes: total, applied: total * done / steps,
                                bytes: 120_600_000, copied: 120_600_000 * done / steps)
        }
    }

    func preview(_ id: String) async throws -> PairSyncState {
        jobs[id] = (.preview, Self.polls)
        let state = PairSyncState.working(.preview, cancelling: false, progress: progress(.preview, polls: Self.polls, for: id))
        states[id] = state
        return state
    }

    func run(_ id: String, approvedA: UInt64, approvedB: UInt64,
             reviewToken: String?) async throws -> PairSyncState {
        jobs[id] = (.run, Self.polls)
        let state = PairSyncState.working(.run, cancelling: false, progress: progress(.run, polls: Self.polls, for: id))
        states[id] = state
        return state
    }

    func status(_ id: String) async throws -> PairSyncState {
        guard let current = jobs[id] else { return states[id] ?? .idle }
        let job = current.job
        let polls = current.polls
        if polls > 1 {
            jobs[id] = (job, polls - 1)
            let state = PairSyncState.working(job == .preview ? .preview : .run, cancelling: false,
                                              progress: progress(job, polls: polls - 1, for: id))
            states[id] = state
            return state
        }
        jobs[id] = nil
        switch job {
        case .preview:
            states[id] = .previewed(await plan(for: id))
        case .run:
            states[id] = .finished(finish(id, cancelled: false))
        }
        return states[id] ?? .idle
    }

    private func finish(_ id: String, cancelled: Bool) -> RunSummary {
        let changes = Fixtures.changes(for: id).filter { !(excluded[id] ?? []).contains($0.path) }
        func count(_ matches: (ChangeAction) -> Bool) -> UInt64 {
            UInt64(changes.filter { matches($0.action) }.count)
        }
        let copied = count { if case .copy = $0 { true } else { false } }
        let updated = count { if case .update = $0 { true } else { false } }
        let archived = count { if case .archive = $0 { true } else { false } }
        let conflicts = count { if case .conflict = $0 { true } else { false } }
        let started = Date()
        runs += 1
        let millis = String(runs % 1_000)
        let archive = "2026-10-22T10-15-00.\(String(repeating: "0", count: 3 - millis.count))\(millis)Z"
        histories[id, default: []].insert(RunRecord(
            started: started, finished: started, trigger: .manual, end: cancelled ? .cancelled : .completed,
            copied: copied, updated: updated, archived: archived, recovered: 0, conflicts: conflicts,
            failures: 0, message: nil), at: 0)
        return RunSummary(cancelled: cancelled, copied: copied, updated: updated, archived: archived,
                          recovered: 0, conflicts: conflicts, failures: [], failureCount: 0,
                          archive: archive)
    }

    func cancel(_ id: String) async throws -> PairSyncState {
        guard let current = jobs[id] else { return states[id] ?? .idle }
        jobs[id] = nil
        states[id] = current.job == .run ? .finished(finish(id, cancelled: true)) : .idle
        return states[id] ?? .idle
    }

    func undo(_ id: String) async throws -> PairSyncState {
        let started = Date()
        histories[id, default: []].insert(RunRecord(
            started: started, finished: started, trigger: .undo, end: .completed, copied: 0, updated: 0,
            archived: 2, recovered: 1, conflicts: 0, failures: 0, message: nil), at: 0)
        let state = PairSyncState.undone(UndoSummary(
            cancelled: false, removed: 2, restored: 1, beside: 0, notUndone: [], notUndoneCount: 0,
            archive: "2026-10-22T10-16-00Z"))
        states[id] = state
        return state
    }

    func resolve(_ id: String, path: Data, keep: FolderSide,
                 reviewToken: String) async throws -> PairSyncState {
        excluded[id, default: []].insert(path)
        return states[id] ?? .idle
    }

    func keepBoth(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState {
        excluded[id, default: []].insert(path)
        return states[id] ?? .idle
    }

    func skipOnce(_ id: String, path: Data, reviewToken: String) async throws -> PairSyncState {
        excluded[id, default: []].insert(path)
        return states[id] ?? .idle
    }

    func history(_ id: String) async throws -> [RunRecord] { histories[id] ?? [] }

    func changes(_ id: String, after: Data?, filter: ChangeFilter?,
                 reviewToken: String) async throws -> ChangePage {
        guard after == nil else { return ChangePage(changes: [], more: false) }
        let skipped = excluded[id] ?? []
        let changes = Fixtures.changes(for: id).filter { change in
            guard !skipped.contains(change.path) else { return false }
            switch (filter, change.action) {
            case (nil, _), (.copies, .copy), (.updates, .update), (.archives, .archive),
                 (.conflicts, .conflict), (.moves, .move):
                return true
            default:
                return false
            }
        }
        return ChangePage(changes: changes, more: false)
    }
}

actor FixtureArchiveClient: ArchiveClient {
    private var restored: Set<Data> = []

    func runs(_ id: String, side: FolderSide) async throws -> ArchiveRuns {
        guard side == .b else { return ArchiveRuns(runs: [], more: false) }
        return ArchiveRuns(runs: [
            ArchiveRun(name: "2026-10-21T18-00-03Z", started: Date(timeIntervalSinceNow: -20 * 3600)),
            ArchiveRun(name: "2026-10-19T18-00-01Z", started: Date(timeIntervalSinceNow: -68 * 3600)),
        ], more: true)
    }

    func items(_ id: String, side: FolderSide, run: String, after: Data?) async throws -> ArchivePage {
        guard after == nil else { return ArchivePage(items: [], more: false) }
        let items = [
            ArchiveItem(path: Data("2024/drafts/old-banner.psd".utf8), size: 66_000_000),
            ArchiveItem(path: Data("2024/drafts/banner-v1.psd".utf8), size: 58_400_000),
            ArchiveItem(path: Data("2026/Trip/day2/IMG_2001.RAW".utf8), size: 40_800_000),
            ArchiveItem(path: Data("2026/Trip/notes.md".utf8), size: 11_900),
        ]
        return ArchivePage(items: items.filter { !restored.contains($0.path) }, more: false)
    }

    func putBack(_ id: String, side: FolderSide, run: String, path: Data) async throws -> Data {
        restored.insert(path)
        return path
    }

    func putBackAll(_ id: String, side: FolderSide, run: String) async throws -> RunRestore {
        RunRestore(restored: 3, beside: 1, failures: 0)
    }
}

actor FixtureAutomationClient: AutomationClient {
    private var status = AutomationStatus(state: .running)

    func state() async throws -> AutomationStatus { status }

    func pause(minutes: UInt32?) async throws -> AutomationStatus {
        status.state = .paused(until: minutes.map { Date(timeIntervalSinceNow: Double($0) * 60) })
        return status
    }

    func resume() async throws -> AutomationStatus {
        status.state = .running
        return status
    }

    func setLowPower(_ syncs: LowPowerSyncs) async throws -> AutomationStatus {
        status.lowPower = syncs
        return status
    }
}

actor FixtureMacToMacClient: MacToMacClient {
    private var pairingOpen = false
    private var pending: [PendingCode] = []
    private var paired = [PairedDevice(device: "studio", name: "Mac Studio",
                                       paired: Date(timeIntervalSinceNow: -30 * 86_400))]
    private var shared = [FolderShare(id: "share-1", device: "studio",
                                      path: Data(Fixtures.root.appending(path: "Documents").path(percentEncoded: false).utf8),
                                      name: "Documents")]

    private var current: MacToMacState {
        MacToMacState(
            isOn: true, name: "MacBook Pro", pairingOpen: pairingOpen, pending: pending, paired: paired,
            nearby: [
                NearbyDevice(device: "studio", name: "Mac Studio", openForPairing: false, paired: true),
                NearbyDevice(device: "mini", name: "Mac mini", openForPairing: true, paired: false),
            ],
            discoveryProblem: nil)
    }

    func state() async throws -> MacToMacState { current }
    func start() async throws -> MacToMacState { current }

    func openPairing() async throws -> MacToMacState {
        pairingOpen = true
        return current
    }

    func closePairing() async throws -> MacToMacState {
        pairingOpen = false
        return current
    }

    func pair(device: String) async throws -> PendingCode {
        let code = PendingCode(device: device, name: "Mac mini", code: 482_913)
        pending = [code]
        return code
    }

    func confirm(device: String) async throws -> MacToMacState {
        pending.removeAll { $0.device == device }
        paired.append(PairedDevice(device: device, name: "Mac mini", paired: Date()))
        return current
    }

    func cancel(device: String) async throws -> MacToMacState {
        pending.removeAll { $0.device == device }
        return current
    }

    func forget(device: String) async throws -> MacToMacState {
        paired.removeAll { $0.device == device }
        return current
    }

    func shares() async throws -> [FolderShare] { shared }

    func share(path: Data, name: String, device: String) async throws -> FolderShare {
        let share = FolderShare(id: "share-\(shared.count + 1)", device: device, path: path, name: name)
        shared.append(share)
        return share
    }

    func stopSharing(id: String) async throws -> [FolderShare] {
        shared.removeAll { $0.id == id }
        return shared
    }

    func offered(device: String) async throws -> [OfferedShare] {
        guard device == "studio" else { throw FixtureError.notAvailable }
        return [OfferedShare(id: "share-1", name: "Documents"), OfferedShare(id: "share-2", name: "Design")]
    }
}

/// The agent's login item in a tour: on, and never registered anew, so a
/// tour leaves the system's login items alone.
@MainActor
final class FixtureAgentLoginItem: AgentRegistering {
    var registration: AgentRegistration { .enabled }
    var agentBuild: Data? { nil }

    func register() throws -> AgentRegistration { .enabled }
    func reregister() async throws -> AgentRegistration { .enabled }
    func openLoginItems() {}
}

/// Login-item state kept in memory.
@MainActor
final class FixtureLoginItem: LoginItemControl {
    private(set) var status: LoginStatus = .enabled

    func enable() throws { status = .enabled }
    func disable() throws { status = .disabled }
}
#endif
