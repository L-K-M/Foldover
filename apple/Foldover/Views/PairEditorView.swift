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
            }
            .disabled(viewModel.isSaving)
            Text("Saving configures the pair. Preview and sync are not available yet.")
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
