// The sheet's smaller parts: a folder's label on its half, the card a
// run leaves, the light that passes down the page while comparing, and
// the motions lines make (printing in, refusing a turn).

import AppKit
import SwiftUI

/// One of the pair's folders at the head of its page: its name, where it
/// is, its full path on hover, and a menu to show it in Finder or copy
/// its path. It reads left to right like the page's lines under it.
struct FoldFolderLabel: View {
    let viewModel: MainViewModel
    let folder: SelectedFolder
    let side: FolderSide

    /// Clear of the dog-ear at Folder B's outer corner.
    private static let earRoom: CGFloat = 30

    var body: some View {
        let text = viewModel.folderText(folder)
        let parts = Self.parts(text)
        VStack(alignment: .leading, spacing: 1) {
            Text(PairSyncViewModel.folderName(side))
                .font(FoldType.eyebrow)
                .textCase(.uppercase)
                .tracking(FoldType.eyebrowTracking)
                .foregroundStyle(FoldPalette.umber)
            // The name stays whole as long as it can; where the folder
            // is gives way first, keeping its nearest folders.
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Label(parts.name, systemImage: viewModel.folderSymbol(folder))
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(FoldPalette.ink(side))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .layoutPriority(1)
                if !parts.location.isEmpty {
                    Text(parts.location)
                        .font(FoldType.meta)
                        .foregroundStyle(FoldPalette.umber)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
            }
        }
        // In line with the page's names, and clear of the dog-ears: A's
        // names start past its marks, which clear its ear.
        .padding(.leading, side == .a
                 ? FoldMetrics.outerMarginA + FoldMetrics.markWidth + FoldMetrics.markGap
                 : FoldMetrics.innerMargin)
        .padding(.trailing, side == .a ? FoldMetrics.innerMargin : Self.earRoom)
        .frame(maxWidth: .infinity, alignment: .leading)
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

    /// A folder's text as its name and where it is: "Photos" on
    /// "mynas.local/~". The scheme is left out, since the symbol already
    /// says it is a server. Text without a folder above it, such as a
    /// server's address or another Mac's folder, is all name.
    static func parts(_ text: String) -> (name: String, location: String) {
        var path = Substring(text)
        if path.count > 1, path.hasSuffix("/") { path = path.dropLast() }
        guard let slash = path.lastIndex(of: "/"), path.index(after: slash) < path.endIndex else {
            return (String(path), "")
        }
        var location = path[..<slash]
        // "sftp://host" names a server, not a folder inside "sftp:/".
        guard !location.hasSuffix(":/") else { return (String(path), "") }
        if let scheme = location.range(of: "://") { location = location[scheme.upperBound...] }
        return (String(path[path.index(after: slash)...]), location.isEmpty ? "/" : String(location))
    }
}

/// The card a run leaves on the page the leaf pressed: a flat square of
/// the paper's cerulean back with its title and its counts in large
/// white figures, and under it what the run left for a person.
struct FoldImprintView: View {
    let imprint: FoldImprint

    /// Crisp, as a folded paper's corner is.
    private static let corner: CGFloat = 3

    var body: some View {
        VStack(spacing: 12) {
            card
            ForEach(imprint.notes, id: \.self) { note in
                Text(note)
                    .font(.callout)
                    .foregroundStyle(FoldPalette.umber)
                    .multilineTextAlignment(.center)
            }
        }
        .accessibilityElement(children: .combine)
    }

    private var card: some View {
        VStack(spacing: 10) {
            Text(imprint.title)
                .font(FoldType.backEyebrow)
                .textCase(.uppercase)
                .tracking(FoldType.eyebrowTracking * 1.5)
            // The counts abreast while the page holds them, else a list.
            ViewThatFits(in: .horizontal) {
                abreast(spacing: 26, font: FoldType.figure)
                abreast(spacing: 14, font: FoldType.smallFigure)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(imprint.figures, id: \.label) { figure in
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Text(figure.value)
                                .font(FoldType.smallFigure)
                            figureLabel(figure)
                        }
                    }
                }
            }
        }
        .foregroundStyle(.white)
        .padding(.vertical, 18)
        .padding(.horizontal, 22)
        // Flat: no facet ridge runs through the figures.
        .background(Kami(finish: .flat).clipShape(RoundedRectangle(cornerRadius: Self.corner, style: .continuous)))
        // Cast onto the paper down and to the right, away from the light.
        .shadow(color: FoldPalette.foldShadow.opacity(0.28), radius: 12, x: 3, y: 8)
    }

    private func abreast(spacing: CGFloat, font: Font) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: spacing) {
            ForEach(imprint.figures, id: \.label) { figure in
                VStack(spacing: 2) {
                    Text(figure.value)
                        .font(font)
                    figureLabel(figure)
                }
            }
        }
    }

    private func figureLabel(_ figure: FoldImprint.Figure) -> some View {
        Text(figure.label)
            .font(FoldType.backEyebrow)
            .textCase(.uppercase)
            .tracking(FoldType.eyebrowTracking)
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
            // A soft band reads as smooth at a modest rate.
            TimelineView(.animation(minimumInterval: 1.0 / 30)) { context in
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
            // Printing usually begins just after the line first appears,
            // so this follows `active`, not the appearance alone.
            .onChange(of: active, initial: true) { _, printing in
                guard printing else { return }
                inked = false
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
