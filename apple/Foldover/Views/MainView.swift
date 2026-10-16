// The main window's placeholder content: title, empty state, core
// version. The pair sidebar, live plan and inspector arrive in P3
// (PLAN.md §3.8).

import SwiftUI

struct MainView: View {
    let viewModel: MainViewModel

    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "arrow.triangle.2.circlepath")
                .font(.system(size: 48))
                .foregroundStyle(.secondary)

            Text(viewModel.title)
                .font(.largeTitle)

            Text(viewModel.emptyState)
                .foregroundStyle(.secondary)

            Text(viewModel.coreVersionText)
                .font(.caption)
                .foregroundStyle(.tertiary)
        }
        .frame(minWidth: 480, minHeight: 320)
    }
}
