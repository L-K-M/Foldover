// The leaf: the half of the sheet that lifts off and swings over the
// crease. It carries what its half showed at the moment it lifted, its
// front darkening as it turns away from the light overhead, then shows
// the paper's back once past upright.
//
//        open           upright           folded
//   ┌─────┬─────┐       ┌─┬─────┐       ┌─────┬─────┐
//   │  A  │  B  │  →    │▌│  B  │  →    │desk │back │
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
    let lines: [FoldLine]
    let pageSize: CGSize
    /// Where the line list was scrolled when the leaf lifted.
    let scrollOffset: CGFloat
    let headerHeight: CGFloat
    let listInset: CGFloat
    let compact: Bool
    let reduceMotion: Bool
    /// The folder label printed at the top of the half.
    @ViewBuilder let label: Label

    /// A positive turn about the vertical axis brings the left edge
    /// forward: A's leaf turns positive, B's negative, so each free edge
    /// rises toward the viewer rather than sinking behind the page.
    private static var turnSign: Double { 1 }
    /// Upright, the free edge stands nearest and looks about a fifth
    /// taller than the page: depth without spilling far past it.
    private static var perspective: CGFloat { 0.24 }

    private var half: CGFloat { pageSize.width / 2 }

    var body: some View {
        if reduceMotion {
            // No swing: the half fades from where it lay as its back fades
            // in over the other half.
            ZStack(alignment: .topLeading) {
                front
                    .frame(width: half, height: pageSize.height)
                    .opacity(1 - progress)
                    .offset(x: side == .a ? 0 : half)
                Paper(tone: FoldPalette.paperBack)
                    .frame(width: half, height: pageSize.height)
                    .opacity(progress)
                    .offset(x: side == .a ? half : 0)
            }
        } else {
            faces
                .frame(width: half, height: pageSize.height)
                .rotation3DEffect(.degrees(Self.turnSign * (side == .a ? 1 : -1) * progress * 180),
                                  axis: (x: 0, y: 1, z: 0),
                                  anchor: side == .a ? .trailing : .leading,
                                  perspective: Self.perspective)
                .offset(x: side == .a ? 0 : half)
        }
    }

    private var faces: some View {
        let light = sin(progress * .pi)
        return ZStack {
            if progress >= FoldMotion.tippingPoint {
                // Mirrored so it reads the right way round once turned.
                Paper(tone: FoldPalette.paperBack)
                    .overlay(Color.black.opacity(0.14 * light))
                    .scaleEffect(x: -1)
            } else {
                front
                    .overlay(Color.black.opacity(0.2 * light))
            }
        }
        .compositingGroup()
        .shadow(color: .black.opacity(0.3 * light), radius: 12 * light)
    }

    /// The half's folder label and the lines that were in view, frozen
    /// where the list was scrolled.
    private var front: some View {
        let entries = FoldEntry.entries(lines)
        let offset = max(0, scrollOffset - listInset)
        let first = min(entries.count, Int(offset / FoldMetrics.rowHeight))
        let visible = Int(pageSize.height / FoldMetrics.rowHeight) + 2
        let window = Array(entries.dropFirst(first).prefix(visible))
        return ZStack(alignment: .topLeading) {
            Paper()
            VStack(spacing: 0) {
                label
                    .frame(height: headerHeight)
                VStack(spacing: 0) {
                    ForEach(window) { entry in
                        cell(entry)
                            .frame(height: FoldMetrics.rowHeight)
                    }
                }
                .offset(y: CGFloat(first) * FoldMetrics.rowHeight - scrollOffset + listInset)
                .frame(maxHeight: .infinity, alignment: .top)
                .clipped()
            }
            // The leaf reaches the middle of the crease; the half's lines
            // stop at its edge.
            .padding(side == .a ? .trailing : .leading, FoldMetrics.creaseWidth / 2)
        }
    }

    @ViewBuilder
    private func cell(_ entry: FoldEntry) -> some View {
        switch entry {
        case .folder(let path, let count):
            FoldHeadingHalf(path: path, count: count, side: side)
        case .line(let line):
            FoldHalf(line: line, side: side, compact: compact)
        }
    }
}
