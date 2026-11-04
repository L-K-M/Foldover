// The main window's trailing inspector: the selected change, the
// selected pair's settings at a glance, and its recent syncs.

import SwiftUI

struct PairInspector: View {
    let viewModel: MainViewModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Group {
            if let pair = viewModel.selectedPair {
                form(pair)
            } else {
                ContentUnavailableView("No Pair Selected", systemImage: "sidebar.trailing")
            }
        }
    }

    private func form(_ pair: SyncPair) -> some View {
        let sync = viewModel.sync(for: pair)
        let history = sync.historyRows
        return Form {
            if let row = viewModel.selectedChange {
                Section("Selected Change") {
                    Text(row.path)
                        .textSelection(.enabled)
                    LabeledContent("Change", value: row.actionText)
                    if !row.sizeText.isEmpty {
                        LabeledContent("Size", value: row.sizeText)
                    }
                    if ChangeRowActions.hasActions(row) {
                        VStack(alignment: .leading, spacing: 6) {
                            ChangeRowActions(viewModel: viewModel, sync: sync, row: row)
                        }
                        .buttonStyle(.bordered)
                    }
                }
            }

            Section("Pair") {
                if let notice = sync.previousSettingsNotice {
                    Text(notice)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                LabeledContent("Mode") {
                    HStack(spacing: 6) {
                        Text(sync.activeMode.title)
                        InfoButton(title: Text("Sync Mode"), message: Text(sync.activeMode.explanation))
                    }
                }
                folderRow("Folder A", folder: sync.activeLeft)
                folderRow("Folder B", folder: sync.activeRight)
                LabeledContent(sync.usesPreviousSettings ? "Schedule (Next Run)" : "Schedule",
                               value: PairEditorViewModel.scheduleTitle(pair.settings.options.schedule))
                LabeledContent(sync.usesPreviousSettings ? "Skips (Next Run)" : "Skips",
                               value: viewModel.skipSummary(for: pair))
                HStack {
                    Button("Edit Pair…") { viewModel.editPair() }
                        .disabled(!viewModel.canConfigurePairs)
                    Button("Browse Archive…") { openWindow(id: "archive", value: pair.id) }
                }
            }

            Section("Recent Syncs") {
                if history.isEmpty {
                    Text("No syncs yet")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(history) { row in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(row.when)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            Text(row.summary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        .accessibilityElement(children: .combine)
                    }
                }
            }
        }
        .formStyle(.grouped)
    }

    /// A folder's location, with a button that shows it in Finder when
    /// it is on this Mac.
    private func folderRow(_ title: LocalizedStringKey, folder: SelectedFolder) -> some View {
        let text = viewModel.folderText(folder)
        return LabeledContent(title) {
            HStack(spacing: 4) {
                Text(text)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(text)
                if viewModel.canReveal(folder) {
                    Button("Show in Finder", systemImage: "arrow.forward.circle.fill") {
                        viewModel.reveal(folder)
                    }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.borderless)
                    .help("Show in Finder")
                }
            }
        }
    }
}
