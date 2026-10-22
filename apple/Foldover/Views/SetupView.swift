import SwiftUI

/// The first-run guide: agent, first pair, first sync. It fills the
/// main window's detail until the first pair exists.
struct SetupView: View {
    let viewModel: MainViewModel

    var body: some View {
        // Centered when it fits, scrolling in a short window.
        ViewThatFits(in: .vertical) {
            guide
            ScrollView { guide }
        }
    }

    private var guide: some View {
        VStack(spacing: 24) {
            header
            VStack(alignment: .leading, spacing: 8) {
                ForEach(viewModel.setupSteps) { step in
                    stepRow(step)
                }
            }
            // The main window accepts the drop once pairs can be created.
            if viewModel.canConfigurePairs {
                dropHint
            }
        }
        .frame(maxWidth: 460)
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var header: some View {
        VStack(spacing: 8) {
            Image(systemName: "arrow.triangle.2.circlepath")
                .font(.system(size: 44))
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            Text("Welcome to Foldover")
                .font(.title.bold())
                .accessibilityAddTraits(.isHeader)
            Text("Keep folders in step across disks, servers and other Macs. Foldover shows every change before it runs.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func stepRow(_ step: SetupStep) -> some View {
        HStack(alignment: .top, spacing: 14) {
            badge(step)
            VStack(alignment: .leading, spacing: 6) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(step.title)
                        .font(.headline)
                        .foregroundStyle(step.progress == .upcoming ? .secondary : .primary)
                    Text(step.detail)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .accessibilityElement(children: .combine)
                // The badge shows where the step stands; VoiceOver says it.
                .accessibilityValue(progressText(step.progress))
                if let action = step.action {
                    Button(title(action)) { Task { await viewModel.perform(action) } }
                        .buttonStyle(.borderedProminent)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .background {
            if step.progress == .current {
                RoundedRectangle(cornerRadius: 10).fill(.quaternary)
            }
        }
    }

    /// The step's number; a check once it is done.
    private func badge(_ step: SetupStep) -> some View {
        ZStack {
            switch step.progress {
            case .done:
                Circle().fill(Color.accentColor.opacity(0.15))
                Image(systemName: "checkmark")
                    .font(.callout.bold())
                    .foregroundStyle(.tint)
            case .current:
                Circle().fill(Color.accentColor)
                Text(String(step.id))
                    .font(.callout.bold())
                    .foregroundStyle(.white)
            case .upcoming:
                Circle().fill(.quaternary)
                Text(String(step.id))
                    .font(.callout.bold())
                    .foregroundStyle(.secondary)
            }
        }
        .frame(width: 28, height: 28)
        .accessibilityHidden(true)
    }

    private var dropHint: some View {
        Label("You can also drop two folders here to create a pair.", systemImage: "square.and.arrow.down")
            .font(.callout)
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity)
            .padding(12)
            .overlay {
                RoundedRectangle(cornerRadius: 10)
                    .strokeBorder(.tertiary, style: StrokeStyle(lineWidth: 1, dash: [6]))
            }
    }

    private func progressText(_ progress: SetupProgress) -> Text {
        switch progress {
        case .done: Text("Done")
        case .current: Text("Next step")
        case .upcoming: Text("Not yet")
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
