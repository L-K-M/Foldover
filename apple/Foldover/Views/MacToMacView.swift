import SwiftUI

/// Pairing with other Macs, in Settings.
struct MacToMacView: View {
    let viewModel: MacToMacViewModel
    @State private var forgetting: MacToMacViewModel.PairedRow?

    var body: some View {
        Form {
            if !viewModel.isOn {
                Section {
                    Text(viewModel.intro).font(.callout).foregroundStyle(.secondary)
                    Button("Turn On Mac-to-Mac") { Task { await viewModel.turnOn() } }
                }
            } else {
                Section {
                    if let thisMac = viewModel.thisMac {
                        Text(thisMac)
                    }
                    Toggle("Allow pairing", isOn: Binding(
                        get: { viewModel.pairingOpen },
                        set: { open in Task { await viewModel.setPairing(open ? .open : .closed) } }
                    ))
                    Text(viewModel.pairingNote).font(.callout).foregroundStyle(.secondary)
                }
                ForEach(viewModel.codes) { row in
                    Section(row.title) {
                        Text(row.code)
                            .font(.system(.largeTitle, design: .monospaced))
                            .textSelection(.enabled)
                            .accessibilityLabel(row.code.replacingOccurrences(of: " ", with: ""))
                        Text(row.note).font(.callout).foregroundStyle(.secondary)
                        HStack {
                            Button("Cancel", role: .cancel) { Task { await viewModel.cancel(row) } }
                            Button("Confirm") { Task { await viewModel.confirm(row) } }
                                .keyboardShortcut(.defaultAction)
                        }
                    }
                }
                Section("Macs on This Network") {
                    ForEach(viewModel.nearby) { row in
                        HStack {
                            VStack(alignment: .leading) {
                                Text(row.name)
                                Text(row.detail).font(.caption).foregroundStyle(.secondary)
                            }
                            .accessibilityElement(children: .combine)
                            Spacer()
                            if row.canPair {
                                Button("Pair") { Task { await viewModel.pair(row) } }
                            }
                        }
                    }
                    if let note = viewModel.nearbyNote {
                        Text(note).font(.callout).foregroundStyle(.secondary)
                    }
                }
            }
            if !viewModel.paired.isEmpty {
                Section("Paired Macs") {
                    ForEach(viewModel.paired) { row in
                        HStack {
                            VStack(alignment: .leading) {
                                Text(row.name)
                                Text(row.detail).font(.caption).foregroundStyle(.secondary)
                            }
                            .accessibilityElement(children: .combine)
                            Spacer()
                            Button("Forget…") { forgetting = row }
                        }
                    }
                }
            }
            if let error = viewModel.error {
                Text(error).foregroundStyle(.red).textSelection(.enabled)
            }
        }
        .formStyle(.grouped)
        .task { await viewModel.monitor() }
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
