// The menu-bar surface (PLAN.md §3.8): every pair's status with a way to
// sync it, Sync All, the main window, and Quit.

import AppKit
import SwiftUI

struct MenuBarView: View {
    @Environment(\.openWindow) private var openWindow
    let viewModel: MainViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(viewModel.statusLine)
                .font(.headline)

            ForEach(viewModel.menuRows) { row in
                HStack {
                    VStack(alignment: .leading) {
                        Text(row.name).lineLimit(1)
                        Text(row.status).font(.caption).foregroundStyle(.secondary)
                    }
                    .accessibilityElement(children: .combine)
                    Spacer()
                    Button("Sync") { Task { await viewModel.syncPair(id: row.id) } }
                        .disabled(!row.canSync)
                        .accessibilityLabel("Sync \(row.name)")
                }
            }

            if !viewModel.menuRows.isEmpty {
                Button("Sync All") { Task { await viewModel.syncAll() } }
                    .disabled(!viewModel.canSyncAll)
            }

            if let pause = viewModel.pauseText {
                Text(pause).font(.caption)
                Button("Resume Automatic Syncs") { Task { await viewModel.resume() } }
            } else {
                Menu("Pause Automatic Syncs") {
                    ForEach(PauseLength.allCases, id: \.self) { length in
                        Button(MainViewModel.title(length)) { Task { await viewModel.pause(length) } }
                    }
                }
            }

            if let error = viewModel.operationError {
                Text(error).font(.caption).foregroundStyle(.red)
            }

            Text(viewModel.agentStatusText)
                .font(.caption)
                .foregroundStyle(.secondary)

            Divider()

            // The window's "main" id; if a window is already open
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
        .frame(width: 280)
    }
}
