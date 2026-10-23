import SwiftUI

/// Keep optional guidance beside its control, available by mouse or keyboard.
struct InfoButton: View {
    let title: Text
    let message: Text
    @State private var isPresented = false

    var body: some View {
        Button {
            isPresented.toggle()
        } label: {
            Label { Text("About \(title)") } icon: { Image(systemName: "info.circle") }
        }
        .labelStyle(.iconOnly)
        .buttonStyle(.borderless)
        .font(.body)
        .foregroundStyle(.secondary)
        .help(Text("About \(title)"))
        .popover(isPresented: $isPresented) {
            VStack(alignment: .leading, spacing: 12) {
                title
                    .font(.headline)
                    .accessibilityAddTraits(.isHeader)
                message
                    .font(.body)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("info-message")
            }
            .foregroundStyle(.primary)
            .textCase(nil)
            .multilineTextAlignment(.leading)
            .padding(16)
            .frame(width: 360, alignment: .leading)
            .fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// A setting or section title with its help anchored to the info icon.
struct InfoLabel: View {
    let title: Text
    let message: Text

    var body: some View {
        HStack(spacing: 6) {
            title
            InfoButton(title: title, message: message)
        }
    }
}
