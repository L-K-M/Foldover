// The selected pair (PLAN.md §3.8): where its folders are, what a sync
// will do or did, and the planned changes to review.

import AppKit
import SwiftUI

struct PairDetailView: View {
    @Bindable var viewModel: MainViewModel
    let pair: SyncPair

    var body: some View {
        let sync = viewModel.sync(for: pair)
        // The split view sizes the detail, and a GeometryReader takes that
        // size without reporting a minimum of its own. With the change
        // table and the fixed-size segments inside, the column's minimum
        // otherwise changed from one layout pass to the next until AppKit
        // stopped the loop by throwing.
        GeometryReader { proxy in
            layout(sync)
                .frame(width: proxy.size.width, height: proxy.size.height, alignment: .topLeading)
        }
        // The dialog acts on the request captured when it was presented,
        // so a state change cannot redirect the tap to another plan.
        .confirmationDialog("Approve Sync?", isPresented: Binding(
            get: { sync.approvalRequest != nil },
            set: { if !$0 { sync.declineApproval() } }
        ), presenting: sync.approvalRequest) { request in
            Button("Archive and Sync", role: .destructive) {
                Task { await viewModel.approveInWindow(request) }
            }
            Button("Cancel", role: .cancel) { sync.declineApproval() }
        } message: { request in
            Text(request.message)
        }
        .task(id: pair.id) { await viewModel.sync(for: pair).refresh() }
    }

    private func layout(_ sync: PairSyncViewModel) -> some View {
        let rows = sync.rows
        // A placeholder says what the status bar would, with its buttons;
        // the bar stays only to show a failed request beside it.
        let placeholderSpeaks = rows.isEmpty && sync.placeholder != nil && sync.requestError == nil
        return VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 12) {
                folderHeader
                if !placeholderSpeaks {
                    statusBar(sync)
                }
                if sync.showsChangeFilter {
                    scopeBar(sync)
                }
            }
            .padding(.horizontal, 20)
            .padding(.top, 16)
            .padding(.bottom, 12)

            changes(sync, rows: rows)
                .frame(maxWidth: .infinity, maxHeight: .infinity)

            // Failures beyond the first page; a preview's say so in the scope bar.
            if !sync.showsChangeFilter && sync.hiddenRowsText != nil {
                HStack {
                    Spacer()
                    hiddenRows(sync)
                }
                .padding(.horizontal, 20)
                .padding(.vertical, 8)
            }
        }
    }

    // MARK: Folders

    private var folderHeader: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                DetailFolderChip(viewModel: viewModel, folder: pair.settings.left, side: .a)
                Image(systemName: pair.settings.mode.symbol)
                    .foregroundStyle(.tint)
                    .help(pair.settings.mode.title)
                    .accessibilityLabel(pair.settings.mode.title)
                DetailFolderChip(viewModel: viewModel, folder: pair.settings.right, side: .b)
                Spacer(minLength: 0)
            }
            // No vertical fixedSize in a split-view column: a minimum height
            // that depends on the width makes the column's size limits
            // change on every layout pass, which AppKit ends with a crash.
            Text(viewModel.planText(for: pair))
                .font(.callout)
                .foregroundStyle(.secondary)
                .lineLimit(3)
                .help(viewModel.planText(for: pair))
        }
    }

    // MARK: Status

    private func statusBar(_ sync: PairSyncViewModel) -> some View {
        GroupBox {
            HStack(alignment: .top, spacing: 10) {
                statusSymbol(sync.activity)
                    .frame(width: 20)
                VStack(alignment: .leading, spacing: 6) {
                    Text(sync.statusText)
                        .font(.callout)
                        .lineLimit(4)
                        .textSelection(.enabled)
                        .accessibilityAddTraits(.updatesFrequently)
                    if let fraction = sync.progressFraction {
                        ProgressView(value: fraction)
                            .accessibilityLabel("Progress")
                    }
                    if let error = sync.requestError {
                        Label(error, systemImage: "exclamationmark.octagon")
                            .font(.callout)
                            .foregroundStyle(.red)
                            .textSelection(.enabled)
                    }
                    if let note = sync.archiveNote {
                        Text(note)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                statusButtons(sync)
            }
            .padding(4)
        }
    }

    @ViewBuilder
    private func statusSymbol(_ activity: PairActivity) -> some View {
        switch activity {
        case .working:
            ProgressView()
                .controlSize(.small)
        case .attention:
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
                .accessibilityHidden(true)
        case .waiting:
            Image(systemName: "externaldrive.badge.xmark")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
        case .idle:
            Image(systemName: "info.circle")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
        }
    }

    /// Stop while a job runs; after a sync, what can follow it.
    private func statusButtons(_ sync: PairSyncViewModel) -> some View {
        HStack(spacing: 8) {
            if sync.isWorking {
                Button("Stop") { Task { await sync.cancel() } }
                    .disabled(!sync.canCancel)
            }
            if let card = sync.completionCard {
                if card.conflicts > 0 {
                    Button("Review \(card.conflicts) Conflicts") {
                        Task { await sync.reviewConflicts() }
                    }
                    .disabled(!sync.canStart)
                }
                if card.undoable {
                    undoButton(sync)
                }
            } else if sync.canUndo {
                undoButton(sync)
            }
        }
    }

    private func undoButton(_ sync: PairSyncViewModel) -> some View {
        Button("Undo Sync") { Task { await viewModel.undoSync(pairID: pair.id) } }
            .disabled(!sync.canUndo)
            .help("Put back what this sync replaced or archived, and archive what it created.")
    }

    // MARK: Changes

    /// Which kinds of change the table lists, as a pop-up that fits the
    /// narrowest detail (six segments do not beside the inspector), with
    /// the hidden-rows caption beside it. View › Show has the same
    /// choices with ⌘1 to ⌘6.
    private func scopeBar(_ sync: PairSyncViewModel) -> some View {
        HStack(spacing: 12) {
            Picker("Show", selection: Binding(
                get: { sync.changeFilter },
                set: { filter in Task { await sync.showChanges(filter) } }
            )) {
                Text(PairSyncViewModel.title(nil)).tag(ChangeFilter?.none)
                ForEach(ChangeFilter.allCases, id: \.self) { filter in
                    Text(PairSyncViewModel.title(filter)).tag(ChangeFilter?.some(filter))
                }
            }
            .pickerStyle(.menu)
            .fixedSize()
            Spacer(minLength: 8)
            hiddenRows(sync)
        }
    }

    /// Rows the agent has not sent yet, and the button that fetches them.
    @ViewBuilder
    private func hiddenRows(_ sync: PairSyncViewModel) -> some View {
        if let hidden = sync.hiddenRowsText {
            HStack(spacing: 8) {
                Text(hidden)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                if sync.canLoadMoreChanges {
                    Button("Show More") { Task { await sync.loadMoreChanges() } }
                        .controlSize(.small)
                }
            }
        }
    }

    @ViewBuilder
    private func changes(_ sync: PairSyncViewModel, rows: [ChangeRow]) -> some View {
        if !rows.isEmpty {
            changeTable(sync, rows: rows)
        } else if sync.isWorking || sync.isResolving {
            // The status bar above carries the details.
            ProgressView(sync.shortStatus)
                .controlSize(.large)
        } else if let placeholder = sync.placeholder {
            ContentUnavailableView {
                Label(placeholder.title, systemImage: placeholder.symbol)
            } description: {
                Text(placeholder.message)
                if let note = sync.archiveNote {
                    Text(note)
                        .font(.caption)
                }
            } actions: {
                if let action = placeholder.action {
                    placeholderButton(action, sync: sync)
                }
                statusButtons(sync)
            }
        } else if sync.showsSearch {
            // Last: a search left in the field must not hide progress or
            // a placeholder. There is none while changes are listed, so
            // this is a search that hides every listed row.
            ContentUnavailableView.search(text: sync.searchText)
        }
    }

    @ViewBuilder
    private func placeholderButton(_ action: PlaceholderAction, sync: PairSyncViewModel) -> some View {
        switch action {
        case .preview:
            Button("Preview Changes") { Task { await sync.preview() } }
                .disabled(!sync.canStart)
        }
    }

    private func changeTable(_ sync: PairSyncViewModel, rows: [ChangeRow]) -> some View {
        Table(rows, selection: $viewModel.selectedChangeID) {
            TableColumn("") { row in
                Image(systemName: row.symbol)
                    .foregroundStyle(Self.color(row.tone))
                    .accessibilityLabel(row.actionText)
            }
            .width(24)
            TableColumn("Item") { row in
                Text(row.path)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(row.path)
            }
            .width(min: 120, ideal: 280)
            TableColumn("Change") { row in
                Text(row.actionText)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .help(row.actionText)
            }
            .width(min: 100, ideal: 200)
            TableColumn("Size") { row in
                Text(row.sizeText)
                    .monospacedDigit()
                    .frame(maxWidth: .infinity, alignment: .trailing)
            }
            .width(min: 70, ideal: 80, max: 90)
        }
        .alternatingRowBackgrounds()
        .contextMenu(forSelectionType: ChangeRow.ID.self) { ids in
            if let row = rows.first(where: { ids.contains($0.id) }) {
                ChangeRowActions(viewModel: viewModel, sync: sync, row: row)
            }
        } primaryAction: { ids in
            // Double-clicking a change looks at its file.
            guard let url = rows.first(where: { ids.contains($0.id) })?.look else { return }
            viewModel.quickLookURL = url
        }
        // Space shows the selected change's file, as in Finder.
        .onKeyPress(.space) {
            guard let url = viewModel.selectedChange?.look else { return .ignored }
            viewModel.quickLookURL = url
            return .handled
        }
        // Edit > Copy copies the selected change's path.
        .onCopyCommand {
            guard let path = viewModel.selectedChange?.path else { return [] }
            return [NSItemProvider(object: path as NSString)]
        }
    }

    private static func color(_ tone: ChangeTone) -> Color {
        switch tone {
        case .copy: .accentColor
        case .update, .archive, .neutral: .secondary
        case .conflict: .orange
        case .failure: .red
        }
    }
}

/// What a person can do with one listed change: look at it, or steer
/// what the next sync does with it. The table's context menu and the
/// inspector show the same commands.
struct ChangeRowActions: View {
    let viewModel: MainViewModel
    let sync: PairSyncViewModel
    let row: ChangeRow

    /// Whether the row offers any command, so an empty group can be left out.
    static func hasActions(_ row: ChangeRow) -> Bool {
        row.look != nil || row.resolvable != nil || row.keepsBoth || row.otherWay != nil
            || row.item != nil || row.skippable != nil
    }

    var body: some View {
        if let url = row.look {
            Button("Quick Look") { viewModel.quickLookURL = url }
            Button("Show in Finder") { sync.reveal(row) }
        }
        if row.resolvable != nil {
            Button("Keep Folder A’s Version") { Task { await sync.keep(.a, for: row) } }
                .disabled(!sync.canStart)
            Button("Keep Folder B’s Version") { Task { await sync.keep(.b, for: row) } }
                .disabled(!sync.canStart)
        }
        if row.keepsBoth {
            Button("Keep Both Versions") { Task { await sync.keepBoth(for: row) } }
                .disabled(!sync.canStart)
                .help("Keep Folder A’s version here and save Folder B’s beside it, named \"(from Folder B, date)\".")
        }
        if let side = row.otherWay, let title = row.otherWayTitle {
            Button(title) { Task { await sync.copyOtherWay(row) } }
                .disabled(!sync.canStart)
                .help("Makes the other folder match \(PairSyncViewModel.folderName(side)).")
        }
        if row.item != nil {
            Button("Skip Once") { Task { await sync.skipOnce(row) } }
                .disabled(!sync.canStart)
        }
        if row.skippable != nil {
            Button("Always Skip") { Task { await sync.skipAlways(row) } }
                .disabled(!sync.canStart)
        }
    }
}

/// One of the pair's folders: where it is, its full path on hover, and
/// a menu to show it in Finder or copy its path.
private struct DetailFolderChip: View {
    let viewModel: MainViewModel
    let folder: SelectedFolder
    let side: FolderSide

    var body: some View {
        let text = viewModel.folderText(folder)
        Label(text, systemImage: viewModel.folderSymbol(folder))
            .lineLimit(1)
            .truncationMode(.middle)
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(.quaternary, in: .rect(cornerRadius: 6))
            .help(text)
            .accessibilityLabel(Text("\(PairSyncViewModel.folderName(side)): \(text)"))
            .contextMenu {
                if viewModel.localURL(folder) != nil {
                    Button("Show in Finder") { viewModel.reveal(side) }
                }
                Button("Copy Path") {
                    let pasteboard = NSPasteboard.general
                    pasteboard.clearContents()
                    pasteboard.setString(PairEditorViewModel.pathText(folder), forType: .string)
                }
            }
    }
}
