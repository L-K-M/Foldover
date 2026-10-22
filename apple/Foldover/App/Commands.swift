// The menu bar's commands (PLAN.md §3.8): every main-window command
// with its keyboard shortcut, enabled only when it can act. The main
// window publishes its model as a focused value, so window commands
// follow the key window. The menus own these shortcuts; buttons in the
// windows do not repeat them.
//
// Each menu's items are views rather than inline buttons so that what
// they read from the observable models keeps their enabled states and
// titles current.

import SwiftUI

/// The app, File, Edit and Help menus' additions.
struct AppCommands: Commands {
    let viewModel: MainViewModel
    let updates: UpdateStore
    @FocusedValue(\.mainWindow) private var window: MainViewModel?

    var body: some Commands {
        CommandGroup(after: .appInfo) {
            CheckForUpdatesButton(updates: updates)
        }
        CommandGroup(replacing: .newItem) {
            NewPairButton(viewModel: viewModel)
        }
        CommandGroup(after: .textEditing) {
            FindButton(window: window)
        }
        CommandGroup(replacing: .help) {
            GuideButton()
        }
    }
}

/// The View menu: the sidebar, inspector and toolbar, and which kinds
/// of changes the selected pair lists.
struct ViewCommands: Commands {
    @FocusedValue(\.mainWindow) private var window: MainViewModel?

    var body: some Commands {
        SidebarCommands()
        InspectorCommands()
        ToolbarCommands()
        CommandGroup(before: .toolbar) {
            ChangeFilterItems(window: window)
        }
    }
}

/// The Pair menu: a pair's context-menu commands and the selected
/// change's, for the key main window.
struct PairCommands: Commands {
    let viewModel: MainViewModel
    @FocusedValue(\.mainWindow) private var window: MainViewModel?

    var body: some Commands {
        CommandMenu("Pair") {
            PairMenuItems(viewModel: viewModel, window: window)
        }
    }
}

private struct CheckForUpdatesButton: View {
    let updates: UpdateStore

    var body: some View {
        Button("Check for Updates…") { Task { await updates.checkForUpdates() } }
            .disabled(updates.checking)
    }
}

/// Works with no window open: the editor is a sheet on the main window,
/// so that window comes up first.
private struct NewPairButton: View {
    let viewModel: MainViewModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Button("New Pair…") {
            openWindow(id: "main")
            viewModel.newPair()
        }
        .keyboardShortcut("n")
        // A new editor would replace the one already open.
        .disabled(!viewModel.canConfigurePairs || viewModel.editor != nil)
    }
}

/// Focuses the main window's search field.
private struct FindButton: View {
    let window: MainViewModel?

    var body: some View {
        Divider()
        Button("Find…") { window?.requestFind() }
            .keyboardShortcut("f")
            .disabled(window == nil)
    }
}

private struct GuideButton: View {
    @Environment(\.openURL) private var openURL

    var body: some View {
        Button("Foldover Guide") {
            guard let url = URL(string: "https://github.com/L-K-M/Foldover/tree/main/docs/guide") else { return }
            openURL(url)
        }
    }
}

/// The scope bar's choices, as checkmarked items with ⌘1 to ⌘6.
private struct ChangeFilterItems: View {
    let window: MainViewModel?

    var body: some View {
        let sync = window?.selectedSync
        Section("Show") {
            filterToggle(nil, key: "1", sync: sync)
            filterToggle(.copies, key: "2", sync: sync)
            filterToggle(.updates, key: "3", sync: sync)
            filterToggle(.archives, key: "4", sync: sync)
            filterToggle(.conflicts, key: "5", sync: sync)
            filterToggle(.moves, key: "6", sync: sync)
        }
        .disabled(!(sync?.showsChangeFilter ?? false))
    }

    private func filterToggle(_ filter: ChangeFilter?, key: KeyEquivalent,
                              sync: PairSyncViewModel?) -> some View {
        Toggle(PairSyncViewModel.title(filter), isOn: Binding(
            get: { sync.map { $0.changeFilter == filter } ?? false },
            set: { isOn in
                // Choosing the shown kind again keeps it shown.
                guard isOn, let sync else { return }
                Task { await sync.showChanges(filter) }
            }
        ))
        .keyboardShortcut(key)
    }
}

/// The selected pair's commands; all off while no main window is key.
private struct PairMenuItems: View {
    let viewModel: MainViewModel
    let window: MainViewModel?
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        let sync = window?.selectedSync
        runItems(sync)
        Divider()
        Button("Sync All") { Task { await viewModel.syncAll() } }
            .keyboardShortcut("s", modifiers: [.command, .option])
            .disabled(!viewModel.canSyncAll)
        Divider()
        ChangeMenuItems(window: window, sync: sync)
        Divider()
        folderItems
        Divider()
        configurationItems
    }

    /// Preview, sync and stop go through the window's model, which
    /// records a window sync for Edit > Undo.
    @ViewBuilder
    private func runItems(_ sync: PairSyncViewModel?) -> some View {
        Button("Preview Changes") { Task { await window?.previewSelected() } }
            .keyboardShortcut("r")
            .disabled(!(sync?.canStart ?? false))
        Button("Sync Now") { Task { await window?.syncSelected() } }
            .keyboardShortcut("s")
            .disabled(!(sync?.canSync ?? false))
        Button("Stop") { Task { await window?.stopSelected() } }
            .keyboardShortcut(".")
            .disabled(!(sync?.canCancel ?? false))
    }

    @ViewBuilder
    private var folderItems: some View {
        Button("Show Folder A in Finder") { window?.reveal(.a) }
            .disabled(!(window?.canReveal(.a) ?? false))
        Button("Show Folder B in Finder") { window?.reveal(.b) }
            .disabled(!(window?.canReveal(.b) ?? false))
        Button("Browse Archive…") {
            guard let pairID = window?.selectedPair?.id else { return }
            openWindow(id: "archive", value: pairID)
        }
        .keyboardShortcut("a", modifiers: [.command, .shift])
        .disabled(window?.selectedPair == nil)
    }

    @ViewBuilder
    private var configurationItems: some View {
        Button("Edit Pair…") { window?.editPair() }
            .keyboardShortcut("i")
            .disabled(!canConfigure)
        // No shortcut: ⌘⌫ deletes to the line start in text fields. The
        // Delete key removes the pair while the sidebar has focus.
        Button("Remove Pair…") { window?.requestRemoval() }
            .disabled(!canConfigure)
    }

    /// Editing and removal need a selected pair, and wait while the editor
    /// is open: a new editor would replace it.
    private var canConfigure: Bool {
        guard let window, window.selectedPair != nil, window.editor == nil else { return false }
        return window.canConfigurePairs
    }
}

/// The selected change's commands, as in the change table's context
/// menu. Steering a change waits while the pair is busy.
private struct ChangeMenuItems: View {
    let window: MainViewModel?
    let sync: PairSyncViewModel?

    var body: some View {
        let change = window?.selectedChange
        let canSteer = sync?.canStart ?? false
        Button("Quick Look") { window?.quickLookURL = change?.look }
            .disabled(change?.look == nil)
        Button("Show in Finder") {
            guard let sync, let change else { return }
            sync.reveal(change)
        }
        .disabled(change?.look == nil)
        Button("Keep Folder A’s Version") {
            guard let sync, let change else { return }
            Task { await sync.keep(.a, for: change) }
        }
        .disabled(!canSteer || change?.resolvable == nil)
        Button("Keep Folder B’s Version") {
            guard let sync, let change else { return }
            Task { await sync.keep(.b, for: change) }
        }
        .disabled(!canSteer || change?.resolvable == nil)
        Button("Keep Both Versions") {
            guard let sync, let change else { return }
            Task { await sync.keepBoth(for: change) }
        }
        .disabled(!canSteer || change?.keepsBoth != true)
        Button(change?.otherWayTitle ?? String(localized: "Copy the Other Way")) {
            guard let sync, let change else { return }
            Task { await sync.copyOtherWay(change) }
        }
        .disabled(!canSteer || change?.otherWay == nil)
        Button("Skip Once") {
            guard let sync, let change else { return }
            Task { await sync.skipOnce(change) }
        }
        .disabled(!canSteer || change?.item == nil)
        Button("Always Skip") {
            guard let sync, let change else { return }
            Task { await sync.skipAlways(change) }
        }
        .disabled(!canSteer || change?.skippable == nil)
    }
}
