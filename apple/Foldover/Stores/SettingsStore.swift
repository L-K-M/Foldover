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
    private let resetFullDiskAccess: @MainActor () async throws -> Void
    private let appBuild: Data?
    private let defaults: UserDefaults
    private let canResetFullDiskAccess: @MainActor () -> Bool
    /// Where the fold's sound switch lives: the main window's sound store
    /// reads the same key.
    private let sounds: FoldSoundStore
    private static let reviewedBuildKey = "fullDiskAccessReviewedBuild"
    private(set) var loginStatus: LoginStatus
    private(set) var error: String?
    private(set) var fullDiskAccessReminderNeeded = false
    private(set) var resettingFullDiskAccess = false
    private(set) var fullDiskAccessError: String?

    var mayResetFullDiskAccess: Bool { !resettingFullDiskAccess && canResetFullDiskAccess() }

    init(loginItem: any LoginItemControl = SystemLoginItem(),
         showFullDiskAccess: @escaping @MainActor () -> Void = { PrivacySettings.openFullDiskAccess() },
         resetFullDiskAccess: @escaping @MainActor () async throws -> Void = {
             try await PrivacySettings.resetFullDiskAccess()
         },
         appBuild: Data? = BackgroundAgent().appBuild,
         defaults: UserDefaults = .standard,
         canResetFullDiskAccess: @escaping @MainActor () -> Bool = { true },
         sounds: FoldSoundStore? = nil) {
        self.loginItem = loginItem
        self.showFullDiskAccess = showFullDiskAccess
        self.resetFullDiskAccess = resetFullDiskAccess
        self.appBuild = appBuild
        self.defaults = defaults
        self.canResetFullDiskAccess = canResetFullDiskAccess
        // The sounds' preference lives with the other settings' defaults.
        let sounds = sounds ?? FoldSoundStore(defaults: defaults)
        self.sounds = sounds
        loginStatus = loginItem.status
        paperSounds = sounds.isEnabled
    }

    /// Whether folding, opening and turning lines make paper sounds.
    private(set) var paperSounds = true

    func setPaperSounds(_ on: Bool) {
        sounds.isEnabled = on
        paperSounds = on
    }

    /// macOS has no supported Full Disk Access status API. A changed signed
    /// build needs a reminder, not an inferred denial or an automatic reset.
    func reviewFullDiskAccessAtLaunch() {
        guard let appBuild else { return }
        fullDiskAccessReminderNeeded = defaults.data(forKey: Self.reviewedBuildKey) != appBuild
    }

    func dismissFullDiskAccessReminder() {
        if let appBuild { defaults.set(appBuild, forKey: Self.reviewedBuildKey) }
        fullDiskAccessReminderNeeded = false
    }

    func resetFullDiskAccessAndOpenSettings() async {
        guard !resettingFullDiskAccess else { return }
        fullDiskAccessError = nil
        guard canResetFullDiskAccess() else {
            fullDiskAccessError = String(localized: "Connect to the background agent and wait for all syncs or previews to finish before resetting access.")
            return
        }
        resettingFullDiskAccess = true
        defer { resettingFullDiskAccess = false }
        do {
            try await resetFullDiskAccess()
            // Opening Settings cannot tell us whether the user grants access.
            defaults.removeObject(forKey: Self.reviewedBuildKey)
            fullDiskAccessReminderNeeded = true
            showFullDiskAccess()
        } catch {
            fullDiskAccessError = error.localizedDescription
        }
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
