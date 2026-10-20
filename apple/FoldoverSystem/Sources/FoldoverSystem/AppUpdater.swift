import Foundation
import Sparkle

/// Updates through Sparkle 2 (PLAN.md §9). Ad-hoc code signing
/// authenticates nothing, so each update carries an EdDSA signature that
/// must match the public key built into the app. A build without that
/// key, such as a development build, cannot update itself.
@MainActor
public final class AppUpdater {
    private let controller: SPUStandardUpdaterController?

    public init() {
        let key = Bundle.main.object(forInfoDictionaryKey: "SUPublicEDKey") as? String ?? ""
        controller = key.isEmpty
            ? nil
            : SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: nil,
                                           userDriverDelegate: nil)
    }

    /// Whether this build can update itself.
    public var isAvailable: Bool { controller != nil }

    /// Look for an update now and walk the person through it.
    public func checkForUpdates() {
        controller?.checkForUpdates(nil)
    }
}
