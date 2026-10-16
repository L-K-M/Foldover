// The menu-bar surface (PLAN.md §3.8): a status line, a way back to
// the main window, and Quit.

import AppKit
import SwiftUI

struct MenuBarView: View {
    @Environment(\.openWindow) private var openWindow
    let viewModel: MainViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(viewModel.statusLine)
                .font(.headline)

            Text(viewModel.coreVersionText)
                .font(.caption)
                .foregroundStyle(.secondary)

            Divider()

            // The WindowGroup's "main" id; if a window is already open
            // it is brought forward instead.
            Button("Open Foldover") {
                openWindow(id: "main")
            }

            Button("Quit Foldover") {
                NSApplication.shared.terminate(nil)
            }
            .keyboardShortcut("q")
        }
        .padding()
        .frame(width: 220)
    }
}
