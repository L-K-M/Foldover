import SwiftUI
import UniformTypeIdentifiers

/// Pairing with other Macs, in Settings.
struct MacToMacView: View {
    let viewModel: MacToMacViewModel
    @State private var forgetting: MacToMacViewModel.PairedRow?
    /// A share about to stop, and the Mac it was shared with.
    @State private var stopping: (share: MacToMacViewModel.ShareRow, mac: String)?
    /// The Mac a folder is being chosen for; kept past the panel closing.
    @State private var sharingWith: MacToMacViewModel.PairedRow?
    @State private var choosingShare = false

    var body: some View {
        Form {
            if !viewModel.isOn {
                Section {
                    Button("Turn On Mac-to-Mac") { Task { await viewModel.turnOn() } }
                } header: {
                    InfoLabel(title: Text("Mac-to-Mac"), message: Text(viewModel.intro))
                }
            } else {
                Section {
                    if let thisMac = viewModel.thisMac {
                        Label(thisMac, systemImage: "laptopcomputer")
                    }
                    NoteToggle(
                        title: Text("Allow pairing"),
                        note: Text("\(viewModel.pairingNote)\n\nOpen pairing only while adding a Mac you trust. Compare the code on both screens before confirming. Turning off Allow pairing does not disconnect Macs you already paired."),
                        isOn: Binding(
                            get: { viewModel.pairingOpen },
                            set: { open in Task { await viewModel.setPairing(open ? .open : .closed) } }
                        )
                    )
                    if viewModel.pairingOpen {
                        Text(viewModel.pairingNote).font(.callout).foregroundStyle(.secondary)
                    }
                }
                ForEach(viewModel.codes) { row in
                    Section(row.title) {
                        Text(row.code)
                            .font(.system(.largeTitle, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity)
                            .accessibilityLabel(row.spoken)
                        Text(row.note).font(.callout).foregroundStyle(.secondary)
                        HStack {
                            Spacer()
                            Button("Cancel", role: .cancel) { Task { await viewModel.cancel(row) } }
                            Button("Confirm") { Task { await viewModel.confirm(row) } }
                                .keyboardShortcut(.defaultAction)
                        }
                    }
                }
                Section {
                    ForEach(viewModel.nearby) { row in
                        MacRow(symbol: "laptopcomputer", name: row.name, detail: row.detail) {
                            if row.canPair {
                                Button("Pair") { Task { await viewModel.pair(row) } }
                            }
                        }
                    }
                } header: {
                    Text("Macs on This Network")
                } footer: {
                    if let note = viewModel.nearbyNote {
                        Text(note)
                    }
                }
            }
            if !viewModel.paired.isEmpty {
                Section {
                    ForEach(viewModel.paired) { row in
                        MacRow(symbol: "laptopcomputer", name: row.name, detail: row.detail) {
                            Button("Share a Folder…") {
                                sharingWith = row
                                choosingShare = true
                            }
                            Button("Forget…") { forgetting = row }
                        }
                        ForEach(row.shares) { share in
                            MacRow(symbol: "folder", name: share.name, detail: share.path) {
                                Button("Stop Sharing…") { stopping = (share, row.name) }
                            }
                            .padding(.leading, 24)
                        }
                    }
                } header: {
                    InfoLabel(
                        title: Text("Paired Macs"),
                        message: Text("\(viewModel.sharingNote)\n\nShare only folders you want that Mac to read and change through its sync pairs. Sharing makes a folder available; it does not start a sync. Stop Sharing revokes that folder's access, while Forget removes the pairing.")
                    )
                }
            }
            if let error = viewModel.error {
                Section {
                    ErrorNote(message: error)
                }
            }
        }
        .formStyle(.grouped)
        .task { await viewModel.monitor() }
        .fileImporter(isPresented: $choosingShare, allowedContentTypes: [.folder],
                      allowsMultipleSelection: false) { result in
            guard let row = sharingWith, let url = try? result.get().first else { return }
            Task { await viewModel.share(url, with: row) }
        }
        .confirmationDialog(
            "Stop sharing “\(stopping?.share.name ?? "")” with “\(stopping?.mac ?? "")”?",
            isPresented: Binding(get: { stopping != nil }, set: { if !$0 { stopping = nil } }),
            presenting: stopping
        ) { stopping in
            Button("Stop Sharing", role: .destructive) {
                Task { await viewModel.stopSharing(stopping.share) }
            }
        } message: { stopping in
            Text("Pairs on “\(stopping.mac)” that use this folder stop syncing until you share it again.")
        }
        .confirmationDialog(
            "Forget “\(forgetting?.name ?? "")”?",
            isPresented: Binding(get: { forgetting != nil }, set: { if !$0 { forgetting = nil } }),
            presenting: forgetting
        ) { row in
            Button("Forget", role: .destructive) { Task { await viewModel.forget(row) } }
        } message: { _ in
            Text("That Mac can no longer connect to this one until you pair them again.")
        }
    }
}

/// A Mac or a shared folder with its detail line, and the controls
/// that act on it.
private struct MacRow<Controls: View>: View {
    let symbol: String
    let name: String
    let detail: String
    let controls: Controls

    init(symbol: String, name: String, detail: String, @ViewBuilder controls: () -> Controls) {
        self.symbol = symbol
        self.name = name
        self.detail = detail
        self.controls = controls()
    }

    var body: some View {
        HStack(spacing: 12) {
            Label {
                VStack(alignment: .leading, spacing: 2) {
                    Text(name)
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            } icon: {
                Image(systemName: symbol)
                    .accessibilityHidden(true)
            }
            .accessibilityElement(children: .combine)
            Spacer(minLength: 8)
            controls
        }
    }
}
