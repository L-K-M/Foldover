import SwiftUI

/// A split view column's content, sized to whatever the split view gives
/// the column and reporting no minimum size of its own.
///
/// AppKit asks every column for its size limits during each constraint
/// pass. When a column's minimum depends on its own width, as it does
/// for wrapping text or a table beside fixed-size controls, the limits
/// change from one pass to the next, and AppKit ends the loop by raising
/// an exception that quits the app. A GeometryReader takes the size it
/// is offered and reports none of its content's, so such content sits in
/// one of these.
///
/// A column left with no minimum at all crashed the same way once the
/// window narrowed and the split view squeezed it, so the column gets a
/// fixed minimum width outside this view, one that the window's minimum
/// width leaves room for beside the other columns at theirs.
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
