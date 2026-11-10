// The sheet's smaller parts: a folder's label on its half, the card a
// run leaves, a working job's readout, the light that passes down the
// page while comparing, and the motions lines make (printing in,
// refusing a turn).

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
        .contextMenu { FoldFolderMenu(viewModel: viewModel, folder: folder) }
    }

    /// A folder's text as its name and where it is: "Photos" on
    /// "mynas.local/~". The scheme is left out, since the symbol already
    /// says it is a server. Text without a folder above it, such as a
    /// server's address or another Mac's folder, is all name.
    static func parts(_ text: String) -> (name: String, location: String) {
        var path = Substring(text)
        while path.count > 1, path.hasSuffix("/") { path = path.dropLast() }
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

/// What a folder's name offers when Control-clicked: show it in Finder,
/// or copy its path.
private struct FoldFolderMenu: View {
    let viewModel: MainViewModel
    let folder: SelectedFolder

    var body: some View {
        if viewModel.localURL(folder) != nil {
            Button("Show in Finder") { viewModel.reveal(folder) }
        }
        Button("Copy Path") {
            let pasteboard = NSPasteboard.general
            pasteboard.clearContents()
            pasteboard.setString(PairEditorViewModel.pathText(folder), forType: .string)
        }
    }
}

/// A folder named at the head of its own half while the leaf lies
/// folded, in white, so each folder stays on its side of the sheet: on
/// the bare desk, the folder whose page lifted off; on the leaf's back,
/// the folder whose page it lies on. The page labels are out of sight
/// then, one folded face down and one covered. The eyebrow says which
/// way the folder's data goes.
///
///        FROM FOLDER A          TO FOLDER B
///        ⌂ lmathis              ⌸ Foldover
///           /Users         /Volumes/Backup SSD
struct FoldRunFolder: View {
    /// How much of the folder is set.
    enum Detail {
        case full
        /// Without where it is, for a half too short for the line.
        case nameOnly
    }

    let viewModel: MainViewModel
    let folder: SelectedFolder
    let side: FolderSide
    let role: FoldRole
    var detail = Detail.full

    /// How far under the top of its half the name hangs, the same on the
    /// leaf and the desk, so the two names read as one line across.
    static let top: CGFloat = 24
    /// The least room between the name and what is set under it.
    static let gap: CGFloat = 18

    var body: some View {
        let text = viewModel.folderText(folder)
        let parts = FoldFolderLabel.parts(text)
        VStack(spacing: 3) {
            Text(role.eyebrow(side))
                .font(FoldType.backEyebrow)
                .textCase(.uppercase)
                .tracking(FoldType.eyebrowTracking * 1.5)
                .opacity(0.75)
            Label(parts.name, systemImage: viewModel.folderSymbol(folder))
                .font(.system(size: 15, weight: .semibold))
                .lineLimit(1)
                .truncationMode(.middle)
            if detail == .full, !parts.location.isEmpty {
                // Where it is gives way first, keeping its nearest folders.
                Text(parts.location)
                    .font(FoldType.meta)
                    .opacity(0.75)
                    .lineLimit(1)
                    .truncationMode(.head)
            }
        }
        .foregroundStyle(.white)
        .help(text)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("\(role.eyebrow(side)): \(text)"))
        .contextMenu { FoldFolderMenu(viewModel: viewModel, folder: folder) }
    }
}

/// The bare desk where the leaf lifted off, while it lies folded over
/// the other half: the folder that was there, and an arrow toward the
/// leaf saying which way the run's data goes and how. Without it the
/// desk lay empty for the whole run.
struct FoldDeskNote: View {
    let viewModel: MainViewModel
    let folder: SelectedFolder
    let side: FolderSide
    let mode: PairMode

    var body: some View {
        // The name hangs where the leaf's does; the arrow sits level with
        // the leaf's readout, and gives way when a short window has no
        // room for it.
        ViewThatFits(in: .vertical) {
            VStack(spacing: FoldRunFolder.gap) {
                name
                direction
                    .frame(maxHeight: .infinity)
            }
            name
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Where the Leaf Lifted Off"))
        .accessibilityIdentifier("fold-desk")
    }

    private var name: some View {
        FoldRunFolder(viewModel: viewModel, folder: folder, side: side, role: FoldRole(side, in: mode))
            .padding(.top, FoldRunFolder.top)
    }

    private var direction: some View {
        VStack(spacing: 14) {
            // The mode alone sets the arrow: in a one-way mode only the
            // source folds, so the desk is always the source's half and
            // the arrow always points across to the leaf.
            FoldArrow(direction: FoldDirection(mode))
            Text(mode.title)
                .font(FoldType.backEyebrow)
                .textCase(.uppercase)
                .tracking(FoldType.eyebrowTracking * 1.5)
                .foregroundStyle(FoldPalette.deskInk)
                .multilineTextAlignment(.center)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(mode.title))
        .accessibilityIdentifier("fold-direction")
    }
}

/// Which way a run's data goes, folded like the app icon's arrow: an
/// ivory tail and a cerulean head in two facets, lit from the top left
/// and casting a soft shadow down and to the right. A Backup's arrow
/// meets a paper edge, since the folder it reaches keeps what the other
/// deletes; a Mirror's flies on; a two-way Sync's are two arrows passing.
///
///   Mirror ═◣▶      Backup ═◣▶▕      Sync  ═◣▶
///                                         ◀◢═
private struct FoldArrow: View {
    let direction: FoldDirection

    private static let size = CGSize(width: 92, height: 54)
    /// Each of a two-way Sync's arrows, and how far they pass each other.
    private static let passingScale: CGFloat = 0.72
    private static let passing: CGFloat = 12
    private static let edgeWidth: CGFloat = 5
    private static let edgeGap: CGFloat = 5

    var body: some View {
        mark
            // Lit from the top left whichever way it points.
            .shadow(color: FoldPalette.castShadow.opacity(0.45), radius: 3, x: 1.5, y: 3)
            // The icon's own ivory and cerulean in either appearance: on
            // the dark field the paper is still the paper.
            .environment(\.colorScheme, .light)
            .accessibilityHidden(true)
    }

    @ViewBuilder
    private var mark: some View {
        switch direction {
        case .mirror(let toward):
            Dart(size: Self.size)
                .scaleEffect(x: toward == .b ? 1 : -1)
        case .backup(let toward):
            // Only the dart turns round: the edge stays lit on its left.
            HStack(spacing: Self.edgeGap) {
                if toward == .a { edge }
                Dart(size: Self.size)
                    .scaleEffect(x: toward == .b ? 1 : -1)
                if toward == .b { edge }
            }
        case .both:
            let small = CGSize(width: Self.size.width * Self.passingScale,
                               height: Self.size.height * Self.passingScale)
            VStack(spacing: 4) {
                Dart(size: small)
                    .offset(x: Self.passing)
                Dart(size: small)
                    .scaleEffect(x: -1)
                    .offset(x: -Self.passing)
            }
        }
    }

    private var edge: some View {
        PaperEdge()
            .frame(width: Self.edgeWidth, height: Self.size.height)
    }

    /// One arrow, pointing right: the tail's two folds under the head's
    /// two facets, the crease down the middle of each.
    private struct Dart: View {
        let size: CGSize

        var body: some View {
            ZStack {
                DartPart(part: .tailUpper).fill(FoldPalette.paper)
                DartPart(part: .tailLower).fill(FoldPalette.paperShade)
                DartPart(part: .tailLower).fill(FoldPalette.foldShadow.opacity(0.14))
                DartPart(part: .tailCrease).stroke(.white.opacity(0.8), lineWidth: 0.75)
                DartPart(part: .headUpper).fill(FoldPalette.kamiLit)
                // The facet facing the light is as bright as the icon's.
                DartPart(part: .headUpper).fill(.white.opacity(0.14))
                DartPart(part: .headLower).fill(FoldPalette.kamiShade)
                DartPart(part: .headCrease).stroke(FoldPalette.kamiRidge.opacity(0.8), lineWidth: 1)
            }
            .frame(width: size.width, height: size.height)
        }
    }

    /// The paper edge a Backup's arrow meets: lit along its left side.
    private struct PaperEdge: View {
        var body: some View {
            RoundedRectangle(cornerRadius: 1)
                .fill(FoldPalette.paperShade)
                .overlay(alignment: .leading) { Color.white.frame(width: 1) }
                .clipShape(RoundedRectangle(cornerRadius: 1))
        }
    }

    /// The arrow's pieces, in a box it fills, pointing right:
    ///
    ///                 ◢ top
    ///   ┌───────────╱  ╲
    ///   ╲ notch   back ───▶ tip
    ///   ╱───────────╲  ╱
    ///                 ◥ bottom
    private struct DartPart: Shape {
        enum Part { case tailUpper, tailLower, tailCrease, headUpper, headLower, headCrease }
        let part: Part

        func path(in rect: CGRect) -> Path {
            func point(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
                CGPoint(x: rect.minX + x * rect.width, y: rect.minY + y * rect.height)
            }
            // The tail runs under the head, so no seam shows between them.
            let tailTop: CGFloat = 0.24, tailBottom: CGFloat = 0.76, tailEnd: CGFloat = 0.66
            let notch = point(0.17, 0.5), tailMiddle = point(tailEnd, 0.5)
            let top = point(0.56, 0), tip = point(1, 0.5), bottom = point(0.56, 1), back = point(0.47, 0.5)
            var path = Path()
            switch part {
            case .tailUpper:
                path.addLines([point(0, tailTop), point(tailEnd, tailTop), tailMiddle, notch])
                path.closeSubpath()
            case .tailLower:
                path.addLines([notch, tailMiddle, point(tailEnd, tailBottom), point(0, tailBottom)])
                path.closeSubpath()
            case .tailCrease:
                path.move(to: notch)
                path.addLine(to: tailMiddle)
            case .headUpper:
                path.addLines([top, tip, back])
                path.closeSubpath()
            case .headLower:
                path.addLines([back, tip, bottom])
                path.closeSubpath()
            case .headCrease:
                path.move(to: back)
                path.addLine(to: tip)
            }
            return path
        }
    }
}

/// A disk's name and free space beside a small capacity donut, in white
/// on the leaf's back under the readout of the run that copies there.
struct FoldDiskView: View {
    let disk: FoldDisk

    private static let donutSize: CGFloat = 40

    var body: some View {
        VStack(spacing: 5) {
            HStack(spacing: 10) {
                DiskDonut(stored: disk.stored, arriving: disk.arriving)
                    .frame(width: Self.donutSize, height: Self.donutSize)
                VStack(alignment: .leading, spacing: 3) {
                    Text(disk.name)
                        .font(FoldType.backEyebrow)
                        .textCase(.uppercase)
                        .tracking(FoldType.eyebrowTracking * 1.5)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Text(disk.free)
                        .font(.callout)
                    if let toCopy = disk.toCopy {
                        Text(toCopy)
                            .font(.callout)
                            .opacity(0.75)
                    }
                }
                .multilineTextAlignment(.leading)
                .fixedSize(horizontal: false, vertical: true)
            }
            if disk.mayNotFit {
                Label(FoldDisk.mayNotFitText, systemImage: "exclamationmark.triangle.fill")
                    .font(.callout)
                    .symbolRenderingMode(.multicolor)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .foregroundStyle(.white)
        .multilineTextAlignment(.center)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(disk.spoken)
        .accessibilityAddTraits(.updatesFrequently)
        .accessibilityIdentifier("fold-disk")
    }
}

/// Capacity, not progress: a closed ring in shades of white, with stored
/// data brightest, incoming copies fainter, and free space dimmest. Butt
/// caps keep the segments proportional even for an empty or full disk.
private struct DiskDonut: View {
    let stored: Double
    let arriving: Double

    private static let lineWidth: CGFloat = 6

    var body: some View {
        let stored = min(1, self.stored)
        let filled = min(1, stored + arriving)
        ZStack {
            Circle().trim(from: filled, to: 1)
                .stroke(.white.opacity(0.22), lineWidth: Self.lineWidth)
            Circle().trim(from: stored, to: filled)
                .stroke(.white.opacity(0.6), lineWidth: Self.lineWidth)
            Circle().trim(from: 0, to: stored)
                .stroke(.white.opacity(0.92), lineWidth: Self.lineWidth)
        }
        .padding(Self.lineWidth / 2)
        .rotationEffect(.degrees(-90))
        .accessibilityHidden(true)
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
            // One text, so on a narrow page the notes wrap together
            // rather than vie for lines and cut each other short.
            if !imprint.notes.isEmpty {
                Text(imprint.notes.joined(separator: "\n"))
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
                // The narrowest page: each count over its label, which
                // shrinks a little rather than break mid-word.
                VStack(spacing: 6) {
                    ForEach(imprint.figures, id: \.label) { figure in
                        VStack(spacing: 2) {
                            Text(figure.value)
                                .font(FoldType.smallFigure)
                            figureLabel(figure)
                                .lineLimit(1)
                                .minimumScaleFactor(0.8)
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

/// What a readout is printed on.
enum FoldSurface {
    /// The paper's face, in umber: a preview comparing.
    case paper
    /// The leaf's cerulean back, in white: a run pressing.
    case back
}

/// A working job's readout: the stage it is in, a figure and what it
/// counts, a bar, and a few short lines. The same layout is printed in
/// white on the folded leaf's back while a run presses, and in umber on
/// the page while a preview compares. The bar appears only once the job
/// knows its whole; until then a spinner turns under the count.
///
///        COMPARING                    SYNCING
///         371,762                       42%
///      ITEMS CHECKED                  COPIED
///            ◌                 ━━━━━━━━░░░░░░░░░░░
///   of 6,810,813 found       1,204 of 5,320 changes
///        so far            16.8 GB of 40 GB · 85.0 MB/s
///                             about 4 min 32 s left
struct FoldReadoutView: View {
    let readout: FoldReadout
    let surface: FoldSurface
    /// The sheet's motion policy: under it, figures change in place.
    let reduceMotion: Bool

    /// The bar's longest reach, so it reads as a gauge on a wide page.
    private static let barWidth: CGFloat = 220
    private static let roll = Animation.smooth(duration: 0.3)

    var body: some View {
        VStack(spacing: 10) {
            Text(readout.title)
                .font(FoldType.backEyebrow)
                .textCase(.uppercase)
                .tracking(FoldType.eyebrowTracking * 1.5)
            figure
                // Once the plan is known the figure is a share, not a
                // count: it takes the count's place rather than rolling
                // on from it. Stop keeps whichever it is.
                .id(readout.fraction != nil)
            bar
            if !readout.details.isEmpty {
                // One text, so on a narrow page the lines wrap together,
                // and whole: a short page grows the readout rather than
                // cut its lines.
                Text(readout.details.joined(separator: "\n"))
                    .font(.callout)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .foregroundStyle(ink)
        .animation(reduceMotion ? nil : Self.roll, value: readout.stage)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(readout.spoken)
        .accessibilityAddTraits(.updatesFrequently)
        .accessibilityIdentifier("fold-readout")
    }

    private var ink: Color { surface == .back ? .white : FoldPalette.umber }

    @ViewBuilder
    private var figure: some View {
        if let value = readout.figure {
            VStack(spacing: 2) {
                Text(value)
                    .font(FoldType.figure)
                    // The digits roll as the count climbs.
                    .contentTransition(.numericText())
                    .animation(reduceMotion ? nil : Self.roll, value: value)
                    // Hundreds of millions still fit the narrowest page.
                    .lineLimit(1)
                    .minimumScaleFactor(0.4)
                if let caption = readout.caption {
                    Text(caption)
                        .font(FoldType.backEyebrow)
                        .textCase(.uppercase)
                        .tracking(FoldType.eyebrowTracking)
                }
            }
        } else {
            // Nothing counts while another Mac holds the folders.
            Image(systemName: "hourglass")
                .font(.system(size: 30, weight: .light))
        }
    }

    @ViewBuilder
    private var bar: some View {
        if let fraction = readout.fraction {
            ProgressView(value: fraction)
                .tint(ink)
                .frame(maxWidth: Self.barWidth)
        } else if readout.stage != .waiting {
            // The job cannot say yet how much there is: a spinner, which
            // no one reads as a share. An open-ended bar would be read as
            // one, since Reduce Motion holds its moving segment still.
            ProgressView()
                .controlSize(.small)
        }
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
