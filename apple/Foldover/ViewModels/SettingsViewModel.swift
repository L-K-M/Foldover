import Foundation

/// App settings: strings and actions only.
@MainActor
struct SettingsViewModel {
    let store: SettingsStore

    var opensAtLogin: Bool { store.loginStatus != .disabled }

    var loginNote: String {
        String(localized: "Syncing runs in the background agent whether or not the app is open. Opening the app at login keeps the menu bar icon and notifications available.\n\nTurn this off if you prefer to open the interface yourself; it does not turn off the background agent or your pairs' schedules.")
    }

    var loginApprovalNote: String? {
        guard store.loginStatus == .requiresApproval else { return nil }
        return String(localized: "Allow Foldover in System Settings › General › Login Items to finish.")
    }

    var error: String? { store.error }
    var needsAccessReminder: Bool { store.fullDiskAccessReminderNeeded }
    var resettingAccess: Bool { store.resettingFullDiskAccess }
    var mayResetAccess: Bool { store.mayResetFullDiskAccess }
    var accessError: String? { store.fullDiskAccessError }

    var accessRecoveryNote: String {
        String(localized: "macOS may forget Full Disk Access after an update. Check that Foldover is enabled in System Settings. If access still fails, reset Foldover's access, remove any old Foldover entry, and add the current app from Applications again.\n\nResetting removes Foldover's existing grant. Finish or stop any sync before resetting, then grant access again in Settings. Foldover cannot verify the grant automatically.")
    }

    func dismissAccessReminder() { store.dismissFullDiskAccessReminder() }
    func resetAccessAndOpenSettings() async { await store.resetFullDiskAccessAndOpenSettings() }

    var accessNote: String {
        String(localized: "If a sync says Foldover may not read a folder, for example in Documents, Desktop, iCloud Drive or on an external or network volume, add Foldover to Full Disk Access.\n\nThis grants broader access to protected data on this Mac. It does not choose folders to sync: your pairs still determine what Foldover copies. Grant it when a folder's privacy restrictions prevent the access you need.")
    }

    func openFullDiskAccess() { store.openFullDiskAccess() }

    func setLoginLaunch(_ launch: LoginLaunch) {
        store.setLoginLaunch(launch)
    }

    func refresh() { store.refresh() }
}
