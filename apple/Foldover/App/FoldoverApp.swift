// The app shell (PLAN.md §4.5): one main window plus the menu-bar
// extra. This file only wires the scenes; state lives in the stores
// and view models.

import AppIntents
import SwiftUI

@main
struct FoldoverApp: App {
    @State private var viewModel: MainViewModel
    @State private var settings = SettingsStore()

    init() {
        let viewModel = MainViewModel(appInfo: AppInfoStore())
        _viewModel = State(initialValue: viewModel)
        // Shortcuts actions run in the app and share its state.
        AppDependencyManager.shared.add(dependency: viewModel)
        // Runs for the app's lifetime, with or without an open window.
        Task { await viewModel.monitor() }
    }

    var body: some Scene {
        // One configuration window shares the same selection and editor as the menu bar.
        Window("Foldover", id: "main") {
            MainView(viewModel: viewModel)
        }

        // .window style hosts a regular SwiftUI view instead of the
        // default menu rendering, which is what the status surfaces in
        // PLAN.md §3.8 need.
        MenuBarExtra("Foldover", systemImage: "arrow.triangle.2.circlepath") {
            MenuBarView(viewModel: viewModel)
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView(viewModel: SettingsViewModel(store: settings))
        }
    }
}
