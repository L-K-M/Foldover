import SwiftUI
import UniformTypeIdentifiers

struct PairEditorView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var viewModel: PairEditorViewModel
    @State private var choosingSide = PairFolderSide.left
    @State private var choosingFolder = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(viewModel.title).font(.title2)
            Form {
                TextField("Name", text: $viewModel.name)
                LabeledContent("Folder A") {
                    Text(viewModel.folderText(.left)).lineLimit(2).textSelection(.enabled)
                    Button("Choose A…") { choosingSide = .left; choosingFolder = true }
                }
                LabeledContent("Folder B") {
                    Text(viewModel.folderText(.right)).lineLimit(2).textSelection(.enabled)
                    Button("Choose B…") { choosingSide = .right; choosingFolder = true }
                }
                Picker("Mode", selection: $viewModel.mode) {
                    ForEach(PairMode.allCases, id: \.self) { mode in Text(mode.title).tag(mode) }
                }
                Text(viewModel.mode.explanation).foregroundStyle(.secondary)
                Section("Metadata") {
                    Toggle("Modification dates", isOn: $viewModel.options.syncModificationTimes)
                    Toggle("Tags, Finder info and other extended attributes",
                           isOn: $viewModel.options.syncExtendedAttributes)
                    Toggle("Permissions and access lists", isOn: $viewModel.options.syncPermissions)
                    Text("Resource forks always sync. Without permissions, a file's executable bit still syncs.")
                        .font(.callout).foregroundStyle(.secondary)
                    if viewModel.resetsSyncHistory {
                        Text("Changing metadata settings resets this pair's sync history. The next preview compares both folders from scratch.")
                            .font(.callout).foregroundStyle(.orange)
                    }
                }
                Section("Skip") {
                    TextEditor(text: $viewModel.filterText)
                        .font(.body.monospaced())
                        .frame(minHeight: 60)
                        .accessibilityLabel("Rules for files to skip")
                    Text("One rule per line, like .gitignore: *.tmp skips temporary files, build/ skips folders named build. Rules apply to both folders; a .foldoverignore file in either folder adds more.")
                        .font(.callout).foregroundStyle(.secondary)
                    Toggle("Skip large files", isOn: $viewModel.limitsSize)
                    if viewModel.limitsSize {
                        TextField("Larger than (MB)", value: $viewModel.sizeLimitMegabytes, format: .number)
                        Text("A file this large in either folder is skipped in both, so it is never copied or deleted.")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                }
                Section("Schedule") {
                    Picker("Sync automatically", selection: $viewModel.options.schedule) {
                        ForEach(viewModel.scheduleChoices, id: \.self) { schedule in
                            Text(PairEditorViewModel.scheduleTitle(schedule)).tag(schedule)
                        }
                    }
                    Text("Automatic syncs start after the first sync you run yourself. They stop instead of asking when they would archive many items.")
                        .font(.callout).foregroundStyle(.secondary)
                }
                Section("Durability") {
                    Picker("Write safety", selection: $viewModel.options.durability) {
                        ForEach(PairDurability.allCases, id: \.self) { durability in
                            Text(durability.title).tag(durability)
                        }
                    }
                    Text(viewModel.options.durability.explanation).foregroundStyle(.secondary)
                }
            }
            .disabled(viewModel.isSaving)
            Text("Saving configures the pair. Nothing is copied until you preview and sync.")
                .font(.callout).foregroundStyle(.secondary)
            if let message = viewModel.errorMessage {
                Text(message).foregroundStyle(.red).textSelection(.enabled)
                    .accessibilityLabel(String(localized: "Settings were not confirmed: \(message)"))
            }
            HStack {
                if viewModel.isSaving { ProgressView().controlSize(.small).accessibilityLabel("Saving pair") }
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction).disabled(viewModel.isSaving)
                Button("Save Pair") {
                    Task { if await viewModel.save() { dismiss() } }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!viewModel.canSave)
            }
        }
        .padding(24)
        .frame(width: 600)
        .interactiveDismissDisabled(viewModel.isSaving)
        .fileImporter(isPresented: $choosingFolder, allowedContentTypes: [.folder], allowsMultipleSelection: false) {
            viewModel.chooseFolder($0, side: choosingSide)
        }
    }
}
