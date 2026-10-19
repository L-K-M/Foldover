import SwiftUI

/// The Settings window (⌘,).
struct SettingsView: View {
    let viewModel: SettingsViewModel
    let macToMac: MacToMacViewModel

    var body: some View {
        TabView {
            Tab("General", systemImage: "gearshape") { general }
            Tab("Mac-to-Mac", systemImage: "laptopcomputer") { MacToMacView(viewModel: macToMac) }
        }
        .frame(width: 460)
    }

    private var general: some View {
        Form {
            Toggle("Open Foldover at login", isOn: Binding(
                get: { viewModel.opensAtLogin },
                set: { opens in viewModel.setLoginLaunch(opens ? .open : .stayClosed) }
            ))
            Text(viewModel.loginNote).font(.callout).foregroundStyle(.secondary)
            if let error = viewModel.error {
                Text(error).foregroundStyle(.red).textSelection(.enabled)
            }
            Section("Folder Access") {
                Text(viewModel.accessNote).font(.callout).foregroundStyle(.secondary)
                Button("Open Full Disk Access Settings") { viewModel.openFullDiskAccess() }
            }
        }
        .formStyle(.grouped)
        .onAppear { viewModel.refresh() }
    }
}
