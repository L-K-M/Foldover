// One entry of the sheet as two pages and the gutter between them, as in
// a book's spread. Each page reads left to right and is a whole reading
// of its folder at the path, so the leaf can carry one page on its own
// while it folds. The page that changes carries a mark in its outer
// margin, the verb and size end Folder B's page, and nothing is printed
// in the gutter.
//
//   ┌─ Folder A ────────────────┐   ┌─ Folder B ─────────────────────┐
//   │ ▢ 2026/Trip/day2          │ ┆ │ 2026/Trip/day2       3 changes │
//   │   IMG_2041.RAW            │ ┆ │ IMG_2041.RAW    copy · 41 MB → │
//   │ ← notes.md                │ ┆ │ notes.md        copy · 12 KB   │
//   │   no copy                 │ ┆ │ old-banner.psd       archive ▤ │
//   └───────────────────────────┘   └────────────────────────────────┘

import SwiftUI

enum FoldMetrics {
    /// Every entry is one line tall, so the leaf can find the visible
    /// lines from the scroll offset alone.
    static let rowHeight: CGFloat = 26
    /// The gutter down the middle of the sheet: only the crease is drawn
    /// in it.
    static let creaseWidth: CGFloat = 32
    /// A page's margin beside the gutter.
    static let innerMargin: CGFloat = 8
    /// The outer margins, beyond the marks. Folder B's is wider: the
    /// list's overlay scroller passes over it.
    static let outerMarginA: CGFloat = 12
    static let outerMarginB: CGFloat = 18
    /// The mark column and the space between it and the words.
    static let markWidth: CGFloat = 18
    static let markGap: CGFloat = 6
}

/// A mark a click can turn: it stages the line's next way, as ← and → do.
struct FoldTurnControl {
    let enabled: Bool
    /// The ways the line can turn, by name.
    let help: String
    let cycle: @MainActor () -> Void
}

/// One page of a line: what that folder holds at the path, and what
/// happens to it there.
struct FoldHalf: View {
    let line: FoldLine
    let side: FolderSide
    /// How near the folding leaf is, 0…1: arriving copies fill with the
    /// incoming ink as it approaches.
    var peek: Double = 0
    /// A turn ← or → staged on the line, marked where it would land.
    var staged: FoldFlow?
    /// Refused turns so far: each one shakes the mark.
    var shakes = 0
    /// What a click on the mark does; without it the mark is only print.
    var turn: FoldTurnControl?
    @Environment(\.colorSchemeContrast) private var contrast
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        HStack(spacing: 0) {
            if side == .a {
                margin
                    .padding(.trailing, FoldMetrics.markGap)
                name
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                HStack(spacing: 8) {
                    name
                    Spacer(minLength: 0)
                    note
                }
                .frame(maxWidth: .infinity)
                margin
                    .padding(.leading, FoldMetrics.markGap)
            }
        }
        .padding(.leading, side == .a ? FoldMetrics.outerMarginA : FoldMetrics.innerMargin)
        .padding(.trailing, side == .a ? FoldMetrics.innerMargin : FoldMetrics.outerMarginB)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    /// The outer margin's mark: a button while a click can turn the line.
    private var margin: some View {
        let badge = line.badge(side, staged: staged)
        return Group {
            if let turn, badge != nil {
                Button(action: turn.cycle) {
                    FoldBadgeView(badge: badge, side: side, outlined: staged != nil)
                }
                .buttonStyle(.plain)
                // Return belongs to the staged turn, not to a focused mark.
                .focusable(false)
                .disabled(!turn.enabled)
                .help(turn.help)
                .accessibilityHidden(true)
            } else {
                FoldBadgeView(badge: badge, side: side, outlined: staged != nil)
            }
        }
        .frame(width: FoldMetrics.markWidth)
        .modifier(Shake(count: shakes))
    }

    /// The line's verb and size, at the end of Folder B's page, or what
    /// went wrong, in full on hover.
    private var note: some View {
        Group {
            if line.flow == .failed {
                Text(line.actionText)
                    .help(line.actionText)
            } else {
                ViewThatFits(in: .horizontal) {
                    if !line.sizeText.isEmpty { Text("\(line.verb) · \(line.sizeText)") }
                    Text(line.verb)
                    Color.clear.frame(width: 0, height: 0)
                }
            }
        }
        .font(FoldType.meta)
        .foregroundStyle(line.flow == .failed ? FoldPalette.failure : FoldPalette.umber)
        .lineLimit(1)
        .layoutPriority(-1)
    }

    @ViewBuilder
    private var name: some View {
        switch line.mark(side) {
        case .ink, .retouch:
            nameText.foregroundStyle(FoldPalette.ink(side))
        case .impression:
            impression
        case .lift:
            nameText
                .strikethrough(color: FoldPalette.ink(side).opacity(0.6))
                .foregroundStyle(FoldPalette.ink(side).opacity(0.72))
        case .faint:
            nameText.foregroundStyle(FoldPalette.umber.opacity(0.6))
        case .blank:
            absence
        }
    }

    private var nameText: some View {
        Text(line.name)
            .font(FoldType.name)
            .lineLimit(1)
            .truncationMode(.middle)
    }

    /// Where the other folder's version lands: its name in that folder's
    /// ink, lighter and thinner until the leaf comes over and fills it.
    @ViewBuilder
    private var impression: some View {
        let from: FolderSide = side == .a ? .b : .a
        let faint = arriving(from)
        let arrivingName = Text(line.name)
            .font(FoldType.arrivingName)
            .foregroundStyle(FoldPalette.ink(from).opacity(faint + (1 - faint) * peek))
        if let origin = line.movedFrom {
            // A rename shows the name it had while the page has room.
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 4) {
                    Text(Self.lastComponent(origin))
                        .font(.system(size: 11))
                        .strikethrough()
                        .foregroundStyle(FoldPalette.umber.opacity(0.7))
                    Image(systemName: "arrow.right")
                        .font(.system(size: 8, weight: .semibold))
                        .foregroundStyle(FoldPalette.umber)
                    arrivingName
                }
                .lineLimit(1)
                arrivingName
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        } else {
            arrivingName
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }

    /// An archive's other half: say there is no copy rather than leave a
    /// hole that reads as "nothing happens here". A failure names its
    /// item once, on Folder A's page.
    @ViewBuilder
    private var absence: some View {
        if line.mark(side == .a ? .b : .a) == .lift {
            Text("no copy")
                .font(FoldType.meta)
                .foregroundStyle(FoldPalette.umber.opacity(0.8))
                .padding(.horizontal, 6)
                .padding(.vertical, 1)
                .overlay(RoundedRectangle(cornerRadius: 3)
                    .strokeBorder(FoldPalette.umber.opacity(0.45), style: StrokeStyle(lineWidth: 0.75, dash: [2, 2])))
        } else if line.flow == .failed, side == .a {
            nameText.foregroundStyle(FoldPalette.failure)
        }
    }

    /// How much ink an arriving copy shows before the leaf comes over:
    /// at least 3:1 on either half, below the folder's own version and
    /// above a line that waits. Increase Contrast prints it near full.
    private func arriving(_ from: FolderSide) -> Double {
        if contrast == .increased { return 0.85 }
        switch (from, scheme) {
        case (.a, .dark): return 0.55
        case (.b, .dark): return 0.6
        case (.a, _): return 0.6
        case (.b, _): return 0.75
        }
    }

    private static func lastComponent(_ path: String) -> String {
        path.split(separator: "/").last.map(String.init) ?? path
    }
}

/// A folder heading on one page: the folder on both, its count at the
/// end of Folder B's.
struct FoldHeadingHalf: View {
    let path: String
    let count: Int
    let side: FolderSide

    /// The count as the heading says it, e.g. "3 changes".
    static func countText(_ count: Int) -> String {
        count == 1 ? String(localized: "1 change") : String(localized: "\(count) changes")
    }

    var body: some View {
        HStack(spacing: 0) {
            if side == .a {
                Image(systemName: "folder")
                    .font(.system(size: 9))
                    .foregroundStyle(FoldPalette.umber.opacity(0.7))
                    .frame(width: FoldMetrics.markWidth)
                    .padding(.trailing, FoldMetrics.markGap)
            }
            Text(path)
                .font(FoldType.heading)
                .lineLimit(1)
                .truncationMode(.head)
            Spacer(minLength: 8)
            if side == .b {
                // The count before the path's head; a bare number on a
                // narrow page.
                ViewThatFits(in: .horizontal) {
                    Text(Self.countText(count))
                    Text(count, format: .number)
                }
                .font(FoldType.meta)
                .lineLimit(1)
                .layoutPriority(1)
                // In line with the notes under it, clear of the marks.
                Color.clear
                    .frame(width: FoldMetrics.markWidth + FoldMetrics.markGap, height: 0)
            }
        }
        .foregroundStyle(FoldPalette.umber)
        .padding(.leading, side == .a ? FoldMetrics.outerMarginA : FoldMetrics.innerMargin)
        .padding(.trailing, side == .a ? FoldMetrics.innerMargin : FoldMetrics.outerMarginB)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(.top, 4)
    }
}

/// A page's margin mark: a disc in the arriving version's ink with the
/// way it travels, the pin that holds a conflict, or what else happens
/// to the folder there. A staged turn shows its disc in outline until
/// Return runs it.
struct FoldBadgeView: View {
    let badge: FoldBadge?
    /// The page the mark is on.
    let side: FolderSide
    var outlined = false

    var body: some View {
        Group {
            if let badge {
                mark(badge)
            } else {
                Color.clear
            }
        }
        .frame(width: 18, height: 18)
    }

    @ViewBuilder
    private func mark(_ badge: FoldBadge) -> some View {
        switch badge {
        case .arrives(let flow):
            disc(flow == .toA ? "arrow.left" : "arrow.right", ink: FoldPalette.ink(FoldLine.keptSide(flow) ?? side))
        case .retouch:
            // The other folder's metadata, in its ink.
            Image(systemName: "tag.fill")
                .font(.system(size: 8))
                .foregroundStyle(FoldPalette.ink(side == .a ? .b : .a))
        case .lift:
            Image(systemName: "archivebox")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(FoldPalette.ink(side).opacity(0.72))
        case .held:
            Pin()
        case .waiting:
            Image(systemName: "hourglass")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(FoldPalette.umber)
        case .failed:
            Image(systemName: "xmark.octagon.fill")
                .font(.system(size: 10))
                .foregroundStyle(FoldPalette.failure)
        }
    }

    private func disc(_ symbol: String, ink: Color) -> some View {
        Image(systemName: symbol)
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
