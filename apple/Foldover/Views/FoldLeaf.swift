// The leaf: the half of the sheet that lifts off and swings over the
// crease. It carries what its half showed at the moment it lifted, its
// face darkening as it turns away from the light overhead, then shows
// the paper's cerulean back once past upright, as origami paper does.
// Its words give way to type bars as it starts to turn, so perspective
// never squeezes letters into slivers.
//
//        open           upright           folded
//   ┌─────┬─────┐       ┌─┬─────┐       ┌─────┬─────┐
//   │  A  │  B  │  →    │▌│  B  │  →    │navy │◤ ◢ │  the back
//   └─────┴─────┘       └─┴─────┘       └─────┴─────┘

import SwiftUI

/// Hands its content the leaf's swing at every frame of an animation.
/// `withAnimation` sets the state to its end at once; the sheet has to
/// draw every step between, so it reads the swing from here.
struct SwingProgress<Content: View>: View, Animatable {
    var animatableData: Double
    private let content: (Double) -> Content

    init(_ progress: Double, @ViewBuilder content: @escaping (Double) -> Content) {
        animatableData = progress
        self.content = content
    }

    var body: some View {
        content(min(max(animatableData, 0), 1))
    }
}

/// The lifted half at swing `progress`, 0 open … 1 folded flat, laid out
/// over the whole page and turning about the crease.
struct FoldLeafView<Label: View>: View {
    let side: FolderSide
    let progress: Double
    /// The half's entries as the sheet grouped them.
    let entries: [FoldEntry]
    let pageSize: CGSize
    /// Where the line list was scrolled when the leaf lifted.
    let scrollOffset: CGFloat
    let headerHeight: CGFloat
    let listInset: CGFloat
    let reduceMotion: Bool
    /// The selected line and its tint, and a turn staged on a line: the
    /// leaf carries them as its page showed them.
    let selection: (id: ChangeRow.ID, tint: SelectionTint)?
    let staged: (id: ChangeRow.ID, flow: FoldFlow)?
    /// The folder label printed at the top of the half.
    @ViewBuilder let label: Label

    /// A positive turn about the vertical axis brings the left edge
    /// forward: A's leaf turns positive, B's negative, so each free edge
    /// rises toward the viewer rather than sinking behind the page.
    private static var turnSign: Double { 1 }
    /// Upright, the free edge stands nearest and looks a little taller
    /// than the page: depth without spilling over the words around it.
    private static var perspective: CGFloat { 0.16 }
    /// How far either side of upright the paper's edge shows, and how
    /// tall it looks there, its near end lifted toward the viewer.
    private static var edgeWindow: Double { 0.05 }
    private static var edgeReach: CGFloat { 1.08 }
    /// How dark the type bars print: lines, not words.
    private static var barInk: Double { 0.45 }
    /// How wide the glint on the back is, in fractions of its diagonal.
    private static var glintBand: Double { 0.18 }

    private var half: CGFloat { pageSize.width / 2 }

    var body: some View {
        if reduceMotion {
            // No swing: the half fades from where it lay, then its back
            // fades in over the other half. One is gone before the other
            // comes, so neither lies half-transparent over a page's lines.
            ZStack(alignment: .topLeading) {
                front(ink: 1)
                    .frame(width: half, height: pageSize.height)
                    .opacity(FoldMotion.reducedFace(progress))
                    .offset(x: side == .a ? 0 : half)
                Kami()
                    .frame(width: half, height: pageSize.height)
                    .opacity(FoldMotion.reducedBack(progress))
                    .offset(x: side == .a ? half : 0)
            }
        } else {
            ZStack(alignment: .topLeading) {
                castShadow
                faces
                    .frame(width: half, height: pageSize.height)
                    .rotation3DEffect(.degrees(Self.turnSign * (side == .a ? 1 : -1) * progress * 180),
                                      axis: (x: 0, y: 1, z: 0),
                                      anchor: side == .a ? .trailing : .leading,
                                      perspective: Self.perspective)
                    .offset(x: side == .a ? 0 : half)
                edge
            }
        }
    }

    /// The face turns away from the light toward the navy's shade; the
    /// back arrives in shade and brightens as it comes down, a glint
    /// crossing it on the way.
    private var faces: some View {
        let light = sin(progress * .pi)
        return ZStack {
            if progress >= FoldMotion.tippingPoint {
                // Mirrored so it reads the right way round once turned.
                Kami()
                    .overlay(FoldPalette.deskShade.opacity(0.38 * light))
                    .overlay { glint }
                    .scaleEffect(x: -1)
            } else {
                front(ink: FoldMotion.faceInk(progress))
                    .overlay(FoldPalette.deskShade.opacity(0.12 * light))
            }
        }
    }

    private var glint: some View {
        let sweep = min(max((progress - 0.55) / 0.4, 0), 1)
        let strength = max(0, sin(.pi * (2 * progress - 1)))
        return LinearGradient(stops: [
            .init(color: .clear, location: max(0, sweep - Self.glintBand)),
            .init(color: .white.opacity(0.18 * strength), location: sweep),
            .init(color: .clear, location: min(1, sweep + Self.glintBand)),
        ], startPoint: .topLeading, endPoint: .bottomTrailing)
    }

    /// The shadow the leaf casts on what lies under it, from the crease
    /// past its free edge: wide and soft while the leaf is high, narrow
    /// and dark as it comes down. Short of upright that is the navy its
    /// half lifted off; past upright, the other half's paper.
    @ViewBuilder
    private var castShadow: some View {
        if progress < 1 {
            let lowness = abs(2 * progress - 1)
            let over: FolderSide = progress > FoldMotion.tippingPoint ? (side == .a ? .b : .a) : side
            let reach = half * lowness
            let width = reach + 4 + 24 * (1 - lowness)
            let color = over == side
                ? FoldPalette.castShadow.opacity(0.35 + 0.25 * lowness)
                : FoldPalette.foldShadow.opacity(0.1 + 0.2 * lowness)
            LinearGradient(stops: [.init(color: color, location: 0), .init(color: color, location: reach / width),
                                   .init(color: .clear, location: 1)],
                           startPoint: over == .b ? .leading : .trailing,
                           endPoint: over == .b ? .trailing : .leading)
                .frame(width: width, height: pageSize.height)
                .blur(radius: 1 + 8 * (1 - lowness))
                .offset(x: over == .b ? half : half - width)
        }
    }

    /// The paper's edge, seen near upright, so the hinge never vanishes.
    private var edge: some View {
        Rectangle()
            .fill(FoldPalette.paper)
            .frame(width: 1.5, height: pageSize.height * Self.edgeReach)
            .shadow(color: .black.opacity(0.25), radius: 1)
            .opacity(max(0, 1 - abs(progress - FoldMotion.tippingPoint) / Self.edgeWindow))
            .offset(x: half - 0.75, y: -pageSize.height * (Self.edgeReach - 1) / 2)
    }

    /// The paper's face with what was printed on it: words at `ink`, and
    /// type bars where the words fade.
    private func front(ink: Double) -> some View {
        ZStack(alignment: .topLeading) {
            Paper(facet: PaperFacet(side))
            if ink < 1 {
                printed
                    .redacted(reason: .placeholder)
                    .opacity((1 - ink) * Self.barInk)
            }
            if ink > 0 {
                printed.opacity(ink)
            }
        }
    }

    /// The half's folder label and the lines that were in view, frozen
    /// where the list was scrolled.
    private var printed: some View {
        let offset = max(0, scrollOffset - listInset)
        let first = min(entries.count, Int(offset / FoldMetrics.rowHeight))
        let visible = Int(pageSize.height / FoldMetrics.rowHeight) + 2
        let window = Array(entries.dropFirst(first).prefix(visible))
        return VStack(spacing: 0) {
            label
                .frame(height: headerHeight)
            // The rows hang from under the label and are cut at the
            // page's foot. Laid out directly they would be taller than
            // the page, and the leaf would center them above it.
            Color.clear
                .overlay(alignment: .top) {
                    VStack(spacing: 0) {
                        ForEach(window) { entry in
                            cell(entry)
                                .frame(height: FoldMetrics.rowHeight)
                        }
                    }
                    .offset(y: CGFloat(first) * FoldMetrics.rowHeight - scrollOffset + listInset)
                }
                .clipped()
        }
        // The leaf reaches the middle of the gutter; the page's lines
        // stop at its edge.
        .padding(side == .a ? .trailing : .leading, FoldMetrics.creaseWidth / 2)
    }

    @ViewBuilder
    private func cell(_ entry: FoldEntry) -> some View {
        switch entry {
        case .folder(let path, let count):
            FoldHeadingHalf(path: path, count: count, side: side)
        case .line(let line):
            FoldHalf(line: line, side: side, staged: staged?.id == line.id ? staged?.flow : nil)
                .background {
                    if let selection, selection.id == line.id { selection.tint }
                }
        }
    }
}
