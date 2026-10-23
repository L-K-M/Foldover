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
