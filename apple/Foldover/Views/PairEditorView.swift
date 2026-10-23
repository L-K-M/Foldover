import SwiftUI
import UniformTypeIdentifiers

/// The sheet that creates or edits a pair. The form scrolls; the plan
/// sentence and the Cancel and Save buttons stay in view below it.
struct PairEditorView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL
    @Bindable var viewModel: PairEditorViewModel
    @State private var choosingSide = PairFolderSide.left
    @State private var choosingFolder = false
    @State private var enteringServer: PairFolderSide?
    @State private var choosingOtherMac: PairFolderSide?
    /// The folder row a drag is over, highlighted to take the drop.
    @State private var dropTarget: PairFolderSide?

    var body: some View {
        VStack(spacing: 0) {
            header
            Form {
                nameSection
                foldersSection
                syncSection
                skipSection
                metadataSection
                advancedSection
            }
            .formStyle(.grouped)
            .disabled(viewModel.isSaving)
            Divider()
            footer
        }
        .frame(minWidth: 800, idealWidth: 920, minHeight: 380, idealHeight: 540, maxHeight: 820)
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

    private var header: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(viewModel.title)
                .font(.title2)
                .accessibilityAddTraits(.isHeader)
            Text("Saving configures the pair. Nothing is copied until you preview and sync.")
                .font(.callout)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 20)
        .padding(.top, 20)
        .padding(.bottom, 4)
    }

    private var nameSection: some View {
        Section {
            TextField("Name", text: $viewModel.name)
        }
    }

    // MARK: Folders

    private var foldersSection: some View {
        Section {
            HStack(spacing: 16) {
                folderCard(.left)
                modePicker
                folderCard(.right)
            }
            Text(viewModel.mode.explanation)
                .font(.callout)
                .foregroundStyle(.secondary)
            if let warning = viewModel.serverWarning {
                WarningLabel(text: warning)
            }
            ForEach(viewModel.readinessWarnings, id: \.self) { warning in
                WarningLabel(text: warning)
            }
        } header: {
            Text("Folders")
        } footer: {
            if !viewModel.nearbyServers.isEmpty {
                Text("Connect to a nearby server from the Choose menu, open a shared folder in Finder, then pick it here.")
            }
        }
    }

    private func folderName(_ side: PairFolderSide) -> String {
        PairSyncViewModel.folderName(side == .left ? .a : .b)
    }

    /// Each card chooses or accepts a dropped folder for its side.
    private func folderCard(_ side: PairFolderSide) -> some View {
        let name = folderName(side)
        let path = viewModel.folderText(side)
        return VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 6) {
                Text(name)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Label(path, systemImage: viewModel.folderSymbol(side))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
            }
            .accessibilityElement(children: .combine)
            .help(path)
            chooseMenu(side, folderName: name)
        }
        .frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(.quaternary, in: .rect(cornerRadius: 6))
        .overlay {
            if dropTarget == side {
                RoundedRectangle(cornerRadius: 6)
                    .strokeBorder(Color.accentColor, lineWidth: 2)
                    .background(Color.accentColor.opacity(0.1), in: RoundedRectangle(cornerRadius: 6))
            }
        }
        .dropDestination(for: URL.self) { urls, _ in
            viewModel.chooseDropped(urls, side: side)
        } isTargeted: { targeted in
            // A drag leaving one card must not clear the other's highlight.
            if targeted {
                dropTarget = side
            } else if dropTarget == side {
                dropTarget = nil
            }
        }
    }

    private func chooseMenu(_ side: PairFolderSide, folderName: String) -> some View {
        Menu("Choose…") {
            Button("Folder on This Mac…") {
                choosingSide = side
                choosingFolder = true
            }
            Button("Folder on an SFTP Server…") { enteringServer = side }
            if viewModel.offersOtherMacs {
                Button("Folder on Another Mac…") { choosingOtherMac = side }
            }
            if !viewModel.nearbyServers.isEmpty {
                Section("Connect to a Nearby Server") {
                    ForEach(viewModel.nearbyServers) { server in
                        Button(server.name) {
                            if let url = viewModel.serverURL(server) { openURL(url) }
                        }
                    }
                }
            }
        }
        .fixedSize()
        .help("Choose a folder, or drop one here.")
        .accessibilityLabel(String(localized: "Choose \(folderName)"))
    }

    // MARK: Sync

    /// Keep direction between the folders, matching the selected mode.
    private var modePicker: some View {
        VStack(spacing: 8) {
            Image(systemName: viewModel.mode.symbol)
                .font(.title2)
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            Picker("Mode", selection: $viewModel.mode) {
                ForEach(PairMode.allCases, id: \.self) { mode in
                    Text(mode.title).tag(mode)
                }
            }
            .pickerStyle(.menu)
            .labelsHidden()
            .accessibilityIdentifier("pair-mode")
        }
        .fixedSize()
    }

    private var syncSection: some View {
        Section {
            if let warning = viewModel.recordsWarning {
                WarningLabel(text: warning)
            }
            if viewModel.offersKeepingNothing {
                Toggle(isOn: $viewModel.keepsNothingInTarget) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(viewModel.keepingNothingTitle)
                        Text(viewModel.keepingNothingExplanation)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            Picker("Sync Automatically", selection: $viewModel.options.schedule) {
                ForEach(viewModel.scheduleChoices, id: \.self) { schedule in
                    Text(PairEditorViewModel.scheduleTitle(schedule)).tag(schedule)
                }
            }
            .pickerStyle(.menu)
            if viewModel.showsDailySettings {
                DatePicker("At", selection: $viewModel.dailyTime, displayedComponents: .hourAndMinute)
                LabeledContent("On") { dayToggles }
                if let warning = viewModel.dailyWarning {
                    WarningLabel(text: warning)
                }
            }
        } header: {
            Text("Sync")
        } footer: {
            VStack(alignment: .leading, spacing: 4) {
                if let note = viewModel.scheduleNote {
                    Text(note)
                }
                Text("Automatic syncs start after the first sync you run yourself. They stop instead of asking when they would archive many items.")
            }
        }
    }

    private var dayToggles: some View {
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

    // MARK: Skip

    private var skipSection: some View {
        Section {
            VStack(alignment: .leading, spacing: 6) {
                TextEditor(text: $viewModel.filterText)
                    .font(.body.monospaced())
                    .frame(minHeight: 72)
                    .accessibilityLabel("Rules for files to skip")
                Text("One rule per line, like .gitignore: *.tmp skips temporary files, build/ skips folders named build. Rules apply to both folders; a .foldoverignore file in either folder adds more.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            LimitRow(
                title: "Skip files larger than",
                note: "A file this large in either folder is skipped in both, so it is never copied or deleted.",
                isOn: $viewModel.limitsSize,
                value: $viewModel.sizeLimitMegabytes,
                range: 1...1_000_000_000,
                fieldTitle: "Larger than (MB)",
                unit: "MB"
            )
            LimitRow(
                title: "Skip files unchanged for more than",
                note: "Skipped only while unchanged in both folders; editing one makes it sync again.",
                isOn: $viewModel.limitsAge,
                value: $viewModel.ageLimitDays,
                // The view model clamps to 36,500 days (100 years).
                range: 1...36_500,
                fieldTitle: "Unchanged for more than (days)",
                unit: "days"
            )
            TextField("Canary file", text: $viewModel.canary)
            if let warning = viewModel.canaryWarning {
                WarningLabel(text: warning)
            }
        } header: {
            Text("Skip")
        } footer: {
            Text("A file that must stay in both folders unchanged. If it goes missing or changes, syncs stop before writing anything.")
        }
    }

    // MARK: Metadata and advanced

    private var metadataSection: some View {
        Section {
            Toggle("Modification dates", isOn: $viewModel.options.syncModificationTimes)
            Toggle("Tags, Finder info and other extended attributes",
                   isOn: $viewModel.options.syncExtendedAttributes)
            Toggle("Permissions and access lists", isOn: $viewModel.options.syncPermissions)
            if viewModel.resetsSyncHistory {
                WarningLabel(text: String(localized: "Changing metadata settings resets this pair's sync history. The next preview compares both folders from scratch."))
            }
        } header: {
            Text("Metadata")
        } footer: {
            Text(viewModel.metadataNote)
        }
    }

    private var advancedSection: some View {
        Section("Advanced") {
            LimitRow(
                title: "Limit bandwidth",
                note: "All of this pair's copies together stay under this rate.",
                isOn: $viewModel.limitsBandwidth,
                value: $viewModel.bandwidthLimitMegabytesPerSecond,
                range: 1...1_000_000,
                fieldTitle: "At most (MB/s)",
                unit: "MB/s"
            )
            Picker("Write Safety", selection: $viewModel.options.durability) {
                ForEach(viewModel.durabilityChoices, id: \.self) { durability in
                    Text(durability.title).tag(durability)
                        .disabled(viewModel.usesServerFolder && durability == .full)
                }
            }
            .pickerStyle(.menu)
            if let warning = viewModel.durabilityWarning {
                WarningLabel(text: warning)
            }
            Text(viewModel.durabilityExplanation)
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: Footer

    private var footer: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let message = viewModel.errorMessage {
                Text(message)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
                    .accessibilityLabel(String(localized: "Settings were not confirmed: \(message)"))
            }
            HStack(spacing: 12) {
                // The plan sentence rewrites itself as settings change (PLAN.md §3.8).
                Text(viewModel.planText)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .help(viewModel.planText)
                if viewModel.isSaving {
                    ProgressView().controlSize(.small).accessibilityLabel("Saving pair")
                }
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(viewModel.isSaving)
                Button("Save Pair") {
                    Task { if await viewModel.save() { dismiss() } }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!viewModel.canSave)
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
    }
}

/// A caution or a blocking problem, shown beside the setting behind it.
private struct WarningLabel: View {
    let text: String

    var body: some View {
        Label(text, systemImage: "exclamationmark.triangle.fill")
            .font(.callout)
            .foregroundStyle(.orange)
            .textSelection(.enabled)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// A switch for an optional limit with its note beneath, and the limit's
/// number beside it, which stays in place but disabled while the limit
/// is off.
private struct LimitRow: View {
    let title: LocalizedStringKey
    let note: LocalizedStringKey
    @Binding var isOn: Bool
    @Binding var value: Int
    let range: ClosedRange<Int>
    let fieldTitle: LocalizedStringKey
    let unit: LocalizedStringKey

    var body: some View {
        HStack(spacing: 12) {
            Toggle(isOn: $isOn) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                    Text(note)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            NumberField(title: fieldTitle, value: $value, range: range, unit: unit, isActive: isOn)
        }
    }
}

/// A whole number typed or stepped, with its unit after it.
private struct NumberField: View {
    let title: LocalizedStringKey
    @Binding var value: Int
    let range: ClosedRange<Int>
    let unit: LocalizedStringKey
    let isActive: Bool

    var body: some View {
        HStack(spacing: 6) {
            // Empty while the limit is off, so the view model's 0 for "no
            // limit" never shows as a number. While on, the field binds the
            // number itself, so text that is no number reverts on commit.
            Group {
                if isActive {
                    TextField(title, value: $value, format: .number)
                } else {
                    TextField(title, text: .constant(""))
                }
            }
            .labelsHidden()
            .multilineTextAlignment(.trailing)
            .frame(width: 80)
            Stepper(title, value: $value, in: range)
                .labelsHidden()
            Text(unit)
                .foregroundStyle(.secondary)
        }
        .disabled(!isActive)
    }
}

/// A folder a paired Mac shares, for one side of a pair.
private struct OtherMacFolderPicker: View {
    @Environment(\.dismiss) private var dismiss
    let viewModel: PairEditorViewModel
    let side: PairFolderSide
    @State private var mac: PairedDevice?
    @State private var folders: [OfferedShare] = []
    @State private var selection: OfferedShare.ID?
    @State private var loading = false

    var body: some View {
        VStack(spacing: 0) {
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
                    List(folders, selection: $selection) { folder in
                        Label(folder.name, systemImage: "folder")
                    }
                    // Double-clicking a folder chooses it.
                    .contextMenu(forSelectionType: OfferedShare.ID.self) { _ in
                        EmptyView()
                    } primaryAction: { ids in
                        guard let id = ids.first else { return }
                        choose(id)
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
            }
            .padding(16)
            Divider()
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                Button("Use This Folder") {
                    if let selection { choose(selection) }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(selection == nil)
            }
            .padding(16)
        }
        .frame(width: 420, height: 380)
        // A new selection cancels the last one's request, so a slow Mac's
        // folders never show under another Mac.
        .task(id: mac) {
            selection = nil
            folders = []
            guard let mac else {
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

    private func choose(_ id: OfferedShare.ID) {
        guard let mac, let folder = folders.first(where: { $0.id == id }) else { return }
        viewModel.chooseShared(folder, on: mac, side: side)
        dismiss()
    }
}

/// A folder on an SFTP server, typed or browsed to (PLAN.md §4.3).
private struct ServerFolderPicker: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var viewModel: PairEditorViewModel
    let side: PairFolderSide
    /// The listed folder a click highlighted; a double-click opens it.
    @State private var selection: String?

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 12) {
                Text("Folder on an SFTP Server").font(.headline)
                HStack {
                    TextField("sftp://user@host/path", text: $viewModel.serverLocator)
                        .onSubmit { Task { await viewModel.browseServer() } }
                    Button("Browse") { Task { await viewModel.browseServer() } }
                        .disabled(viewModel.isBrowsingServer)
                }
                List(selection: $selection) {
                    if let parent = viewModel.serverListing?.parent {
                        Label("Enclosing Folder", systemImage: "arrow.turn.up.left")
                            .tag(parent)
                    }
                    ForEach(viewModel.serverListing?.folders ?? []) { folder in
                        Label(folder.name, systemImage: "folder")
                    }
                }
                // Rows are tagged with their locators, so opening one is
                // the same call for a folder and for the enclosing folder.
                .contextMenu(forSelectionType: String.self) { _ in
                    EmptyView()
                } primaryAction: { locators in
                    guard let locator = locators.first else { return }
                    selection = nil
                    Task { await viewModel.openServerFolder(locator) }
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
            }
            .padding(16)
            Divider()
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                Button("Use This Folder") {
                    // Like an open panel: a highlighted listed folder wins
                    // over the folder being browsed.
                    if let selection,
                       viewModel.serverListing?.folders.contains(where: { $0.locator == selection }) == true {
                        viewModel.serverLocator = selection
                    }
                    if viewModel.useServerFolder(side: side) { dismiss() }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(viewModel.serverLocator.isEmpty)
            }
            .padding(16)
        }
        .frame(width: 500, height: 520)
    }
}

extension PairFolderSide: Identifiable {
    var id: Self { self }
}
