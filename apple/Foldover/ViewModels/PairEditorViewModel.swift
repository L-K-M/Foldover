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
        left = editing?.settings.left
        right = editing?.settings.right
    }

    var title: String { editing == nil ? String(localized: "New Pair") : String(localized: "Edit Pair") }
    var canSave: Bool { !isSaving && !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && left != nil && right != nil }

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
            let saved = try await store.save(PairSettings(name: name, left: left, right: right, mode: mode), editing: editing)
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
