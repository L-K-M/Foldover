import ServiceManagement

public enum LoginItemStatus: Sendable {
    case enabled
    case disabled
    /// Registered, but the user must allow it in System Settings.
    case requiresApproval
}

/// Opening the app itself at login, so the menu bar extra and
/// notifications are there without launching it by hand. The background
/// agent syncs either way.
@MainActor
public final class LoginItem {
    public init() {}

    public var status: LoginItemStatus {
        switch SMAppService.mainApp.status {
        case .enabled: .enabled
        case .requiresApproval: .requiresApproval
        case .notRegistered, .notFound: .disabled
        @unknown default: .disabled
        }
    }

    public func enable() throws {
        try SMAppService.mainApp.register()
    }

    public func disable() throws {
        try SMAppService.mainApp.unregister()
    }
}
