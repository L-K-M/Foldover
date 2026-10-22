import AppKit

/// Finder, for showing a pair's folders and the files a sync changes.
@MainActor
public enum FinderReveal {
    /// Select `urls` in Finder windows, opening them as needed.
    public static func reveal(_ urls: [URL]) {
        NSWorkspace.shared.activateFileViewerSelecting(urls)
    }
}
