// The archive window: one pair's archive browsed beside the main
// window, so it can stay open while you work.

import SwiftUI

/// One archive window. It owns its store, so each window browses on
/// its own.
struct ArchiveWindow: View {
    let pairID: String
    let viewModel: MainViewModel
    @State private var store: ArchiveStore?

    var body: some View {
        content
            .frame(minWidth: 600, minHeight: 340)
            .onAppear {
                if store == nil { store = viewModel.makeArchiveStore() }
            }
    }

    @ViewBuilder
    private var content: some View {
        if let store {
            if let archive = viewModel.archive(for: pairID, store: store) {
                ArchiveView(viewModel: archive)
            } else if !viewModel.pairsLoaded {
                // A window restored at launch opens before the catalog
                // loads, and waits here while the agent is off.
                ContentUnavailableView {
                    Label(viewModel.detailTitle, systemImage: "archivebox")
                } description: {
                    Text(viewModel.pairError ?? viewModel.agentStatusText)
                }
                .navigationTitle("Archive")
            } else {
                ContentUnavailableView("Pair Removed", systemImage: "folder.badge.minus",
                                       description: Text("This pair no longer exists. You can close this window."))
                    .navigationTitle("Archive")
            }
        } else {
            Color.clear
        }
    }
}

/// A pair's archive: its runs on the left, what the chosen run archived
/// on the right, and Put Back in the toolbar.
struct ArchiveView: View {
    let viewModel: ArchiveViewModel
    @State private var selection: Set<ArchiveItemRow.ID> = []
    @State private var confirmingPutBackAll = false
    /// Set once loading has begun, so a store that has not opened yet
    /// does not read as an empty archive.
    @State private var hasStarted = false

    var body: some View {
        let items = viewModel.itemRows
        let selected = selectedRows(selection, in: items)
        NavigationSplitView {
            runList
                .navigationSplitViewColumnWidth(min: 170, ideal: 200, max: 280)
        } detail: {
            detail(items)
                .navigationTitle(viewModel.pairName)
                .navigationSubtitle("Archive")
        }
        .toolbar { archiveToolbar(selected: selected) }
        .confirmationDialog("Put Back All?", isPresented: $confirmingPutBackAll) {
            Button("Put Back All") { Task { await viewModel.putBackAll() } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(viewModel.putBackAllQuestion)
        }
        // A selection belongs to the list it was made in.
        .onChange(of: viewModel.side) { selection.removeAll() }
        .onChange(of: viewModel.selectedRun) { selection.removeAll() }
        .task(id: viewModel.pair.id) {
            hasStarted = true
            await viewModel.load()
        }
    }

    // MARK: Runs

    private var runList: some View {
        List(viewModel.runRows, selection: runSelection) { row in
            Text(row.title).tag(row.id)
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if let note = viewModel.olderRunsNote {
                Text(note)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
            }
        }
    }

    private var runSelection: Binding<String?> {
        Binding(
            get: { viewModel.selectedRun },
            set: { run in
                guard let run else { return }
                Task { await viewModel.select(run) }
            }
        )
    }

    // MARK: Items

    /// In a GeometryReader, so the table cannot make the split view's
    /// column limits change from one layout pass to the next.
    private func detail(_ items: [ArchiveItemRow]) -> some View {
        GeometryReader { _ in
            VStack(spacing: 0) {
                content(items)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                Divider()
                statusBar(items)
            }
        }
    }

    @ViewBuilder
    private func content(_ items: [ArchiveItemRow]) -> some View {
        if !items.isEmpty {
            itemTable(items)
        } else if hasStarted, let empty = viewModel.emptyText {
            ContentUnavailableView {
                Label("Nothing Archived", systemImage: "archivebox")
            } description: {
                Text(empty)
            }
        } else if viewModel.error != nil {
            // The status line below says what went wrong.
            ContentUnavailableView("Cannot Load Archive", systemImage: "exclamationmark.triangle")
        } else {
            ProgressView()
                .controlSize(.large)
                .accessibilityLabel("Loading Archive")
        }
    }

    private func itemTable(_ items: [ArchiveItemRow]) -> some View {
        Table(items, selection: $selection) {
            TableColumn("Item") { row in
                Text(row.path)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(row.path)
            }
            .width(min: 160, ideal: 420)
            TableColumn("Size") { row in
                Text(row.size)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .trailing)
            }
            .width(min: 70, ideal: 80, max: 100)
        }
        .alternatingRowBackgrounds()
        // Double-clicking does nothing: putting back is a deliberate choice.
        .contextMenu(forSelectionType: ArchiveItemRow.ID.self) { ids in
            if !ids.isEmpty {
                Button("Put Back") { putBack(selectedRows(ids, in: items)) }
                    .disabled(!viewModel.canPutBack)
            }
        }
    }

    /// Put-back status and on-demand guidance stay available together.
    private func statusBar(_ items: [ArchiveItemRow]) -> some View {
        HStack(spacing: 12) {
            statusLine
                .lineLimit(3)
            Spacer(minLength: 0)
            if viewModel.isLoading && !items.isEmpty {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel("Loading More Archive Items")
            }
            if viewModel.canLoadMore {
                Button("Show More") { Task { await viewModel.loadMore() } }
                    .controlSize(.small)
            }
            InfoButton(
                title: Text("Archive"),
                message: Text("\(viewModel.explanation)\n\nChoose the folder and sync that held the version you need, then select items and use Put Back. Each archive stays with its folder, so that disk or server must be available. The archive is short-term recovery, not a separate backup against losing the disk.")
            )
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 8)
    }

    @ViewBuilder
    private var statusLine: some View {
        if let error = viewModel.error {
            Label(error, systemImage: "exclamationmark.octagon")
                .font(.callout)
                .foregroundStyle(.red)
                .textSelection(.enabled)
        } else if let status = viewModel.statusText {
            Text(status)
                .font(.callout)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .accessibilityAddTraits(.updatesFrequently)
        }
    }

    // MARK: Toolbar

    // The side picker sits in the middle so the window reads as one
    // folder's archive at a time.
    @ToolbarContentBuilder
    private func archiveToolbar(selected: [ArchiveItemRow]) -> some ToolbarContent {
        ToolbarItem(placement: .principal) {
            Picker("Folder", selection: sideSelection) {
                Text(ArchiveViewModel.sideTitle(.a)).tag(FolderSide.a)
                Text(ArchiveViewModel.sideTitle(.b)).tag(FolderSide.b)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
        }
        ToolbarItem(placement: .primaryAction) {
            Button("Put Back") { putBack(selected) }
                .help("Put the selected items back where they were")
                .disabled(selected.isEmpty || !viewModel.canPutBack)
        }
        ToolbarItem(placement: .primaryAction) {
            Button("Put Back All…") { confirmingPutBackAll = true }
                .help("Put back everything this sync archived")
                .disabled(!viewModel.canPutBackAll)
        }
    }

    private var sideSelection: Binding<FolderSide> {
        Binding(
            get: { viewModel.side },
            set: { side in Task { await viewModel.show(side) } }
        )
    }

    // MARK: Actions

    private func selectedRows(_ ids: Set<ArchiveItemRow.ID>, in items: [ArchiveItemRow]) -> [ArchiveItemRow] {
        items.filter { ids.contains($0.id) }
    }

    private func putBack(_ rows: [ArchiveItemRow]) {
        guard !rows.isEmpty else { return }
        Task {
            await viewModel.putBack(rows)
            // Items put back leave the list; keep the selection that is still in it.
            selection.formIntersection(viewModel.itemRows.map(\.id))
        }
    }
}
