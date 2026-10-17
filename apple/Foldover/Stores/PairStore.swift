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

struct PairSettings: Equatable, Sendable {
    let name: String
    let left: SelectedFolder
    let right: SelectedFolder
    let mode: PairMode
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
                       rightRoot: settings.right.bytes, mode: coreMode(settings.mode))
    }

    private func pair(_ pair: LocalPairConfiguration) -> SyncPair {
        let draft = pair.configuration
        return SyncPair(id: pair.id, revision: pair.revision, settings: PairSettings(
            name: draft.name, left: SelectedFolder(bytes: draft.leftRoot),
            right: SelectedFolder(bytes: draft.rightRoot), mode: mode(draft.mode)
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

private enum PairStoreError: LocalizedError {
    case invalidFolder
    case invalidPage
    case saving

    var errorDescription: String? {
        switch self {
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
