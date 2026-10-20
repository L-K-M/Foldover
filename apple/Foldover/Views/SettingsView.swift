import SwiftUI

/// The Settings window (⌘,).
struct SettingsView: View {
    let viewModel: SettingsViewModel
    let macToMac: MacToMacViewModel
    let automation: AutomationStore

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
            Section("Automatic Syncs") {
                Toggle("Sync automatically in Low Power Mode", isOn: Binding(
                    get: { automation.lowPower == .run },
                    set: { run in Task { await automation.setLowPower(run ? .run : .pause) } }
                ))
                Text("When off, automatic syncs wait until Low Power Mode ends. Syncs over the network always wait while the network is expensive or Low Data Mode is on.")
                    .font(.callout).foregroundStyle(.secondary)
            }
            Section("Folder Access") {
                Text(viewModel.accessNote).font(.callout).foregroundStyle(.secondary)
                Button("Open Full Disk Access Settings") { viewModel.openFullDiskAccess() }
            }
        }
        .formStyle(.grouped)
        .onAppear {
            viewModel.refresh()
            Task { await automation.refresh() }
        }
    }
}
