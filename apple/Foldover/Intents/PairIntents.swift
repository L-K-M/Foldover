// Shortcuts actions (PLAN.md §3.8): sync or preview a pair. They run in
// the app, without an extension, through the same view model as the
// windows.

import AppIntents

/// A pair as Shortcuts shows it.
struct PairEntity: AppEntity {
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Pair"
    static let defaultQuery = PairQuery()

    let id: String
    let name: String

    var displayRepresentation: DisplayRepresentation { DisplayRepresentation(title: "\(name)") }
}

struct PairQuery: EntityQuery {
    @Dependency private var viewModel: MainViewModel

    func entities(for identifiers: [String]) async throws -> [PairEntity] {
        try await suggestedEntities().filter { identifiers.contains($0.id) }
    }

    func suggestedEntities() async throws -> [PairEntity] {
        await viewModel.intentPairs().map { PairEntity(id: $0.id, name: $0.name) }
    }
}

struct SyncPairIntent: AppIntent {
    static let title: LocalizedStringResource = "Sync Pair"
    static let description: IntentDescription = IntentDescription("Syncs a Foldover pair now and says what changed.")

    @Parameter(title: "Pair") var pair: PairEntity
    @Dependency private var viewModel: MainViewModel

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let outcome = await viewModel.perform(.sync, onPairWithID: pair.id)
        return .result(dialog: "\(outcome)")
    }
}

struct PreviewPairIntent: AppIntent {
    static let title: LocalizedStringResource = "Preview Pair"
    static let description: IntentDescription = IntentDescription("Compares a Foldover pair's folders and says what a sync would change, without changing anything.")

    @Parameter(title: "Pair") var pair: PairEntity
    @Dependency private var viewModel: MainViewModel

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let outcome = await viewModel.perform(.preview, onPairWithID: pair.id)
        return .result(dialog: "\(outcome)")
    }
}

struct FoldoverShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(intent: SyncPairIntent(), phrases: ["Sync a pair in \(.applicationName)"],
                    shortTitle: "Sync Pair", systemImageName: "arrow.triangle.2.circlepath")
        AppShortcut(intent: PreviewPairIntent(), phrases: ["Preview a pair in \(.applicationName)"],
                    shortTitle: "Preview Pair", systemImageName: "eye")
    }
}
