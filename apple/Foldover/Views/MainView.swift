// Pairs, their sync controls and background-agent status (PLAN.md §3.8).

import SwiftUI
import UniformTypeIdentifiers

struct MainView: View {
    @Bindable var viewModel: MainViewModel

    var body: some View {
        NavigationSplitView {
            List(selection: $viewModel.selectedPairID) {
                Section("Pairs") {
                    ForEach(viewModel.pairs) { pair in
                        VStack(alignment: .leading) {
                            Text(pair.settings.name)
                            Text(pair.settings.mode.title).font(.caption).foregroundStyle(.secondary)
                        }
                        .tag(pair.id)
                    }
                }
            }
            .navigationTitle(viewModel.title)
            .overlay {
                if viewModel.pairsLoading { ProgressView("Loading pairs…") }
            }
            .safeAreaInset(edge: .bottom) {
                VStack(alignment: .leading, spacing: 8) {
                    Text(viewModel.agentStatusText).font(.caption)
                    Button("Enable Background Agent") { Task { await viewModel.enableAgent() } }
                    if viewModel.agentNeedsApproval {
                        Button("Open Login Items") { viewModel.openLoginItems() }
                    }
                    if let error = viewModel.pairError {
                        Text(error).font(.caption).foregroundStyle(.red).textSelection(.enabled)
                    }
                    if let error = viewModel.operationError {
                        Text(error).font(.caption).foregroundStyle(.red).textSelection(.enabled)
                    }
                }
                .padding()
            }
            // Drag two folders in to create a pair (PLAN.md §3.8).
            .dropDestination(for: URL.self) { urls, _ in
                viewModel.newPair(dropping: urls)
            }
        } detail: {
            if let pair = viewModel.selectedPair {
                VStack(alignment: .leading, spacing: 16) {
                    Text(pair.settings.name).font(.title)
                    LabeledContent("Folder A", value: viewModel.folderText(pair.settings.left))
                    LabeledContent("Folder B", value: viewModel.folderText(pair.settings.right))
                    Text(pair.settings.mode.title).font(.headline)
                    Text(pair.settings.mode.explanation).foregroundStyle(.secondary)
                    Button("Edit Pair") { viewModel.editPair() }.disabled(!viewModel.canConfigurePairs)
                    Button("Browse Archive…") { viewModel.openArchive() }
                    Button("Remove Pair", role: .destructive) { viewModel.requestRemoval() }
                        .disabled(!viewModel.canConfigurePairs)
                    Divider()
                    PairSyncView(viewModel: viewModel.sync(for: pair))
                }
                .textSelection(.enabled)
                .padding(24)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            } else {
                Group {
                    if viewModel.showsSetup {
                        SetupView(viewModel: viewModel)
                    } else {
                        ContentUnavailableView(viewModel.detailTitle, systemImage: "folder.badge.plus",
                            description: Text("Choose two folders and a sync mode, then preview and sync."))
                    }
                }
                .dropDestination(for: URL.self) { urls, _ in
                    viewModel.newPair(dropping: urls)
                }
            }
        }
        .toolbar {
            Button("New Pair", systemImage: "plus") { viewModel.newPair() }
                .keyboardShortcut("n")
                .disabled(!viewModel.canConfigurePairs)
            Button("Refresh", systemImage: "arrow.clockwise") { Task { await viewModel.refreshAgent() } }
        }
        .sheet(item: Binding(get: { viewModel.editor }, set: { _ in viewModel.dismissEditor() })) { editor in
            PairEditorView(viewModel: editor)
        }
        .sheet(item: Binding(get: { viewModel.archivePair }, set: { _ in viewModel.dismissArchive() })) { pair in
            ArchiveView(viewModel: viewModel.archive(for: pair))
        }
        .confirmationDialog("Remove Pair?", isPresented: Binding(
            get: { viewModel.removalTarget != nil }, set: { _ in viewModel.cancelRemoval() }
        ), presenting: viewModel.removalTarget) { pair in
            Button("Remove Pair", role: .destructive) { Task { await viewModel.removePair(pair) } }
            Button("Cancel", role: .cancel) { viewModel.cancelRemoval() }
        } message: { pair in
            Text("Remove settings for \(pair.settings.name)? Your folders stay in place.")
        }
        .frame(minWidth: 800, minHeight: 460)
        .task { await viewModel.refreshAgent() }
    }
}
