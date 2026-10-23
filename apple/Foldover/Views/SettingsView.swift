import SwiftUI

/// The Settings window (⌘,).
struct SettingsView: View {
    let viewModel: SettingsViewModel
    let macToMac: MacToMacViewModel
    let automation: AutomationStore

    var body: some View {
        TabView {
            Tab("General", systemImage: "gearshape") { general.settingsTabFrame() }
            Tab("Mac-to-Mac", systemImage: "laptopcomputer") {
                MacToMacView(viewModel: macToMac).settingsTabFrame()
            }
        }
    }

    private var general: some View {
        Form {
            Section {
                NoteToggle(
                    title: Text("Open Foldover at login"),
                    note: Text(viewModel.loginNote),
                    isOn: Binding(
                        get: { viewModel.opensAtLogin },
                        set: { opens in viewModel.setLoginLaunch(opens ? .open : .stayClosed) }
                    )
                )
                if let error = viewModel.error {
                    ErrorNote(message: error)
                }
            }
            Section("Automatic Syncs") {
                NoteToggle(
                    title: Text("Sync automatically in Low Power Mode"),
                    note: Text("When off, automatic syncs wait until Low Power Mode ends. Syncs over the network always wait while the network is expensive or Low Data Mode is on."),
                    isOn: Binding(
                        get: { automation.lowPower == .run },
                        set: { run in Task { await automation.setLowPower(run ? .run : .pause) } }
                    )
                )
                if let error = automation.error {
                    ErrorNote(message: error)
                }
            }
            Section {
                Button("Open Full Disk Access Settings…") { viewModel.openFullDiskAccess() }
            } header: {
                Text("Folder Access")
            } footer: {
                Text(viewModel.accessNote)
            }
        }
        .formStyle(.grouped)
        .onAppear {
            viewModel.refresh()
            Task { await automation.refresh() }
        }
    }
}

/// A switch with its explanation under the title, as System Settings
/// lays one out.
struct NoteToggle: View {
    let title: Text
    let note: Text
    @Binding var isOn: Bool

    var body: some View {
        Toggle(isOn: $isOn) {
            VStack(alignment: .leading, spacing: 2) {
                title
                note
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
    }
}

/// A failure shown in a settings form.
struct ErrorNote: View {
    let message: String

    var body: some View {
        Label(message, systemImage: "exclamationmark.octagon")
            .font(.callout)
            .foregroundStyle(.red)
            .textSelection(.enabled)
    }
}

private extension View {
    /// Every settings tab shares one width. The height is bounded so a
    /// long tab scrolls instead of growing past the screen.
    func settingsTabFrame() -> some View {
        frame(width: 520)
            .frame(minHeight: 280, idealHeight: 440, maxHeight: 620)
    }
}
