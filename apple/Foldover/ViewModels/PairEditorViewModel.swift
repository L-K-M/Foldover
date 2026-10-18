import Foundation
import Observation

enum PairFolderSide: Equatable {
    case left
    case right
}

@Observable
@MainActor
final class PairEditorViewModel: Identifiable {
    let id = UUID()
    private let store: PairStore
    private let editing: SyncPair?
    var name: String
    var mode: PairMode
    var options: PairOptions
    private(set) var left: SelectedFolder?
    private(set) var right: SelectedFolder?
    private(set) var isSaving = false
    private(set) var errorMessage: String?
    private(set) var savedPairID: String?

    init(store: PairStore, editing: SyncPair? = nil) {
        self.store = store
        self.editing = editing
        name = editing?.settings.name ?? ""
        mode = editing?.settings.mode ?? .sync
        options = editing?.settings.options ?? .defaults
        left = editing?.settings.left
        right = editing?.settings.right
    }

    var title: String { editing == nil ? String(localized: "New Pair") : String(localized: "Edit Pair") }
    var canSave: Bool { !isSaving && !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && left != nil && right != nil }

    /// Exclusion rules as editable text, one per line.
    var filterText: String {
        get { options.filters.joined(separator: "\n") }
        set {
            options.filters = newValue.split(separator: "\n", omittingEmptySubsequences: false)
                .map(String.init)
        }
    }

    /// Fixed choices, plus a stored interval set elsewhere (e.g. the CLI).
    var scheduleChoices: [SyncSchedule] {
        var choices: [SyncSchedule] = [.manual, .onChange, .every(minutes: 5), .every(minutes: 15),
                                       .every(minutes: 60), .every(minutes: 24 * 60)]
        if !choices.contains(options.schedule) { choices.append(options.schedule) }
        return choices
    }

    static func scheduleTitle(_ schedule: SyncSchedule) -> String {
        switch schedule {
        case .manual: String(localized: "Only when I click Sync Now")
        case .onChange: String(localized: "When files change")
        case .every(60): String(localized: "Every hour")
        case .every(1440): String(localized: "Every day")
        case .every(let minutes): String(localized: "Every \(minutes) minutes")
        }
    }

    /// Versions depend on synced metadata, so changing it resets sync history.
    var resetsSyncHistory: Bool {
        guard let editing else { return false }
        return !options.syncsSameMetadata(as: editing.settings.options)
    }

    func folderText(_ side: PairFolderSide) -> String {
        let folder = side == .left ? left : right
        return folder.map(Self.pathText) ?? String(localized: "Choose a folder")
    }

    func chooseFolder(_ result: Result<[URL], Error>, side: PairFolderSide) {
        do {
            guard let url = try result.get().first else { return }
            let folder = try store.folder(from: url)
            switch side {
            case .left: left = folder
            case .right: right = folder
            }
            errorMessage = nil
        } catch {
            errorMessage = PairStore.message(for: error)
        }
    }

    func save() async -> Bool {
        guard canSave, let left, let right else { return false }
        isSaving = true
        errorMessage = nil
        defer { isSaving = false }
        do {
            let saved = try await store.save(PairSettings(
                name: name, left: left, right: right, mode: mode, options: options
            ), editing: editing)
            savedPairID = saved.id
            return true
        } catch {
            errorMessage = PairStore.message(for: error)
            return false
        }
    }

    static func pathText(_ folder: SelectedFolder) -> String {
        String(data: folder.bytes, encoding: .utf8) ?? String(localized: "Path contains non-UTF-8 bytes")
    }
}

extension PairMode {
    var title: String {
        switch self {
        case .sync: String(localized: "Sync both ways")
        case .mirrorLeft: String(localized: "Mirror A to B")
        case .mirrorRight: String(localized: "Mirror B to A")
        case .backupLeft: String(localized: "Back up A to B")
        case .backupRight: String(localized: "Back up B to A")
        }
    }

    var explanation: String {
        switch self {
        case .sync: String(localized: "Changes flow both ways. Conflicting edits wait for your decision.")
        case .mirrorLeft: String(localized: "Folder B is made identical to A. Replaced and removed files are archived for 30 days.")
        case .mirrorRight: String(localized: "Folder A is made identical to B. Replaced and removed files are archived for 30 days.")
        case .backupLeft: String(localized: "New and changed files are copied from A to B. Nothing in B is deleted.")
        case .backupRight: String(localized: "New and changed files are copied from B to A. Nothing in A is deleted.")
        }
    }
}

extension PairDurability {
    var title: String {
        switch self {
        case .standard: String(localized: "Standard")
        case .full: String(localized: "Survive power loss")
        }
    }

    var explanation: String {
        switch self {
        case .standard: String(localized: "Written files survive app and system crashes.")
        case .full: String(localized: "Also flushes the drive's cache. Slower: about 10–30 ms more per file.")
        }
    }
}
