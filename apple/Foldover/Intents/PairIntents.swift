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

/// How long Shortcuts pauses automatic syncs.
enum PauseLengthEntity: String, AppEnum {
    case oneHour
    case fourHours
    case untilResumed

    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Pause Length"
    static let caseDisplayRepresentations: [PauseLengthEntity: DisplayRepresentation] = [
        .oneHour: "For 1 Hour",
        .fourHours: "For 4 Hours",
        .untilResumed: "Until Resumed",
    ]

    var length: PauseLength {
        switch self {
        case .oneHour: .oneHour
        case .fourHours: .fourHours
        case .untilResumed: .untilResumed
        }
    }
}

struct PauseAutomaticSyncsIntent: AppIntent {
    static let title: LocalizedStringResource = "Pause Automatic Syncs"
    static let description: IntentDescription = IntentDescription("Holds scheduled and change-triggered syncs for every pair. Manual syncs still run.")

    @Parameter(title: "Length", default: .oneHour) var length: PauseLengthEntity
    @Dependency private var viewModel: MainViewModel

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let outcome = await viewModel.pauseForShortcuts(length.length)
        return .result(dialog: "\(outcome)")
    }
}

struct ResumeAutomaticSyncsIntent: AppIntent {
    static let title: LocalizedStringResource = "Resume Automatic Syncs"

    @Dependency private var viewModel: MainViewModel

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let outcome = await viewModel.resumeForShortcuts()
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
