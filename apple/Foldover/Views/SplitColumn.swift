import SwiftUI

/// Content sized to whatever its split view column gives it, reporting
/// no minimum size of its own.
///
/// AppKit asks every split view column for its size limits during each
/// constraint pass. When a column's minimum depends on its own width, as
/// it does for wrapping text or a table beside fixed-size controls, the
/// limits can change from one pass to the next, and AppKit ends the loop
/// by raising an exception that quits the app. A GeometryReader takes
/// the size it is offered and reports none of its content's, so content
/// like that sits in one, with any minimum width set outside it.
struct SplitColumn<Content: View>: View {
    private let alignment: Alignment
    private let content: Content

    init(alignment: Alignment = .center, @ViewBuilder content: () -> Content) {
        self.alignment = alignment
        self.content = content()
    }

    var body: some View {
        GeometryReader { proxy in
            content
                .frame(width: proxy.size.width, height: proxy.size.height, alignment: alignment)
        }
    }
}

// TEMPORARY, removed before merge: how the detail beside the inspector
// pane should get its minimum. A Debug build launched with
// `-foldover-layout <flag>` takes `paneColumnMin` (the minimum as a
// column width instead of a frame) or `paneNoMin` (no minimum, and the
// old 760 pt window minimum); without one, the frame and 820 pt.
enum LayoutExperiment {
    private static let flags: Set<String> = {
        #if DEBUG
        let value = UserDefaults.standard.string(forKey: "foldover-layout") ?? ""
        return Set(value.split(separator: ",").map(String.init))
        #else
        return []
        #endif
    }()

    static func has(_ flag: String) -> Bool { flags.contains(flag) }
}

/// The detail column's minimum as a column width, when given one.
struct DetailColumnWidth: ViewModifier {
    let min: CGFloat?

    @ViewBuilder
    func body(content: Content) -> some View {
        if let min {
            content.navigationSplitViewColumnWidth(min: min, ideal: min + 200)
        } else {
            content
        }
    }
}
