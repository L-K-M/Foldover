import Foundation

/// App settings: strings and actions only.
@MainActor
struct SettingsViewModel {
    let store: SettingsStore

    var opensAtLogin: Bool { store.loginStatus != .disabled }

    var loginNote: String {
        switch store.loginStatus {
        case .requiresApproval:
            String(localized: "Allow Foldover in System Settings › General › Login Items to finish.")
        case .enabled, .disabled:
            String(localized: "Syncing runs in the background agent whether or not the app is open. Opening the app at login keeps the menu bar icon and notifications available.")
        }
    }

    var error: String? { store.error }

    func setLoginLaunch(_ launch: LoginLaunch) {
        store.setLoginLaunch(launch)
    }

    func refresh() { store.refresh() }
}
