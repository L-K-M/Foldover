// The sheet's smaller parts: a folder's label on its half, the imprint a
// run leaves, the light that passes down the page while comparing, and
// the motions lines make (printing in, refusing a turn).

import AppKit
import SwiftUI

/// One of the pair's folders on its half of the page: where it is, its
/// full path on hover, and a menu to show it in Finder or copy its path.
struct FoldFolderLabel: View {
    let viewModel: MainViewModel
    let folder: SelectedFolder
    let side: FolderSide

    var body: some View {
        let text = viewModel.folderText(folder)
        VStack(alignment: side == .a ? .leading : .trailing, spacing: 1) {
            Text(PairSyncViewModel.folderName(side))
                .font(FoldType.meta)
                .foregroundStyle(FoldPalette.pencil)
            Label(text, systemImage: viewModel.folderSymbol(folder))
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(FoldPalette.ink(side))
                .lineLimit(1)
                .truncationMode(.middle)
        }
        // Room at the outer edge for the dog-ear.
        .padding(.leading, side == .a ? 30 : 8)
        .padding(.trailing, side == .a ? 8 : 30)
        .frame(maxWidth: .infinity, alignment: side == .a ? .leading : .trailing)
        .help(text)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("\(PairSyncViewModel.folderName(side)): \(text)"))
        .contextMenu {
            if viewModel.localURL(folder) != nil {
                Button("Show in Finder") { viewModel.reveal(side) }
            }
            Button("Copy Path") {
                let pasteboard = NSPasteboard.general
                pasteboard.clearContents()
                pasteboard.setString(PairEditorViewModel.pathText(folder), forType: .string)
            }
        }
    }
}

/// The run's imprint, set across the crease: its title, its counts in
/// large figures, and what it left for a person.
struct FoldImprintView: View {
    let imprint: FoldImprint
    let ink: Color

    var body: some View {
        VStack(spacing: 8) {
            Text(imprint.title)
                .font(FoldType.imprintTitle)
                .tracking(2)
                .foregroundStyle(FoldPalette.pencil)
            HStack(alignment: .firstTextBaseline, spacing: 22) {
                ForEach(imprint.figures, id: \.label) { figure in
                    VStack(spacing: 0) {
                        Text(figure.value)
                            .font(FoldType.figure)
                        Text(figure.label)
                            .font(FoldType.meta)
                    }
                }
            }
            .foregroundStyle(ink)
            .letterpress()
            ForEach(imprint.notes, id: \.self) { note in
                Text(note)
                    .font(.callout)
                    .foregroundStyle(FoldPalette.pencil)
            }
        }
        .padding(.vertical, 18)
        .padding(.horizontal, 30)
        .overlay {
            RoundedRectangle(cornerRadius: 2)
                .strokeBorder(ink.opacity(0.3), lineWidth: 1.25)
                .padding(3)
        }
        .accessibilityElement(children: .combine)
    }
}

/// A soft band of light passing down the page while the folders are
/// compared; absent under Reduce Motion.
struct ComparingSweep: View {
    let paused: Bool
    let timeScale: Double
    private static let period = 1.6
    private static let band: CGFloat = 80

    var body: some View {
        if !paused {
            TimelineView(.animation) { context in
                let period = Self.period * timeScale
                let phase = context.date.timeIntervalSinceReferenceDate
                    .truncatingRemainder(dividingBy: period) / period
                GeometryReader { proxy in
                    LinearGradient(colors: [.clear, Color.accentColor.opacity(0.10), .clear],
                                   startPoint: .top, endPoint: .bottom)
                        .frame(height: Self.band)
                        .offset(y: phase * (proxy.size.height + Self.band) - Self.band)
                }
            }
            .allowsHitTesting(false)
            .accessibilityHidden(true)
        }
    }
}

/// Ink spreading into paper: a printed line comes in from a blur.
struct InkIn: ViewModifier {
    let delay: Double
    let active: Bool
    @State private var inked = false

    func body(content: Content) -> some View {
        let shown = !active || inked
        content
            .opacity(shown ? 1 : 0)
            .blur(radius: shown ? 0 : 2.5)
            .scaleEffect(shown ? 1 : 1.015)
            .onAppear {
                guard active else { return }
                withAnimation(.easeOut(duration: 0.3).delay(delay)) { inked = true }
            }
    }
}

/// A short side-to-side shake: the line will not turn that way.
struct Shake: GeometryEffect {
    /// The shake count; each step from one count to the next swings
    /// twice and comes to rest.
    var animatableData: Double

    init(count: Int) {
        animatableData = Double(count)
    }

    func effectValue(size: CGSize) -> ProjectionTransform {
        ProjectionTransform(CGAffineTransform(translationX: 2.5 * sin(animatableData * .pi * 4), y: 0))
    }
}
