import AppKit

/// What a release check learned, reduced to what a person is told.
public enum UpdateOutcome: Equatable {
    /// A newer release exists; its message carries the download link.
    case available(AppRelease)
    /// The running build is the newest release.
    case upToDate
    /// The check could not reach or read the release feed.
    case failed
}

/// Reports an update check's outcome in a panel (PLAN.md §9, ADR 0045).
/// A panel, not a notification: the answer carries the download link a
/// notification cannot. Automatic checks send only `.available`; the
/// other outcomes answer the menu item's check.
@MainActor
public enum UpdatePrompt {
    public static func present(_ outcome: UpdateOutcome, installed: String) {
        let alert = NSAlert()
        switch outcome {
        case .available(let release):
            alert.messageText = String(localized: "Foldover \(release.version) is available")
            alert.informativeText = String(localized: "This copy runs Foldover \(installed). Download the new version and replace the app to update.")
            alert.addButton(withTitle: String(localized: "Download"))
            alert.addButton(withTitle: String(localized: "Later"))
            if alert.runModal() == .alertFirstButtonReturn {
                NSWorkspace.shared.open(release.downloadURL)
            }
        case .upToDate:
            alert.messageText = String(localized: "Foldover is up to date")
            alert.informativeText = String(localized: "Foldover \(installed) is the newest release.")
            alert.runModal()
        case .failed:
            alert.messageText = String(localized: "The update check failed")
            alert.informativeText = String(localized: "Foldover could not reach GitHub. Check the connection and try again.")
            alert.alertStyle = .warning
            alert.runModal()
        }
    }
}
