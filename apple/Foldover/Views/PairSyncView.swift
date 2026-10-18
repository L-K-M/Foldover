import SwiftUI

/// Preview, sync and their results for one pair.
struct PairSyncView: View {
    let viewModel: PairSyncViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Button("Preview") { Task { await viewModel.preview() } }
                    .disabled(!viewModel.canStart)
                Button("Sync Now") { Task { await viewModel.sync() } }
                    .keyboardShortcut("s")
                    .disabled(!viewModel.canStart)
                if viewModel.isWorking {
                    ProgressView().controlSize(.small)
                    Button("Cancel") { Task { await viewModel.cancel() } }
                        .disabled(!viewModel.canCancel)
                }
            }
            Text(viewModel.statusText)
                .foregroundStyle(.secondary)
                .accessibilityAddTraits(.updatesFrequently)
            if let error = viewModel.requestError {
                Text(error).foregroundStyle(.red)
            }
            if !viewModel.rows.isEmpty {
                List(viewModel.rows) { row in
                    HStack {
                        VStack(alignment: .leading) {
                            Text(row.path).lineLimit(1).truncationMode(.middle)
                            Text(row.description).font(.caption).foregroundStyle(.secondary)
                        }
                        .accessibilityElement(children: .combine)
                        .contextMenu {
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
                if let hidden = viewModel.hiddenRowsText {
                    Text(hidden).font(.caption).foregroundStyle(.secondary)
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
        .confirmationDialog("Archive Many Items?", isPresented: Binding(
            get: { viewModel.approvalRequest != nil }, set: { _ in viewModel.declineApproval() }
        )) {
            Button("Archive and Sync", role: .destructive) {
                guard let approval = viewModel.approval else { return }
                Task { await viewModel.approve(approval) }
            }
            Button("Cancel", role: .cancel) { viewModel.declineApproval() }
        } message: {
            Text(viewModel.approvalRequest ?? "")
        }
        .task(id: viewModel.pair.id) { await viewModel.refresh() }
    }
}
