import SwiftUI

/// A per-build reminder, never a claim that macOS denied or granted access.
struct FullDiskAccessReminder: View {
    let viewModel: SettingsViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Review Full Disk Access", systemImage: "folder.badge.questionmark")
                .font(.headline)
            Text(viewModel.accessRecoveryNote).font(.callout)
            HStack {
                Button("Open Settings…") { viewModel.openFullDiskAccess() }
                FullDiskAccessResetButton(viewModel: viewModel)
                Spacer()
                Button("Not Now") { viewModel.dismissAccessReminder() }
                    .disabled(viewModel.resettingAccess)
            }
            if let error = viewModel.accessError { ErrorNote(message: error) }
            if !viewModel.mayResetAccess, !viewModel.resettingAccess {
                Text("Connect to the background agent and wait for all syncs or previews to finish before resetting access.")
                    .font(.callout).foregroundStyle(.secondary)
            }
        }
        .padding()
        .background(.quaternary)
        .accessibilityIdentifier("full-disk-access-reminder")
    }
}

struct FullDiskAccessResetButton: View {
    let viewModel: SettingsViewModel

    var body: some View {
        Button(viewModel.resettingAccess ? "Resetting Access…" : "Reset and Open Settings…") {
            Task { await viewModel.resetAccessAndOpenSettings() }
        }
        .disabled(!viewModel.mayResetAccess)
        .help("Resets only Foldover's Full Disk Access. Finish or stop any sync first, then grant access again in Settings.")
    }
}
