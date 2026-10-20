import Darwin
import FoldoverCore
import Foundation
import Observation

enum PairMode: CaseIterable, Hashable, Sendable {
    case sync
    case mirrorLeft
    case mirrorRight
    case backupLeft
    case backupRight
}

struct SelectedFolder: Equatable, Sendable {
    let bytes: Data
}

/// When the agent syncs without being asked, after the first manual sync.
enum SyncSchedule: Hashable, Sendable {
    case manual
    case every(minutes: UInt32)
    /// After files change, and hourly.
    case onChange
    /// At `minutes` after local midnight, on each day in `days`.
    case daily(minutes: UInt32, days: Set<SyncDay>)
}

/// A weekday in a daily schedule, Monday first.
enum SyncDay: CaseIterable, Hashable, Sendable {
    case monday
    case tuesday
    case wednesday
    case thursday
    case friday
    case saturday
    case sunday
}

enum PairDurability: CaseIterable, Hashable, Sendable {
    case standard
    case full
}

/// Which folders hold Foldover's records (`.foldover`): its identity,
/// lease and archive.
enum PairRecords: Hashable, Sendable {
    case both
    /// Only a Backup's source, for a target that cannot hold `.foldover`:
    /// files a sync replaces there are deleted, not archived.
    case sourceOnly
}

/// Metadata categories and flush strength a pair uses.
struct PairOptions: Equatable, Sendable {
    var syncModificationTimes: Bool
    var syncExtendedAttributes: Bool
    var syncPermissions: Bool
    var durability: PairDurability
    var schedule: SyncSchedule
    /// Gitignore-style exclusion rules, applied to both folders.
    var filters: [String]
    /// Files larger than this many bytes are skipped in both folders.
    var sizeLimit: UInt64?
    /// Files unchanged for more than this many days in both folders are skipped.
    var ageLimit: UInt32?
    /// A file kept unchanged in both folders that proves the right ones
    /// are mounted; syncs stop before writing if it is missing or changed.
    var canary: Data?
    var records: PairRecords
    /// File data bytes per second the pair's copies share at most.
    var bandwidthLimit: UInt64?

    /// The engine's defaults, so the app never restates them.
    static var defaults: PairOptions { pairOptions(FoldoverCore.defaultPairOptions()) }

    func syncsSameMetadata(as other: PairOptions) -> Bool {
        syncModificationTimes == other.syncModificationTimes
            && syncExtendedAttributes == other.syncExtendedAttributes
            && syncPermissions == other.syncPermissions
    }
}

struct PairSettings: Equatable, Sendable {
    let name: String
    let left: SelectedFolder
    let right: SelectedFolder
    let mode: PairMode
    let options: PairOptions

    init(name: String, left: SelectedFolder, right: SelectedFolder, mode: PairMode,
         options: PairOptions = .defaults) {
        self.name = name
        self.left = left
        self.right = right
        self.mode = mode
        self.options = options
    }
}

struct SyncPair: Identifiable, Equatable, Sendable {
    let id: String
    let revision: UInt64
    let settings: PairSettings
}

struct PairCatalogPage: Sendable {
    let pairs: [SyncPair]
    let next: String?
}

/// What will not work as a person might expect with a picked folder
/// (PLAN.md §3.6). The agent saves regardless; the editor blocks saving
/// only over refused records.
struct PairReadinessWarning: Equatable, Sendable {
    enum Kind: Equatable, Sendable {
        /// Another sync engine manages this folder; `provider` names it.
        case cloudManaged(provider: String)
        /// The pair syncs extended attributes this side cannot keep.
        case metadataUnsupported
        /// One side ignores name case and the other does not.
        case namesDiffer
        /// This folder cannot hold Foldover's `.foldover` records, so it
        /// can only be a Backup target that keeps none.
        case recordsUnsupported
    }

    let side: FolderSide
    let kind: Kind
}

/// An SMB file server seen on the local network (PLAN.md §3.8).
struct NearbyServer: Equatable, Sendable, Identifiable {
    let name: String
    /// Where SMB connects, e.g. "nas.local".
    let host: String
    let port: UInt16

    var id: String { "\(host):\(port)" }
}

protocol PairClient: Sendable {
    func page(after: String?) async throws -> PairCatalogPage
    func create(_ settings: PairSettings) async throws -> SyncPair
    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair
    func remove(_ pair: SyncPair) async throws -> SyncPair
    func check(_ settings: PairSettings) async throws -> [PairReadinessWarning]
    /// Waits about two seconds while the agent browses the network.
    func servers() async throws -> [NearbyServer]
}

// Keep generated async values inside one actor; only native Sendable records leave it.
private actor CorePairClient: PairClient {
    func page(after: String?) async throws -> PairCatalogPage {
        let page = try await FoldoverCore.pairPage(after: after)
        return PairCatalogPage(pairs: page.pairs.map(pair), next: page.next)
    }

    func create(_ settings: PairSettings) async throws -> SyncPair {
        pair(try await FoldoverCore.pairCreate(configuration: configuration(settings)))
    }

    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair {
        self.pair(try await FoldoverCore.pairUpdate(
            id: pair.id, revision: pair.revision, configuration: configuration(settings)
        ))
    }

    func remove(_ pair: SyncPair) async throws -> SyncPair {
        self.pair(try await FoldoverCore.pairRemove(id: pair.id, revision: pair.revision))
    }

    func check(_ settings: PairSettings) async throws -> [PairReadinessWarning] {
        try await FoldoverCore.checkPair(configuration: configuration(settings)).map(warning)
    }

    func servers() async throws -> [NearbyServer] {
        try await FoldoverCore.nearbyFileServers().map {
            NearbyServer(name: $0.name, host: $0.host, port: $0.port)
        }
    }

    private func warning(_ warning: ReadinessWarning) -> PairReadinessWarning {
        let side: FolderSide = warning.side == .left ? .a : .b
        let kind: PairReadinessWarning.Kind = switch warning.kind {
        case .cloudManaged: .cloudManaged(provider: warning.provider ?? "")
        case .metadataUnsupported: .metadataUnsupported
        case .namesDiffer: .namesDiffer
        case .recordsUnsupported: .recordsUnsupported
        }
        return PairReadinessWarning(side: side, kind: kind)
    }

    private func configuration(_ settings: PairSettings) -> LocalPairDraft {
        LocalPairDraft(name: settings.name, leftRoot: settings.left.bytes,
                       rightRoot: settings.right.bytes, mode: coreMode(settings.mode),
                       options: corePairOptions(settings.options))
    }

    private func pair(_ pair: LocalPairConfiguration) -> SyncPair {
        let draft = pair.configuration
        return SyncPair(id: pair.id, revision: pair.revision, settings: PairSettings(
            name: draft.name, left: SelectedFolder(bytes: draft.leftRoot),
            right: SelectedFolder(bytes: draft.rightRoot), mode: mode(draft.mode),
            options: pairOptions(draft.options)
        ))
    }

    private func coreMode(_ mode: PairMode) -> FoldoverCore.PairMode {
        switch mode {
        case .sync: .sync
        case .mirrorLeft: .mirrorLeft
        case .mirrorRight: .mirrorRight
        case .backupLeft: .backupLeft
        case .backupRight: .backupRight
        }
    }

    private func mode(_ mode: FoldoverCore.PairMode) -> PairMode {
        switch mode {
        case .sync: .sync
        case .mirrorLeft: .mirrorLeft
        case .mirrorRight: .mirrorRight
        case .backupLeft: .backupLeft
        case .backupRight: .backupRight
        }
    }
}

private func pairOptions(_ options: LocalPairOptions) -> PairOptions {
    let durability: PairDurability = switch options.durability {
    case .standard: .standard
    case .full: .full
    }
    return PairOptions(
        syncModificationTimes: options.modificationTimes == .sync,
        syncExtendedAttributes: options.extendedAttributes == .sync,
        syncPermissions: options.permissions == .sync,
        durability: durability,
        schedule: syncSchedule(options.schedule),
        filters: options.filters,
        sizeLimit: options.sizeLimit,
        ageLimit: options.ageLimit,
        canary: options.canary,
        records: options.records == .sourceOnly ? .sourceOnly : .both,
        bandwidthLimit: options.bandwidthLimit
    )
}

private func syncSchedule(_ schedule: FoldoverCore.PairSchedule) -> SyncSchedule {
    switch schedule {
    case .manual: .manual
    case .every(let minutes): .every(minutes: minutes)
    case .onChange: .onChange
    case .daily(let minutes, let days): .daily(minutes: minutes, days: Set(days.map(syncDay)))
    }
}

private func coreSchedule(_ schedule: SyncSchedule) -> FoldoverCore.PairSchedule {
    switch schedule {
    case .manual: .manual
    case .every(let minutes): .every(minutes: minutes)
    case .onChange: .onChange
    case .daily(let minutes, let days): .daily(minutes: minutes, days: days.map(coreDay))
    }
}

private func syncDay(_ day: FoldoverCore.ScheduleDay) -> SyncDay {
    switch day {
    case .monday: .monday
    case .tuesday: .tuesday
    case .wednesday: .wednesday
    case .thursday: .thursday
    case .friday: .friday
    case .saturday: .saturday
    case .sunday: .sunday
    }
}

private func coreDay(_ day: SyncDay) -> FoldoverCore.ScheduleDay {
    switch day {
    case .monday: .monday
    case .tuesday: .tuesday
    case .wednesday: .wednesday
    case .thursday: .thursday
    case .friday: .friday
    case .saturday: .saturday
    case .sunday: .sunday
    }
}

private func corePairOptions(_ options: PairOptions) -> LocalPairOptions {
    func metadata(_ syncs: Bool) -> MetadataSync { syncs ? .sync : .ignore }
    let durability: WriteDurability = switch options.durability {
    case .standard: .standard
    case .full: .full
    }
    return LocalPairOptions(
        modificationTimes: metadata(options.syncModificationTimes),
        extendedAttributes: metadata(options.syncExtendedAttributes),
        permissions: metadata(options.syncPermissions),
        durability: durability,
        schedule: coreSchedule(options.schedule),
        filters: options.filters,
        sizeLimit: options.sizeLimit,
        ageLimit: options.ageLimit,
        canary: options.canary,
        records: options.records == .sourceOnly ? .sourceOnly : .both,
        bandwidthLimit: options.bandwidthLimit
    )
}

private enum PairStoreError: LocalizedError {
    case invalidFolder
    case invalidPage
    case saving
    case unskippablePath

    var errorDescription: String? {
        switch self {
        case .unskippablePath: String(localized: "This name is not valid UTF-8 text, so a rule cannot skip it. Add a pattern in the pair's settings instead.")
        case .invalidFolder: String(localized: "Choose a folder on this Mac or a mounted volume.")
        case .invalidPage: String(localized: "The agent returned an invalid pair page. Refresh to try again.")
        case .saving: String(localized: "Wait for the current settings to finish saving.")
        }
    }
}

@Observable
@MainActor
final class PairStore {
    private let client: any PairClient
    private var generation = 0
    private(set) var pairs: [SyncPair] = []
    private(set) var isLoading = false
    private(set) var isSaving = false
    private(set) var hasLoaded = false
    private(set) var loadError: String?

    init(client: any PairClient = CorePairClient()) {
        self.client = client
    }

    func refresh() async {
        guard !isSaving else { return }
        generation += 1
        let request = generation
        isLoading = true
        loadError = nil
        do {
            var loaded: [SyncPair] = []
            var after: String?
            repeat {
                let page = try await client.page(after: after)
                guard generation == request else { return }
                if let next = page.next, after.map({ next <= $0 }) ?? false {
                    throw PairStoreError.invalidPage
                }
                loaded.append(contentsOf: page.pairs)
                after = page.next
            } while after != nil
            pairs = loaded
            hasLoaded = true
            isLoading = false
        } catch {
            guard generation == request else { return }
            isLoading = false
            loadError = Self.message(for: error)
        }
    }

    /// What will not work as a person might expect with `settings`' folders
    /// (PLAN.md §3.6); warnings never block saving.
    func check(_ settings: PairSettings) async throws -> [PairReadinessWarning] {
        try await client.check(settings)
    }

    /// SMB file servers seen on the local network (PLAN.md §3.8); waits
    /// about two seconds while the agent browses.
    func nearbyServers() async throws -> [NearbyServer] {
        try await client.servers()
    }

    func save(_ settings: PairSettings, editing: SyncPair?) async throws -> SyncPair {
        guard !isSaving else { throw PairStoreError.saving }
        generation += 1
        isLoading = false
        isSaving = true
        defer { isSaving = false }
        let saved = if let editing {
            try await client.update(editing, settings: settings)
        } else {
            try await client.create(settings)
        }
        pairs.removeAll { $0.id == saved.id }
        pairs.append(saved)
        pairs.sort { $0.id < $1.id }
        return saved
    }

    /// Skip `path` from now on: add a rule matching exactly it to the
    /// pair's filters. `expectedRevision` is the pair revision of the
    /// preview the item was reviewed in, so the edit cannot silently
    /// land on a pair that changed since. The base is kept, so nothing
    /// is deleted.
    func skip(_ path: Data, inPairWithID id: String, expectedRevision: UInt64) async throws {
        guard let rule = Self.skipRule(for: path) else { throw PairStoreError.unskippablePath }
        guard let pair = pairs.first(where: { $0.id == id }) else { throw PairStoreError.invalidPage }
        var options = pair.settings.options
        guard !options.filters.contains(rule) else { return }
        options.filters.append(rule)
        let settings = PairSettings(name: pair.settings.name, left: pair.settings.left,
                                    right: pair.settings.right, mode: pair.settings.mode, options: options)
        _ = try await save(settings, editing: SyncPair(
            id: pair.id, revision: expectedRevision, settings: pair.settings
        ))
    }

    /// An anchored gitignore rule for exactly `path`: `docs/a*.txt`
    /// becomes `/docs/a\*.txt`. Nil when the path is not UTF-8 text,
    /// which rules cannot express.
    static func skipRule(for path: Data) -> String? {
        guard let text = String(data: path, encoding: .utf8) else { return nil }
        let body = text.reversed().drop(while: { $0 == " " }).reversed()
        let trailingSpaces = text.count - body.count
        var rule = "/"
        for character in body {
            if "\\*?[]{}".contains(character) { rule.append("\\") }
            rule.append(character)
        }
        // Gitignore drops unescaped trailing spaces.
        rule += String(repeating: "\\ ", count: trailingSpaces)
        return rule
    }

    func folder(from url: URL) throws -> SelectedFolder {
        guard let bytes = Self.folderBytes(url) else { throw PairStoreError.invalidFolder }
        return SelectedFolder(bytes: bytes)
    }

    /// A file URL's exact path bytes, as the agent takes them.
    static func folderBytes(_ url: URL) -> Data? {
        guard url.isFileURL else { return nil }
        return url.withUnsafeFileSystemRepresentation { path -> Data? in
            guard let path else { return nil }
            return Data(bytes: path, count: strlen(path))
        }
    }

    func remove(_ pair: SyncPair) async throws {
        guard !isSaving else { throw PairStoreError.saving }
        generation += 1
        isLoading = false
        isSaving = true
        defer { isSaving = false }
        let removed = try await client.remove(pair)
        guard removed.id == pair.id, removed.revision == pair.revision else { throw PairStoreError.invalidPage }
        pairs.removeAll { $0.id == pair.id }
    }

    static func message(for error: Error) -> String {
        switch error {
        case AgentError.Connection(let message), AgentError.NotRunning(let message),
             AgentError.Request(let message): message
        default: error.localizedDescription
        }
    }
}
