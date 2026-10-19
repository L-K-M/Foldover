import FoldoverSystem
import Foundation
import Observation

enum LoginLaunch: Sendable {
    case open
    case stayClosed
}

enum LoginStatus: Sendable {
    case enabled
    case disabled
    /// Registered, but the user must allow it in System Settings.
    case requiresApproval
}

/// Whether the app opens at login; the system's login items are the
/// source of truth.
@MainActor
protocol LoginItemControl {
    var status: LoginStatus { get }
    func enable() throws
    func disable() throws
}

@MainActor
private final class SystemLoginItem: LoginItemControl {
    private let item = LoginItem()

    var status: LoginStatus {
        switch item.status {
        case .enabled: .enabled
        case .disabled: .disabled
        case .requiresApproval: .requiresApproval
        }
    }

    func enable() throws { try item.enable() }
    func disable() throws { try item.disable() }
}

@Observable
@MainActor
final class SettingsStore {
    private let loginItem: any LoginItemControl
    private let showFullDiskAccess: @MainActor () -> Void
    private(set) var loginStatus: LoginStatus
    private(set) var error: String?

    init(loginItem: any LoginItemControl = SystemLoginItem(),
         showFullDiskAccess: @escaping @MainActor () -> Void = { PrivacySettings.openFullDiskAccess() }) {
        self.loginItem = loginItem
        self.showFullDiskAccess = showFullDiskAccess
        loginStatus = loginItem.status
    }

    /// System Settings' Full Disk Access list, for folders macOS protects.
    func openFullDiskAccess() {
        showFullDiskAccess()
    }

    func setLoginLaunch(_ launch: LoginLaunch) {
        error = nil
        do {
            switch launch {
            case .open: try loginItem.enable()
            case .stayClosed: try loginItem.disable()
            }
        } catch {
            self.error = error.localizedDescription
        }
        loginStatus = loginItem.status
    }

    /// Re-read after the user may have changed it in System Settings.
    func refresh() {
        loginStatus = loginItem.status
    }
}
