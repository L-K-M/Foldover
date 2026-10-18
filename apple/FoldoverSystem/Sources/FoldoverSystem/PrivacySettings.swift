import AppKit

/// System Settings panes Foldover sends people to.
@MainActor
public enum PrivacySettings {
    private static let fullDiskAccess =
        URL(string: "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles")

    /// The Full Disk Access list, where Foldover can be allowed to read
    /// protected folders and external or network volumes.
    public static func openFullDiskAccess() {
        guard let fullDiskAccess else { return }
        NSWorkspace.shared.open(fullDiskAccess)
    }
}
