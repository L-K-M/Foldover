import SwiftUI
import UniformTypeIdentifiers

struct PairEditorView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL
    @Bindable var viewModel: PairEditorViewModel
    @State private var choosingSide = PairFolderSide.left
    @State private var choosingFolder = false
    @State private var enteringServer: PairFolderSide?
    @State private var choosingOtherMac: PairFolderSide?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(viewModel.title).font(.title2)
            Form {
                TextField("Name", text: $viewModel.name)
                LabeledContent("Folder A") {
                    Text(viewModel.folderText(.left)).lineLimit(2).textSelection(.enabled)
                    Button("Choose A…") { choosingSide = .left; choosingFolder = true }
                    Button("SFTP…") { enteringServer = .left }
                    if viewModel.offersOtherMacs {
                        Button("Another Mac…") { choosingOtherMac = .left }
                    }
                }
                LabeledContent("Folder B") {
                    Text(viewModel.folderText(.right)).lineLimit(2).textSelection(.enabled)
                    Button("Choose B…") { choosingSide = .right; choosingFolder = true }
                    Button("SFTP…") { enteringServer = .right }
                    if viewModel.offersOtherMacs {
                        Button("Another Mac…") { choosingOtherMac = .right }
                    }
                }
                if let warning = viewModel.serverWarning {
                    Text(warning)
                        .font(.callout).foregroundStyle(.orange)
                        .textSelection(.enabled)
                }
                ForEach(viewModel.readinessWarnings, id: \.self) { warning in
                    Text(warning)
                        .font(.callout).foregroundStyle(.orange)
                        .textSelection(.enabled)
                }
                if !viewModel.nearbyServers.isEmpty {
                    Section("Servers nearby") {
                        ForEach(viewModel.nearbyServers) { server in
                            LabeledContent(server.name) {
                                Button("Connect") {
                                    if let url = viewModel.serverURL(server) { openURL(url) }
                                }
                            }
                        }
                        Text("Connect, choose a shared folder in Finder, then pick it here.")
                            .font(.callout).foregroundStyle(.secondary)
                    }
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
                    Text(viewModel.metadataNote)
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
                    Toggle("Skip old files", isOn: $viewModel.limitsAge)
                    if viewModel.limitsAge {
                        TextField("Unchanged for more than (days)", value: $viewModel.ageLimitDays, format: .number)
                        Text("Skipped only while unchanged in both folders; editing one makes it sync again.")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    TextField("Canary file", text: $viewModel.canary)
                    if let warning = viewModel.canaryWarning {
                        Text(warning)
                            .font(.callout).foregroundStyle(.orange)
                    }
                    Text("A file that must stay in both folders unchanged. If it goes missing or changes, syncs stop before writing anything.")
                        .font(.callout).foregroundStyle(.secondary)
                }
                Section("Schedule") {
                    Picker("Sync automatically", selection: $viewModel.options.schedule) {
                        ForEach(viewModel.scheduleChoices, id: \.self) { schedule in
                            Text(PairEditorViewModel.scheduleTitle(schedule)).tag(schedule)
                        }
                    }
                    if let note = viewModel.scheduleNote {
                        Text(note)
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    if viewModel.showsDailySettings {
                        DatePicker("At", selection: $viewModel.dailyTime, displayedComponents: .hourAndMinute)
                        LabeledContent("On") {
                            HStack(spacing: 4) {
                                ForEach(SyncDay.allCases, id: \.self) { day in
                                    Toggle(day.shortName, isOn: Binding(
                                        get: { viewModel.dailyDays.contains(day) },
                                        set: { on in
                                            var days = viewModel.dailyDays
                                            if on { days.insert(day) } else { days.remove(day) }
                                            viewModel.dailyDays = days
                                        }
                                    ))
                                    .toggleStyle(.button)
                                }
                            }
                        }
                        if let warning = viewModel.dailyWarning {
                            Text(warning)
                                .font(.callout).foregroundStyle(.orange)
                        }
                    }
                    Text("Automatic syncs start after the first sync you run yourself. They stop instead of asking when they would archive many items.")
                        .font(.callout).foregroundStyle(.secondary)
                }
                // The plan sentence rewrites itself as settings change (PLAN.md §3.8).
                Text(viewModel.planText).foregroundStyle(.secondary)
                Section("Durability") {
                    Picker("Write safety", selection: $viewModel.options.durability) {
                        ForEach(viewModel.durabilityChoices, id: \.self) { durability in
                            Text(durability.title).tag(durability)
                                .disabled(viewModel.usesServerFolder && durability == .full)
                        }
                    }
                    if let warning = viewModel.durabilityWarning {
                        Text(warning)
                            .font(.callout).foregroundStyle(.orange)
                            .textSelection(.enabled)
                    }
                    Text(viewModel.durabilityExplanation).foregroundStyle(.secondary)
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
        .sheet(item: $enteringServer, onDismiss: { viewModel.resetServerBrowser() }) { side in
            ServerFolderPicker(viewModel: viewModel, side: side)
        }
        .fileImporter(isPresented: $choosingFolder, allowedContentTypes: [.folder], allowsMultipleSelection: false) {
            viewModel.chooseFolder($0, side: choosingSide)
        }
        .sheet(item: $choosingOtherMac) { side in
            OtherMacFolderPicker(viewModel: viewModel, side: side)
        }
    }
}

/// A folder a paired Mac shares, for one side of a pair.
private struct OtherMacFolderPicker: View {
    @Environment(\.dismiss) private var dismiss
    let viewModel: PairEditorViewModel
    let side: PairFolderSide
    @State private var mac: PairedDevice?
    @State private var folders: [OfferedShare] = []
    @State private var loading = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Choose a Folder on Another Mac").font(.headline)
            Text(viewModel.otherMacsNote).font(.callout).foregroundStyle(.secondary)
            if !viewModel.pairedMacs.isEmpty {
                Picker("Mac", selection: $mac) {
                    Text("Choose a Mac").tag(PairedDevice?.none)
                    ForEach(viewModel.pairedMacs) { mac in
                        Text(mac.name).tag(PairedDevice?.some(mac))
                    }
                }
                List(folders) { folder in
                    Button(folder.name) {
                        if let mac {
                            viewModel.chooseShared(folder, on: mac, side: side)
                        }
                        dismiss()
                    }
                }
                .overlay {
                    if loading {
                        ProgressView()
                    } else if mac != nil && folders.isEmpty {
                        Text("That Mac shares no folders with this one.").foregroundStyle(.secondary)
                    }
                }
            }
            if let error = viewModel.otherMacsError {
                Text(error).foregroundStyle(.red).textSelection(.enabled)
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
            }
        }
        .padding()
        .frame(width: 380, height: 340)
        // A new selection cancels the last one's request, so a slow Mac's
        // folders never show under another Mac.
        .task(id: mac) {
            guard let mac else {
                folders = []
                loading = false
                return
            }
            loading = true
            let shared = await viewModel.folders(on: mac)
            guard !Task.isCancelled else { return }
            folders = shared
            loading = false
        }
    }
}

/// A folder on an SFTP server, typed or browsed to (PLAN.md §4.3).
private struct ServerFolderPicker: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var viewModel: PairEditorViewModel
    let side: PairFolderSide

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Folder on an SFTP Server").font(.headline)
            HStack {
                TextField("sftp://user@host/path", text: $viewModel.serverLocator)
                    .onSubmit { Task { await viewModel.browseServer() } }
                Button("Browse") { Task { await viewModel.browseServer() } }
                    .disabled(viewModel.isBrowsingServer)
            }
            List {
                if let parent = viewModel.serverListing?.parent {
                    Button("Enclosing Folder") { Task { await viewModel.openServerFolder(parent) } }
                }
                ForEach(viewModel.serverListing?.folders ?? []) { folder in
                    Button(folder.name) { Task { await viewModel.openServerFolder(folder.locator) } }
                }
            }
            .overlay {
                if viewModel.isBrowsingServer {
                    ProgressView()
                } else if viewModel.serverListing?.folders.isEmpty == true {
                    Text("No folders inside this one.").foregroundStyle(.secondary)
                }
            }
            if let error = viewModel.serverBrowseError {
                Text(error).foregroundStyle(.red).textSelection(.enabled)
            }
            Text("Foldover connects with your SSH keys, agent and ~/.ssh/config, like the ssh command; it cannot ask for a password. Use ~ for the home folder, for example sftp://nas.local/~/Photos. Use only trusted servers: changes on the server can redirect Foldover's access outside the folder you choose.")
                .font(.callout).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                Button("Use This Folder") {
                    if viewModel.useServerFolder(side: side) { dismiss() }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(viewModel.serverLocator.isEmpty)
            }
        }
        .padding()
        .frame(width: 480, height: 440)
    }
}

extension PairFolderSide: Identifiable {
    var id: Self { self }
}
