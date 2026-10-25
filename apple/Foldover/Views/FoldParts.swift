// The sheet's smaller parts: a folder's label on its half, the card a
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
                .font(FoldType.eyebrow)
                .textCase(.uppercase)
                .tracking(FoldType.eyebrowTracking)
                .foregroundStyle(FoldPalette.slate)
            Label(text, systemImage: viewModel.folderSymbol(folder))
                .font(.system(size: 13, weight: .semibold))
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

/// The card a run leaves, set across the crease: a square of the
/// paper's cerulean back with its title and its counts in large white
/// figures, and under it what the run left for a person.
struct FoldImprintView: View {
    let imprint: FoldImprint

    private static let corner: CGFloat = 14

    var body: some View {
        VStack(spacing: 12) {
            card
            ForEach(imprint.notes, id: \.self) { note in
                Text(note)
                    .font(.callout)
                    .foregroundStyle(FoldPalette.slate)
                    .multilineTextAlignment(.center)
            }
        }
        .accessibilityElement(children: .combine)
    }

    private var card: some View {
        VStack(spacing: 10) {
            Text(imprint.title)
                .font(FoldType.eyebrow)
                .textCase(.uppercase)
                .tracking(FoldType.eyebrowTracking * 1.5)
                .foregroundStyle(.white.opacity(0.85))
            HStack(alignment: .firstTextBaseline, spacing: 26) {
                ForEach(imprint.figures, id: \.label) { figure in
                    VStack(spacing: 2) {
                        Text(figure.value)
                            .font(FoldType.figure)
                        Text(figure.label)
                            .font(FoldType.eyebrow)
                            .textCase(.uppercase)
                            .tracking(FoldType.eyebrowTracking)
                            .opacity(0.85)
                    }
                }
            }
            .foregroundStyle(.white)
        }
        .padding(.vertical, 18)
        .padding(.horizontal, 30)
        .background(Kami().clipShape(RoundedRectangle(cornerRadius: Self.corner, style: .continuous)))
        // Cast onto the paper, as the icon's folds cast onto the navy.
        .shadow(color: FoldPalette.deskShade.opacity(0.35), radius: 14, x: 0, y: 8)
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
