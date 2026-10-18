import SwiftUI

/// The first-run guide: agent, first pair, first sync.
struct SetupView: View {
    let viewModel: MainViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Welcome to Foldover").font(.largeTitle)
            ForEach(viewModel.setupSteps) { step in
                HStack(alignment: .top, spacing: 12) {
                    Image(systemName: symbol(step.progress))
                        .foregroundStyle(step.progress == .upcoming ? .secondary : .primary)
                        .font(.title2)
                        .accessibilityHidden(true)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(step.title).font(.headline)
                        Text(step.detail).foregroundStyle(.secondary)
                        if let action = step.action {
                            Button(title(action)) { Task { await viewModel.perform(action) } }
                                .buttonStyle(.borderedProminent)
                        }
                    }
                }
                .accessibilityElement(children: .combine)
            }
        }
        .padding(32)
        .frame(maxWidth: 560, maxHeight: .infinity, alignment: .topLeading)
    }

    private func symbol(_ progress: SetupProgress) -> String {
        switch progress {
        case .done: "checkmark.circle.fill"
        case .current: "arrow.right.circle.fill"
        case .upcoming: "circle"
        }
    }

    private func title(_ action: SetupAction) -> LocalizedStringKey {
        switch action {
        case .enableAgent: "Turn On"
        case .openLoginItems: "Open Login Items"
        case .newPair: "New Pair…"
        }
    }
}
