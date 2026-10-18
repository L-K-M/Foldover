import SwiftUI

/// The Settings window (⌘,).
struct SettingsView: View {
    let viewModel: SettingsViewModel

    var body: some View {
        Form {
            Toggle("Open Foldover at login", isOn: Binding(
                get: { viewModel.opensAtLogin },
                set: { opens in viewModel.setLoginLaunch(opens ? .open : .stayClosed) }
            ))
            Text(viewModel.loginNote).font(.callout).foregroundStyle(.secondary)
            if let error = viewModel.error {
                Text(error).foregroundStyle(.red).textSelection(.enabled)
            }
        }
        .formStyle(.grouped)
        .frame(width: 420)
        .onAppear { viewModel.refresh() }
    }
}
