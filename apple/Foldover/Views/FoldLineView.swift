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
    /// How near the folding leaf is, 0…1: arriving copies fill with the
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
            // With no room even for the verb, the line drops it rather
            // than show a stray letter; VoiceOver and the inspector say it.
            ViewThatFits(in: .horizontal) {
                if !line.sizeText.isEmpty { Text("\(line.verb) · \(line.sizeText)") }
                Text(line.verb)
                Color.clear.frame(width: 0, height: 0)
            }
            .font(FoldType.meta)
            .foregroundStyle(line.flow == .failed ? FoldPalette.failure : FoldPalette.slate)
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
            name.foregroundStyle(FoldPalette.slate.opacity(0.6))
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

    /// Where the other folder's version lands: its name in that folder's
    /// ink, faint until the leaf comes over and fills it. Increase
    /// Contrast prints it darker from the start.
    @ViewBuilder
    private var impression: some View {
        let incoming = FoldPalette.ink(side == .a ? .b : .a)
        let faint = contrast == .increased ? Self.arrivingContrast : Self.arriving
        let ink = faint + (1 - faint) * peek
        if compact {
            Capsule()
                .fill(incoming.opacity(ink))
                .frame(width: 26, height: 4)
        } else {
            HStack(spacing: 4) {
                if let from = line.movedFrom {
                    Text(Self.lastComponent(from))
                        .font(.system(size: 11))
                        .strikethrough()
                        .foregroundStyle(FoldPalette.slate.opacity(0.7))
                        .lineLimit(1)
                    Image(systemName: "arrow.right")
                        .font(.system(size: 8, weight: .semibold))
                        .foregroundStyle(FoldPalette.slate)
                }
                name.foregroundStyle(incoming.opacity(ink))
            }
        }
    }

    /// An archive's other half: say there is no copy rather than leave a
    /// hole that reads as "nothing happens here".
    @ViewBuilder
    private var absence: some View {
        if line.mark(side == .a ? .b : .a) == .lift {
            Text("no copy")
                .font(FoldType.meta)
                .foregroundStyle(FoldPalette.slate.opacity(0.8))
                .padding(.horizontal, 6)
                .padding(.vertical, 1)
                .overlay(RoundedRectangle(cornerRadius: 3)
                    .strokeBorder(FoldPalette.slate.opacity(0.45), style: StrokeStyle(lineWidth: 0.75, dash: [2, 2])))
        } else if line.flow == .failed, side == .a {
            HStack(spacing: 4) {
                Image(systemName: "xmark.octagon.fill")
                    .font(.system(size: 10))
                name
            }
            .foregroundStyle(FoldPalette.failure)
        }
    }

    /// How much ink an arriving copy shows before the leaf comes over.
    private static let arriving = 0.38
    private static let arrivingContrast = 0.7

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
        .foregroundStyle(FoldPalette.slate)
        .padding(.leading, side == .a ? 14 : 6)
        .padding(.trailing, side == .a ? 6 : 14)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(.top, 6)
    }
}

/// The pip on the crease: a disc in the source folder's ink with the
/// way the line flows, or the pin that holds a conflict. A staged turn
/// shows its new direction in outline until Return runs it.
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
                    .foregroundStyle(FoldPalette.slate)
            case .still:
                Image(systemName: "equal")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(FoldPalette.slate)
            case .failed:
                Image(systemName: "xmark")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(FoldPalette.failure)
            }
        }
        .frame(width: 18, height: 18)
    }

    private func arrow(_ symbol: String, ink: Color) -> some View {
        let outlined = staged != nil
        return Image(systemName: symbol)
            .font(.system(size: 8, weight: .heavy))
            .foregroundStyle(outlined ? ink : FoldPalette.paper)
            .frame(width: 16, height: 16)
            .background(Circle().fill(outlined ? FoldPalette.paper : ink))
            .overlay {
                if outlined {
                    Circle().strokeBorder(ink, style: StrokeStyle(lineWidth: 1.25, dash: [2.5, 1.5]))
                }
            }
    }
}
