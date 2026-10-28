// The selected pair (PLAN.md §3.8): what the pair does, which changes
// are listed, and the sheet that previews, steers and syncs them.

import AppKit
import SwiftUI

struct PairDetailView: View {
    @Bindable var viewModel: MainViewModel
    let pair: SyncPair
    let focus: FocusState<MainPane?>.Binding

    var body: some View {
        let sync = viewModel.sync(for: pair)
        SplitColumn(alignment: .topLeading) {
            VStack(spacing: 0) {
                deskBar(sync)
                    .padding(.horizontal, 16)
                    .padding(.top, 12)
                    .padding(.bottom, 8)
                    // On the navy field in either appearance.
                    .environment(\.colorScheme, .dark)
                FoldSheet(viewModel: viewModel, sync: sync, pair: pair, rows: sync.rows, focus: focus)
            }
            // The field starts under the toolbar, whose title is drawn for
            // the window's own appearance.
            .background { Desk() }
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

    /// Above the sheet: the pair's plan in words, which kinds of change
    /// it lists, and the rows the agent has not sent yet. The pop-up
    /// fits the narrowest detail; View › Show has the same choices with
    /// ⌘1 to ⌘6.
    private func deskBar(_ sync: PairSyncViewModel) -> some View {
        HStack(spacing: 12) {
            // No vertical fixedSize in a split-view column: a minimum height
            // that depends on the width makes the column's size limits
            // change on every layout pass, which AppKit ends with a crash.
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                // The mode's direction, here rather than on the sheet's fold.
                Image(systemName: pair.settings.mode.symbol)
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(FoldPalette.deskInk)
                    .help(pair.settings.mode.title)
                    .accessibilityLabel(pair.settings.mode.title)
                Text(viewModel.planText(for: pair))
                    .font(.callout)
                    .foregroundStyle(FoldPalette.deskInk)
                    .lineLimit(2)
                    .help(viewModel.planText(for: pair))
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            // One stop for VoiceOver: the mode, then its sentence.
            .accessibilityElement(children: .combine)
            if sync.showsChangeFilter {
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
            }
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
}

/// What a person can do with one listed change: look at it, or steer
/// what the next sync does with it. The sheet's context menu and the
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
