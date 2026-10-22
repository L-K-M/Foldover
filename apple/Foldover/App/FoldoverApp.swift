// The app shell (PLAN.md §4.5): one main window plus the menu-bar
// extra. This file only wires the scenes; state lives in the stores
// and view models.

import AppIntents
import Foundation
import SwiftUI

@main
struct FoldoverApp: App {
    @State private var viewModel: MainViewModel
    @State private var settings = SettingsStore()
    @State private var macToMac: MacToMacStore
    @State private var automation = AutomationStore()
    @State private var updates = UpdateStore()

    init() {
        // One store each, so the pair editor sees the Macs Settings
        // paired and Settings sees the menu bar's automation state.
        let macToMac = MacToMacStore()
        _macToMac = State(initialValue: macToMac)
        let automation = AutomationStore()
        _automation = State(initialValue: automation)
        let viewModel = MainViewModel(appInfo: AppInfoStore(), automation: automation, macToMac: macToMac)
        _viewModel = State(initialValue: viewModel)
        // Shortcuts actions run in the app and share its state.
        AppDependencyManager.shared.add(dependency: viewModel)
        // Runs for the app's lifetime, with or without an open window.
        Task { await viewModel.monitor() }
        // A test host is not a launch: a modal prompt there would stall
        // the suite. UI-test launches carry no test environment and opt
        // out with a launch argument instead.
        if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] == nil,
           !CommandLine.arguments.contains("-foldover-skip-update-check") {
            Task { await updates.checkAtLaunch() }
        }
    }

    var body: some Scene {
        // One configuration window shares the same selection and editor as the menu bar.
        Window("Foldover", id: "main") {
            MainView(viewModel: viewModel)
        }
        .commands {
            CommandGroup(after: .appInfo) {
                Button("Check for Updates…") { Task { await updates.checkForUpdates() } }
                    .disabled(updates.checking)
            }
        }

        // .window style hosts a regular SwiftUI view instead of the
        // default menu rendering, which is what the status surfaces in
        // PLAN.md §3.8 need.
        MenuBarExtra("Foldover", systemImage: "arrow.triangle.2.circlepath") {
            MenuBarView(viewModel: viewModel)
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView(viewModel: SettingsViewModel(store: settings),
                         macToMac: MacToMacViewModel(store: macToMac),
                         automation: automation)
        }
    }
}
