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
                if let note = viewModel.loginApprovalNote {
                    Label(note, systemImage: "exclamationmark.triangle.fill")
                        .font(.callout)
                        .foregroundStyle(.orange)
                }
                if let error = viewModel.error {
                    ErrorNote(message: error)
                }
                NoteToggle(
                    title: Text("Play paper sounds"),
                    note: Text(viewModel.paperSoundsNote),
                    isOn: Binding(
                        get: { viewModel.playsPaperSounds },
                        set: { viewModel.setPaperSounds($0) }
                    )
                )
            }
            Section("Automatic Syncs") {
                NoteToggle(
                    title: Text("Sync automatically in Low Power Mode"),
                    note: Text("When off, automatic syncs wait until Low Power Mode ends to conserve battery. Turn this on if keeping folders current matters more than saving power.\n\nSyncs over the network always wait while the network is expensive or Low Data Mode is on. This setting controls automatic runs; you can still start a sync yourself."),
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
                Text(viewModel.accessRecoveryNote).font(.callout).foregroundStyle(.secondary)
                FullDiskAccessResetButton(viewModel: viewModel)
                if let error = viewModel.accessError { ErrorNote(message: error) }
            } header: {
                InfoLabel(title: Text("Folder Access"), message: Text(viewModel.accessNote))
            }
        }
        .formStyle(.grouped)
        .onAppear {
            viewModel.refresh()
            Task { await automation.refresh() }
        }
    }
}

/// A switch with optional guidance beside its title.
struct NoteToggle: View {
    let title: Text
    let note: Text
    @Binding var isOn: Bool

    var body: some View {
        HStack {
            Toggle(isOn: $isOn) { title }
                .accessibilityLabel(title)
                .accessibilityHint(note)
            InfoButton(title: title, message: note)
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
