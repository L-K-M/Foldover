import QuickLook
import SwiftUI

/// Preview, sync and their results for one pair.
struct PairSyncView: View {
    let viewModel: PairSyncViewModel
    @State private var selection: ChangeRow.ID?
    @State private var looking: URL?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Button("Preview") { Task { await viewModel.preview() } }
                    .keyboardShortcut("r")
                    .disabled(!viewModel.canStart)
                Button("Sync Now") { Task { await viewModel.sync() } }
                    .keyboardShortcut("s")
                    .disabled(!viewModel.canSync)
                if viewModel.isWorking || viewModel.isResolving {
                    ProgressView().controlSize(.small)
                }
                if viewModel.isWorking {
                    Button("Cancel") { Task { await viewModel.cancel() } }
                        .keyboardShortcut(".")
                        .disabled(!viewModel.canCancel)
                }
                if viewModel.canUndo {
                    Button("Undo") { Task { await viewModel.undo() } }
                        .help("Put back what this sync replaced or archived, and archive what it created.")
                }
            }
            Text(viewModel.statusText)
                .foregroundStyle(.secondary)
                .accessibilityAddTraits(.updatesFrequently)
            if let fraction = viewModel.progressFraction {
                ProgressView(value: fraction)
                    .accessibilityLabel("Progress")
            }
            if let error = viewModel.requestError {
                Text(error).foregroundStyle(.red)
            }
            if viewModel.showsChangeFilter {
                Picker("Show", selection: Binding(
                    get: { viewModel.changeFilter }, set: { filter in Task { await viewModel.showChanges(filter) } }
                )) {
                    Text(PairSyncViewModel.title(nil)).tag(ChangeFilter?.none)
                    ForEach(ChangeFilter.allCases, id: \.self) { filter in
                        Text(PairSyncViewModel.title(filter)).tag(ChangeFilter?.some(filter))
                    }
                }
                .pickerStyle(.segmented)
                .fixedSize()
            }
            if !viewModel.rows.isEmpty {
                List(viewModel.rows, selection: $selection) { row in
                    HStack {
                        VStack(alignment: .leading) {
                            Text(row.path).lineLimit(1).truncationMode(.middle)
                            Text(row.description).font(.caption).foregroundStyle(.secondary)
                        }
                        .accessibilityElement(children: .combine)
                        .contextMenu {
                            if let url = row.look {
                                Button("Quick Look") { looking = url }
                            }
                            if let side = row.otherWay, let title = row.otherWayTitle {
                                Button(title) { Task { await viewModel.copyOtherWay(row) } }
                                    .disabled(!viewModel.canStart)
                                    .help("Makes the other folder match \(PairSyncViewModel.folderName(side)).")
                            }
                            if row.item != nil {
                                Button("Skip Once") { Task { await viewModel.skipOnce(row) } }
                                    .disabled(!viewModel.canStart)
                            }
                            if row.skippable != nil {
                                Button("Always Skip") { Task { await viewModel.skipAlways(row) } }
                                    .disabled(!viewModel.canStart)
                            }
                        }
                        if row.resolvable != nil {
                            Spacer()
                            Button("Keep A") { Task { await viewModel.keep(.a, for: row) } }
                                .disabled(!viewModel.canStart)
                            Button("Keep B") { Task { await viewModel.keep(.b, for: row) } }
                                .disabled(!viewModel.canStart)
                            if row.keepsBoth {
                                Button("Keep Both") { Task { await viewModel.keepBoth(for: row) } }
                                    .disabled(!viewModel.canStart)
                                    .help("Keep Folder A's version here and save Folder B's beside it, named \"(from Folder B, date)\".")
                            }
                        }
                    }
                }
                .frame(minHeight: 160)
                // Space shows the selected change's file, as in Finder.
                .onKeyPress(.space) {
                    guard let url = viewModel.rows.first(where: { $0.id == selection })?.look else {
                        return .ignored
                    }
                    looking = url
                    return .handled
                }
                .quickLookPreview($looking)
                if let hidden = viewModel.hiddenRowsText {
                    HStack {
                        Text(hidden).font(.caption).foregroundStyle(.secondary)
                        if viewModel.canLoadMoreChanges {
                            Button("Show More") { Task { await viewModel.loadMoreChanges() } }
                        }
                    }
                }
            }
            if let note = viewModel.archiveNote {
                Text(note).font(.caption).foregroundStyle(.secondary)
            }
            if !viewModel.historyRows.isEmpty {
                Text("History").font(.headline)
                ForEach(viewModel.historyRows) { row in
                    VStack(alignment: .leading) {
                        Text(row.when).font(.caption).foregroundStyle(.secondary)
                        Text(row.summary)
                    }
                    .accessibilityElement(children: .combine)
                }
            }
        }
        // The dialog acts on the request captured when it was presented,
        // so a state change cannot redirect the tap to another plan.
        .confirmationDialog("Approve Sync?", isPresented: Binding(
            get: { viewModel.approvalRequest != nil },
            set: { if !$0 { viewModel.declineApproval() } }
        ), presenting: viewModel.approvalRequest) { request in
            Button("Archive and Sync", role: .destructive) {
                Task { await viewModel.approve(request) }
            }
            Button("Cancel", role: .cancel) { viewModel.declineApproval() }
        } message: { request in
            Text(request.message)
        }
        .task(id: viewModel.pair.id) { await viewModel.refresh() }
    }
}
