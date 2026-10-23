import SwiftUI

/// A split view column's content, sized to whatever the split view gives
/// the column and reporting no minimum size of its own.
///
/// AppKit asks every column for its size limits during each constraint
/// pass. When a column's minimum depends on its own width, as it does
/// for wrapping text, a status bar in a safe-area inset or a table
/// beside fixed-size controls, the limits change from one pass to the
/// next, and AppKit ends the loop by raising an exception that quits
/// the app. A GeometryReader takes the size it is offered and reports
/// none of its content's, so a column's limits come only from its
/// `navigationSplitViewColumnWidth` or `inspectorColumnWidth` and the
/// window's minimum size. Every column in the app sits in one.
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
