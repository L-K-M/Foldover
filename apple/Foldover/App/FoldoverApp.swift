// The app shell (PLAN.md §4.5): one main window, archive windows,
// Settings and the menu-bar extra. This file only wires the scenes and
// their commands; state lives in the stores and view models.

import AppIntents
import AppKit
import Foundation
import SwiftUI

@main
struct FoldoverApp: App {
    @State private var viewModel: MainViewModel
    @State private var settings: SettingsStore
    @State private var macToMac: MacToMacStore
    @State private var automation: AutomationStore
    @State private var updates: UpdateStore

    init() {
        let stores = Self.makeStores()
        _settings = State(initialValue: stores.settings)
        _macToMac = State(initialValue: stores.macToMac)
        _automation = State(initialValue: stores.automation)
        let updates = UpdateStore()
        _updates = State(initialValue: updates)
        let viewModel = stores.main
        _viewModel = State(initialValue: viewModel)
        // Shortcuts actions run in the app and share its state.
        AppDependencyManager.shared.add(dependency: viewModel)
        // A test host is not a launch: it must leave the developer's login
        // item alone, and a modal prompt there would stall the suite.
        // UI-test launches carry no test environment; they run on fixtures
        // and opt out of the update check with a launch argument instead.
        let isTestHost = ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
        // Runs for the app's lifetime, with or without an open window. An
        // updated app restarts its agent before the first check.
        Task {
            if !isTestHost { await viewModel.restartAgentIfUpdated() }
            await viewModel.monitor()
        }
        if !isTestHost, !CommandLine.arguments.contains("-foldover-skip-update-check") {
            Task { await updates.checkAtLaunch() }
        }
    }

    /// One store each, so the pair editor sees the Macs Settings paired
    /// and Settings sees the menu bar's automation state. A Debug build
    /// launched for a UI tour gets canned agent answers instead.
    private static func makeStores() -> AppStores {
        #if DEBUG
        if Fixtures.isEnabled { return fixtureStores() }
        #endif
        let macToMac = MacToMacStore()
        let automation = AutomationStore()
        return AppStores(
            main: MainViewModel(appInfo: AppInfoStore(), automation: automation, macToMac: macToMac),
            settings: SettingsStore(), macToMac: macToMac, automation: automation)
    }

    #if DEBUG
    /// Every store wired to the canned clients in Fixtures.swift.
    private static func fixtureStores() -> AppStores {
        Fixtures.prepareFolders()
        // Before any window reads its saved frame.
        if ProcessInfo.processInfo.arguments.contains(Fixtures.resetArgument),
           let domain = Bundle.main.bundleIdentifier {
            UserDefaults.standard.removePersistentDomain(forName: domain)
        }
        // A tour's dark pass; the system's own appearance stays as it is.
        if ProcessInfo.processInfo.arguments.contains(Fixtures.darkArgument) {
            NSApplication.shared.appearance = NSAppearance(named: .darkAqua)
        }
        let scenario = FixtureScenario.current
        let pairs = FixturePairClient(scenario: scenario)
        let macToMac = MacToMacStore(client: FixtureMacToMacClient())
        let automation = AutomationStore(client: FixtureAutomationClient())
        let main = MainViewModel(
            appInfo: AppInfoStore(),
            agent: AgentStore(statusRequest: { try await scenario?.agentReport() ?? AgentReport(version: "0.1.0") },
                              leaveSafeModeRequest: { AgentReport(version: "0.1.0") },
                              alert: { _, _ in }, system: FixtureAgentLoginItem()),
            pairs: PairStore(client: pairs),
            sync: SyncStore(client: FixtureSyncClient(pairs: pairs), alert: { _ in }),
            archive: ArchiveStore(client: FixtureArchiveClient()),
            automation: automation, macToMac: macToMac)
        return AppStores(main: main,
                         settings: SettingsStore(loginItem: FixtureLoginItem(), showFullDiskAccess: {}),
                         macToMac: macToMac, automation: automation)
    }
    #endif

    var body: some Scene {
        // The one main window (PLAN.md §7.1): everything is reachable
        // from it, and the menu bar extra shares its selection and editor.
        Window("Foldover", id: "main") {
            MainView(viewModel: viewModel)
        }
        .defaultSize(width: 1000, height: 640)
        .windowToolbarStyle(.unified)
        .commands {
            AppCommands(viewModel: viewModel, updates: updates)
            ViewCommands()
            PairCommands(viewModel: viewModel)
        }

        // One archive window per pair, opened with the pair's ID; each
        // browses with its own store.
        WindowGroup("Archive", id: "archive", for: String.self) { $pairID in
            if let pairID = pairID {
                ArchiveWindow(pairID: pairID, viewModel: viewModel)
            }
        }
        .defaultSize(width: 760, height: 500)

        // .window style hosts a regular SwiftUI view instead of the
        // default menu rendering, which is what the status surfaces in
        // PLAN.md §3.8 need. The symbol shows work, problems and pauses.
        MenuBarExtra {
            MenuBarView(viewModel: viewModel)
        } label: {
            // The menu bar shows only the symbol; the title names the
            // item for VoiceOver.
            Label("Foldover", image: viewModel.menuBarImage)
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView(viewModel: SettingsViewModel(store: settings),
                         macToMac: MacToMacViewModel(store: macToMac),
                         automation: automation)
        }
    }
}

/// The stores the app's scenes share.
@MainActor
struct AppStores {
    let main: MainViewModel
    let settings: SettingsStore
    let macToMac: MacToMacStore
    let automation: AutomationStore
}
