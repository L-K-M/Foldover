// The main window's sidebar (PLAN.md §3.8): every pair with its status
// at a glance, the commands for each, and the background agent's state.

import SwiftUI
import UniformTypeIdentifiers

struct PairSidebar: View {
    @Bindable var viewModel: MainViewModel
    let focus: FocusState<MainPane?>.Binding
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        List(selection: $viewModel.selectedPairID) {
            Section("Pairs") {
                ForEach(viewModel.pairRows) { row in
                    SidebarPairRow(row: row)
                        .contextMenu { pairMenu(row.id) }
                        .tag(row.id)
                }
            }
        }
        .listStyle(.sidebar)
        .navigationSplitViewColumnWidth(min: 200, ideal: 230, max: 340)
        .overlay {
            if viewModel.pairsLoading && !viewModel.pairsLoaded {
                ProgressView()
                    .accessibilityLabel("Loading Pairs")
            }
        }
        // Choosing a pair puts the keys here, as in Mail.
        .focused(focus, equals: .sidebar)
        .onChange(of: viewModel.selectedPairID) { focus.wrappedValue = .sidebar }
        // Delete and Edit > Delete ask first: removal cannot be undone.
        .onDeleteCommand { viewModel.requestRemoval() }
        .onKeyPress(keys: [.delete, .deleteForward]) { _ in
            guard viewModel.selectedPair != nil else { return .ignored }
            viewModel.requestRemoval()
            return .handled
        }
        // Drag one or two folders in to create a pair (PLAN.md §3.8).
        .dropDestination(for: URL.self) { urls, _ in
            viewModel.newPair(dropping: urls)
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            VStack(spacing: 0) {
                Divider()
                SidebarAgentBar(viewModel: viewModel)
            }
        }
    }

    /// The same commands as the Pair menu, for the clicked pair. Those
    /// that act on the selection select the pair first.
    @ViewBuilder
    private func pairMenu(_ pairID: String) -> some View {
        if let pair = viewModel.pairs.first(where: { $0.id == pairID }) {
            let sync = viewModel.sync(for: pair)
            Button("Preview Changes") { Task { await sync.preview() } }
                .disabled(!sync.canStart)
            Button("Sync Now") { Task { await viewModel.syncInWindow(pairID) } }
                .disabled(!sync.canSync)
            Button("Stop") { Task { await sync.cancel() } }
                .disabled(!sync.canCancel)
            Divider()
            Button("Edit Pair…") {
                viewModel.selectedPairID = pairID
                viewModel.editPair()
            }
            .disabled(!viewModel.canConfigurePairs)
            Button("Browse Archive…") { openWindow(id: "archive", value: pairID) }
            Divider()
            Button("Show Folder A in Finder") {
                viewModel.selectedPairID = pairID
                viewModel.reveal(.a)
            }
            .disabled(viewModel.localURL(pair.settings.left) == nil)
            Button("Show Folder B in Finder") {
                viewModel.selectedPairID = pairID
                viewModel.reveal(.b)
            }
            .disabled(viewModel.localURL(pair.settings.right) == nil)
            Divider()
            Button("Remove Pair…") {
                viewModel.selectedPairID = pairID
                viewModel.requestRemoval()
            }
            .disabled(!viewModel.canConfigurePairs)
        }
    }
}

/// One pair: its direction, name and short status, with a spinner while
/// it works, a warning when it needs a look, and its open conflicts.
private struct SidebarPairRow: View {
    let row: PairRow

    var body: some View {
        HStack(spacing: 6) {
            Label {
                VStack(alignment: .leading, spacing: 1) {
                    Text(row.name)
                        .lineLimit(1)
                    Text(row.status)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            } icon: {
                Image(systemName: row.symbol)
                    .help(row.modeTitle)
                    .accessibilityLabel(row.modeTitle)
            }
            Spacer(minLength: 4)
            switch row.activity {
            case .working:
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel("Working")
            case .attention:
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                    .accessibilityLabel("Needs Attention")
            case .idle, .waiting:
                EmptyView()
            }
        }
        .badge(row.conflicts)
    }
}

/// The background agent at a glance: a colored dot, its status and,
/// when something is wrong, the one action that fixes it.
private struct SidebarAgentBar: View {
    let viewModel: MainViewModel

    // The button sits below the text: beside it, a narrow sidebar would
    // cut off a reason the agent gives. Long reasons end in an ellipsis,
    // with the full text in the tooltip.
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 8) {
                Circle()
                    .fill(dotColor)
                    .frame(width: 8, height: 8)
                    .padding(.top, 3)
                    .accessibilityHidden(true)
                Text(viewModel.agentSummary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(4)
                    .help(viewModel.agentSummary)
                    .accessibilityAddTraits(.updatesFrequently)
            }
            if let action = viewModel.agentAction {
                Button { Task { await viewModel.perform(action) } } label: { actionTitle(action) }
                    .controlSize(.small)
                    .help(actionHelp(action))
            }
            // A failed resume stays beside the button that retries it.
            if let error = viewModel.safeModeError {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
            }
            // The detail explains a catalog that cannot load until a pair
            // is selected; then the explanation stays here, by the list.
            if viewModel.selectedPair != nil, let error = viewModel.pairError {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
                Button("Try Again") { Task { await viewModel.perform(.retry) } }
                    .controlSize(.small)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }

    private var dotColor: Color {
        switch viewModel.agentState {
        case .connected: .green
        case .connecting: .secondary
        case .attention: .orange
        case .off: .red
        }
    }

    private func actionTitle(_ action: AgentAction) -> Text {
        switch action {
        case .turnOn: Text("Turn On")
        case .openLoginItems: Text("Open Login Items")
        case .resumeSyncing: Text("Resume Syncing")
        case .retry: Text("Try Again")
        }
    }

    private func actionHelp(_ action: AgentAction) -> Text {
        switch action {
        case .turnOn: Text("Turn on the background agent that syncs your folders.")
        case .openLoginItems: Text("Allow Foldover in System Settings › General › Login Items.")
        case .resumeSyncing: Text(viewModel.safeModeText ?? "")
        case .retry: Text("Connect to the background agent again.")
        }
    }
}
