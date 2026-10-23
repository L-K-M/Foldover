// The main window (PLAN.md §3.8, §7.1): pairs in the sidebar, the
// selected pair's plan and changes in the detail, its settings and
// history in the inspector, and the commands in the toolbar.

import QuickLook
import SwiftUI
import UniformTypeIdentifiers

struct MainView: View {
    @Bindable var viewModel: MainViewModel
    @Environment(\.undoManager) private var undoManager
    @Environment(\.openWindow) private var openWindow
    @SceneStorage("main.inspector") private var showsInspector = true
    /// The pair the window showed last, restored once the catalog has it.
    @SceneStorage("main.pair") private var restoredPairID = ""
    @FocusState private var searchFocused: Bool
    @FocusState private var paneFocus: MainPane?

    var body: some View {
        splitView
            // The window's commands withdraw while its editor or removal
            // dialog is up, so ⌘S in the editor cannot start a sync.
            .focusedSceneValue(\.mainWindow,
                               viewModel.editor == nil && viewModel.removalTarget == nil ? viewModel : nil)
            .quickLookPreview($viewModel.quickLookURL)
            .sheet(item: Binding(get: { viewModel.editor }, set: { _ in viewModel.dismissEditor() })) { editor in
                PairEditorView(viewModel: editor)
            }
            .confirmationDialog("Remove Pair?", isPresented: Binding(
                get: { viewModel.removalTarget != nil }, set: { _ in viewModel.cancelRemoval() }
            ), presenting: viewModel.removalTarget) { pair in
                Button("Remove Pair", role: .destructive) { Task { await viewModel.removePair(pair) } }
                Button("Cancel", role: .cancel) { viewModel.cancelRemoval() }
            } message: { pair in
                Text("Remove settings for \(pair.settings.name)? Your folders stay in place.")
            }
            .alert("Couldn’t Complete the Action", isPresented: Binding(
                get: { viewModel.operationError != nil },
                set: { if !$0 { viewModel.clearOperationError() } }
            )) {
                Button("OK") { viewModel.clearOperationError() }
            } message: {
                Text(viewModel.operationError ?? "")
            }
            .onChange(of: viewModel.findRequests) { searchFocused = true }
            // Initial too: a reopened window has a new undo manager.
            .onChange(of: viewModel.undoableSync, initial: true) { _, sync in registerUndo(sync) }
            .onChange(of: viewModel.pairsLoaded) { restoreSelection() }
            .onChange(of: viewModel.selectedPairID) { _, pairID in restoredPairID = pairID ?? "" }
            .frame(minWidth: 760, minHeight: 460)
            .task {
                await viewModel.refreshAgent()
                restoreSelection()
            }
    }

    private var splitView: some View {
        NavigationSplitView {
            PairSidebar(viewModel: viewModel, focus: $paneFocus)
        } detail: {
            detail
                .navigationTitle(viewModel.selectedPair?.settings.name ?? viewModel.title)
                .navigationSubtitle(viewModel.selectedSync?.shortStatus ?? "")
        }
        .inspector(isPresented: $showsInspector) {
            PairInspector(viewModel: viewModel)
        }
        .searchable(text: searchText, placement: .toolbar, prompt: "Filter Changes")
        .searchFocused($searchFocused)
        .toolbar(id: "main") { mainToolbar }
    }

    @ViewBuilder
    private var detail: some View {
        if let pair = viewModel.selectedPair {
            PairDetailView(viewModel: viewModel, pair: pair, focus: $paneFocus)
                .id(pair.id)
        } else {
            emptyDetail
                // Drag one or two folders in to create a pair (PLAN.md §3.8).
                .dropDestination(for: URL.self) { urls, _ in
                    viewModel.newPair(dropping: urls)
                }
        }
    }

    @ViewBuilder
    private var emptyDetail: some View {
        if let error = viewModel.pairError {
            ContentUnavailableView {
                Label("Cannot Load Pairs", systemImage: "exclamationmark.triangle")
            } description: {
                Text(error)
            } actions: {
                Button("Try Again") { Task { await viewModel.refreshAgent() } }
            }
        } else if viewModel.showsSetup {
            SetupView(viewModel: viewModel)
        } else if viewModel.pairs.isEmpty {
            // The sidebar shows that the catalog is loading.
            Color.clear
        } else {
            ContentUnavailableView("No Pair Selected", systemImage: "sidebar.left",
                                   description: Text("Select a pair in the sidebar, or create one with ⌘N."))
        }
    }

    /// The search field filters the selected pair's listed changes.
    private var searchText: Binding<String> {
        Binding(
            get: { viewModel.selectedSync?.searchText ?? "" },
            set: { text in
                if let sync = viewModel.selectedSync { sync.searchText = text }
            }
        )
    }

    // The menus own the keyboard shortcuts; the help tags name them.
    @ToolbarContentBuilder
    private var mainToolbar: some CustomizableToolbarContent {
        ToolbarItem(id: "preview", placement: .primaryAction) {
            Button("Preview", systemImage: "eye") { Task { await viewModel.previewSelected() } }
                .help("Preview Changes (⌘R)")
                .disabled(!(viewModel.selectedSync?.canStart ?? false))
        }
        // Run and Stop share one slot, as in Xcode.
        ToolbarItem(id: "sync", placement: .primaryAction) {
            if viewModel.selectedSync?.isWorking == true {
                Button("Stop", systemImage: "stop.fill") { Task { await viewModel.stopSelected() } }
                    .help("Stop (⌘.)")
                    .disabled(!(viewModel.selectedSync?.canCancel ?? false))
            } else {
                Button("Sync Now", systemImage: "arrow.triangle.2.circlepath") {
                    Task { await viewModel.syncSelected() }
                }
                .help("Sync Now (⌘S)")
                .disabled(!(viewModel.selectedSync?.canSync ?? false))
            }
        }
        ToolbarItem(id: "newPair", placement: .primaryAction) {
            Button("New Pair", systemImage: "plus") { viewModel.newPair() }
                .help("New Pair (⌘N)")
                .disabled(!viewModel.canConfigurePairs)
        }
        ToolbarItem(id: "inspector", placement: .primaryAction) {
            Button("Inspector", systemImage: "sidebar.trailing") { showsInspector.toggle() }
                .help("Show or Hide the Inspector (⌃⌘I)")
        }
        ToolbarItem(id: "syncAll", placement: .primaryAction) {
            Button("Sync All", systemImage: "arrow.triangle.2.circlepath.circle") {
                Task { await viewModel.syncAll() }
            }
            .help("Sync All (⌥⌘S)")
            .disabled(!viewModel.canSyncAll)
        }
        .defaultCustomization(.hidden)
        ToolbarItem(id: "archive", placement: .primaryAction) {
            Button("Archive", systemImage: "archivebox") {
                if let pairID = viewModel.selectedPairID { openWindow(id: "archive", value: pairID) }
            }
            .help("Browse Archive (⇧⌘A)")
            .disabled(viewModel.selectedPair == nil)
        }
        .defaultCustomization(.hidden)
        ToolbarItem(id: "edit", placement: .primaryAction) {
            Button("Edit Pair", systemImage: "slider.horizontal.3") { viewModel.editPair() }
                .help("Edit Pair (⌘I)")
                .disabled(viewModel.selectedPair == nil || !viewModel.canConfigurePairs)
        }
        .defaultCustomization(.hidden)
    }

    /// Select the pair the window showed last time once the catalog has
    /// it; a selection made meanwhile, here or from the menu bar, wins.
    private func restoreSelection() {
        guard viewModel.selectedPairID == nil, !restoredPairID.isEmpty,
              viewModel.pairs.contains(where: { $0.id == restoredPairID }) else { return }
        viewModel.selectedPairID = restoredPairID
    }

    /// The view model decides what Edit > Undo can reverse; the window
    /// only registers it. Undo is not destructive (the engine puts
    /// replaced files back from the archive), so it asks nothing first.
    private func registerUndo(_ sync: UndoableSync?) {
        undoManager?.removeAllActions(withTarget: viewModel)
        guard let sync, let undoManager else { return }
        let pairID = sync.pairID
        undoManager.registerUndo(withTarget: viewModel) { model in
            Task { @MainActor in await model.undoSync(pairID: pairID) }
        }
        undoManager.setActionName(sync.actionName)
    }
}

/// The main window's panes that take the keyboard. Choosing a pair
/// focuses the sidebar and choosing a change the table, so Delete,
/// Space and the arrow keys act where the person last chose something,
/// whatever the click left as first responder.
enum MainPane: Hashable {
    case sidebar
    case changes
}
