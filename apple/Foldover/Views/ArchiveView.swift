import SwiftUI

/// Browse what syncs archived in each folder and put items back.
struct ArchiveView: View {
    let viewModel: ArchiveViewModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(viewModel.title).font(.title2)
            Text(viewModel.explanation).font(.callout).foregroundStyle(.secondary)
            Picker("Folder", selection: Binding(
                get: { viewModel.side }, set: { side in Task { await viewModel.show(side) } }
            )) {
                Text("Folder A").tag(FolderSide.a)
                Text("Folder B").tag(FolderSide.b)
            }
            .pickerStyle(.segmented)
            .fixedSize()
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading) {
                    List(viewModel.runRows, selection: Binding(
                        get: { viewModel.selectedRun },
                        set: { run in
                            guard let run else { return }
                            Task { await viewModel.select(run) }
                        }
                    )) { row in
                        Text(row.title).tag(row.id)
                    }
                    if let note = viewModel.olderRunsNote {
                        Text(note).font(.caption).foregroundStyle(.secondary)
                    }
                }
                .frame(width: 220)
                VStack(alignment: .leading) {
                    List(viewModel.itemRows) { row in
                        HStack {
                            VStack(alignment: .leading) {
                                Text(row.path).lineLimit(1).truncationMode(.middle)
                                Text(row.size).font(.caption).foregroundStyle(.secondary)
                            }
                            .accessibilityElement(children: .combine)
                            Spacer()
                            Button("Put Back") { Task { await viewModel.putBack(row) } }
                                .disabled(!viewModel.canPutBack)
                        }
                    }
                    if viewModel.canLoadMore {
                        Button("Show More") { Task { await viewModel.loadMore() } }
                    }
                }
            }
            .overlay {
                if let empty = viewModel.emptyText {
                    Text(empty).foregroundStyle(.secondary)
                } else if viewModel.isLoading {
                    ProgressView()
                }
            }
            if let placed = viewModel.placedText {
                Text(placed).foregroundStyle(.secondary)
            }
            if let error = viewModel.error {
                Text(error).foregroundStyle(.red).textSelection(.enabled)
            }
            HStack {
                Spacer()
                Button("Done") { dismiss() }.keyboardShortcut(.cancelAction)
            }
        }
        .padding(20)
        .frame(minWidth: 680, minHeight: 440)
        .task { await viewModel.load() }
    }
}
