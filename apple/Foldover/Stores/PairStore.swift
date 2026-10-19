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
}

enum PairDurability: CaseIterable, Hashable, Sendable {
    case standard
    case full
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

protocol PairClient: Sendable {
    func page(after: String?) async throws -> PairCatalogPage
    func create(_ settings: PairSettings) async throws -> SyncPair
    func update(_ pair: SyncPair, settings: PairSettings) async throws -> SyncPair
    func remove(_ pair: SyncPair) async throws -> SyncPair
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
        ageLimit: options.ageLimit
    )
}

private func syncSchedule(_ schedule: FoldoverCore.PairSchedule) -> SyncSchedule {
    switch schedule {
    case .manual: .manual
    case .every(let minutes): .every(minutes: minutes)
    case .onChange: .onChange
    }
}

private func coreSchedule(_ schedule: SyncSchedule) -> FoldoverCore.PairSchedule {
    switch schedule {
    case .manual: .manual
    case .every(let minutes): .every(minutes: minutes)
    case .onChange: .onChange
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
        ageLimit: options.ageLimit
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
        guard url.isFileURL else { throw PairStoreError.invalidFolder }
        let bytes = url.withUnsafeFileSystemRepresentation { path -> Data? in
            guard let path else { return nil }
            return Data(bytes: path, count: strlen(path))
        }
        guard let bytes else { throw PairStoreError.invalidFolder }
        return SelectedFolder(bytes: bytes)
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
