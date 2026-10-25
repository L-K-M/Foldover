// One entry of the sheet in three parts: Folder A's half, the crease and
// Folder B's half. The leaf carries a half's part on its own while it
// folds, so each half draws without the others. Names sit against the
// crease on both sides, mirrored, so the two versions of a path face
// each other across the fold; the verb and size sit at the outer edge
// of the half the change arrives in.
//
//   2026/Trip/day2  ·  │  · 3 changes
//       IMG_2041.RAW   →   IMG_2041.RAW ······ copy · 41 MB
//            no copy   →   old-banner.psd ···· archive · 66 MB

import SwiftUI

enum FoldMetrics {
    /// Every entry is one line tall, so the leaf can find the visible
    /// lines from the scroll offset alone.
    static let rowHeight: CGFloat = 30
    /// The crease column the pips sit in.
    static let creaseWidth: CGFloat = 30
    /// A half narrower than this shows impressions as short stubs.
    static let compactHalf: CGFloat = 190
}

/// One half of a line: what that folder holds at the path.
struct FoldHalf: View {
    let line: FoldLine
    let side: FolderSide
    let compact: Bool
    /// How near the folding leaf is, 0…1: impressions take on the
    /// incoming ink as it approaches.
    var peek: Double = 0
    @Environment(\.colorSchemeContrast) private var contrast

    var body: some View {
        HStack(spacing: 8) {
            if side == .a {
                meta
                Spacer(minLength: 4)
                mark
            } else {
                mark
                Spacer(minLength: 4)
                meta
            }
        }
        .padding(.leading, side == .a ? 14 : 6)
        .padding(.trailing, side == .a ? 6 : 14)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    /// The verb and size, on the half the change arrives in.
    @ViewBuilder
    private var meta: some View {
        if side == Self.arrivalSide(line.flow) {
            let text = line.sizeText.isEmpty ? line.verb : "\(line.verb) · \(line.sizeText)"
            Text(text)
                .font(FoldType.meta)
                .foregroundStyle(line.flow == .failed ? FoldPalette.failure : FoldPalette.pencil)
                .lineLimit(1)
                .layoutPriority(-1)
        }
    }

    @ViewBuilder
    private var mark: some View {
        switch line.mark(side) {
        case .ink:
            name.foregroundStyle(FoldPalette.ink(side))
        case .impression:
            impression
        case .lift:
            HStack(spacing: 4) {
                Image(systemName: "archivebox")
                    .font(.system(size: 9, weight: .semibold))
                name.strikethrough(color: FoldPalette.ink(side).opacity(0.6))
            }
            .foregroundStyle(FoldPalette.ink(side).opacity(0.72))
            .offset(y: -1)
        case .retouch:
            HStack(spacing: 4) {
                name.foregroundStyle(FoldPalette.ink(side))
                Image(systemName: "tag.fill")
                    .font(.system(size: 8))
                    .foregroundStyle(FoldPalette.ink(side == .a ? .b : .a))
            }
        case .faint:
            name.foregroundStyle(FoldPalette.pencil.opacity(0.6))
        case .blank:
            absence
        }
    }

    private var name: some View {
        Text(line.name)
            .font(FoldType.name)
            .lineLimit(1)
            .truncationMode(.middle)
    }

    /// Where the other folder's version lands: pressed into the paper,
    /// taking on that folder's ink as the leaf comes over.
    @ViewBuilder
    private var impression: some View {
        let incoming = FoldPalette.ink(side == .a ? .b : .a)
        if compact {
            Capsule()
                .fill(FoldPalette.impression.opacity(0.5))
                .overlay(Capsule().fill(incoming).opacity(peek))
                .frame(width: 26, height: 4)
        } else {
            HStack(spacing: 4) {
                if let from = line.movedFrom {
                    Text(Self.lastComponent(from))
                        .font(.system(size: 11))
                        .strikethrough()
                        .foregroundStyle(FoldPalette.pencil.opacity(0.7))
                        .lineLimit(1)
                    Image(systemName: "arrow.right")
                        .font(.system(size: 8, weight: .semibold))
                        .foregroundStyle(FoldPalette.pencil)
                }
                name
                    .impression(contrast)
                    .overlay { name.foregroundStyle(incoming).opacity(peek * 0.85) }
            }
        }
    }

    /// An archive's other half: say there is no copy rather than leave a
    /// hole that reads as "nothing happens here".
    @ViewBuilder
    private var absence: some View {
        let other = line.mark(side == .a ? .b : .a)
        if other == .lift || line.flow == .still {
            Text("no copy")
                .font(FoldType.meta)
                .foregroundStyle(FoldPalette.pencil.opacity(0.8))
                .padding(.horizontal, 6)
                .padding(.vertical, 1)
                .overlay(RoundedRectangle(cornerRadius: 3)
                    .strokeBorder(FoldPalette.pencil.opacity(0.45), style: StrokeStyle(lineWidth: 0.75, dash: [2, 2])))
        } else if line.flow == .failed, side == .a {
            HStack(spacing: 4) {
                Image(systemName: "xmark.octagon.fill")
                    .font(.system(size: 10))
                name
            }
            .foregroundStyle(FoldPalette.failure)
        }
    }

    /// The half whose edge names the change: where it arrives, or the
    /// right-hand half when nothing moves.
    static func arrivalSide(_ flow: FoldFlow) -> FolderSide {
        flow == .toA ? .a : .b
    }

    private static func lastComponent(_ path: String) -> String {
        path.split(separator: "/").last.map(String.init) ?? path
    }
}

/// A folder heading's half: the folder on A's side, its count on B's.
struct FoldHeadingHalf: View {
    let path: String
    let count: Int
    let side: FolderSide

    var body: some View {
        HStack {
            if side == .a {
                Spacer(minLength: 4)
                Text(path)
                    .font(FoldType.heading)
                    .lineLimit(1)
                    .truncationMode(.head)
            } else {
                Text(count == 1 ? String(localized: "1 change") : String(localized: "\(count) changes"))
                    .font(FoldType.meta)
                Spacer(minLength: 4)
            }
        }
        .foregroundStyle(FoldPalette.pencil)
        .padding(.leading, side == .a ? 14 : 6)
        .padding(.trailing, side == .a ? 6 : 14)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(.top, 6)
    }
}

/// The pip on the crease: which way the line flows, or the pin that
/// holds a conflict. A staged turn shows its new direction in outline
/// until Return runs it.
struct FoldPip: View {
    let flow: FoldFlow
    var staged: FoldFlow?

    var body: some View {
        let shown = staged ?? flow
        Group {
            switch shown {
            case .toB: arrow("arrow.right", ink: FoldPalette.ink(.a))
            case .toA: arrow("arrow.left", ink: FoldPalette.ink(.b))
            case .held: Pin()
            case .waiting:
                Image(systemName: "hourglass")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(FoldPalette.pencil)
            case .still:
                Image(systemName: "equal")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(FoldPalette.pencil)
            case .failed:
                Image(systemName: "xmark")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(FoldPalette.failure)
            }
        }
        .frame(width: 18, height: 18)
    }

    private func arrow(_ symbol: String, ink: Color) -> some View {
        Image(systemName: symbol)
            .font(.system(size: 8.5, weight: .heavy))
            .foregroundStyle(ink)
            .frame(width: 16, height: 16)
            .background(Circle().fill(FoldPalette.paper))
            .overlay {
                Circle().strokeBorder(ink.opacity(staged == nil ? 0.35 : 0.9),
                                      style: StrokeStyle(lineWidth: staged == nil ? 0.75 : 1.25,
                                                         dash: staged == nil ? [] : [2.5, 1.5]))
            }
    }
}
