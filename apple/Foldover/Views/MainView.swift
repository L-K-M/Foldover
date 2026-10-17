// Pair-empty content and background-agent controls (PLAN.md §3.8).

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

            Text(viewModel.agentStatusText)
                .font(.callout)
                .multilineTextAlignment(.center)

            HStack {
                Button("Enable Background Agent") {
                    Task { await viewModel.enableAgent() }
                }
                if viewModel.agentNeedsApproval {
                    Button("Open Login Items") { viewModel.openLoginItems() }
                }
                Button("Refresh") { Task { await viewModel.refreshAgent() } }
            }
        }
        .frame(minWidth: 480, minHeight: 320)
        .task { await viewModel.refreshAgent() }
    }
}
