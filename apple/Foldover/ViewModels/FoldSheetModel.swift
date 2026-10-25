// The fold (PLAN.md §3.8, "the preview is the product"): a preview laid
// out as one sheet of paper creased between the pair's two folders.
// Every change is a line across the crease. Each half shows what its
// folder holds at that path, and the crease shows which way the sync
// carries it:
//
//   Folder A                  crease                  Folder B
//   IMG_2041.RAW    ink         →        impression   IMG_2041.RAW
//   notes.md        impression  ←        ink          notes.md
//   header.psd      ink         ⚑        ink          header.psd
//                   blank       →        lift         old-banner.psd
//
// Syncing folds the source half over the other, the way a monotype
// print transfers wet ink. When the run ends the leaf opens on an
// imprint of what the run reports it did: a routine sync plans afresh,
// so the previewed lines are a forecast and are never shown as done.
// This file holds the pure rules: which mark each half carries, what
// each line is called, which ways a person may turn it, and which half
// folds.

import Foundation

/// How one half of a line reads.
enum FoldMark: Equatable, Sendable {
    /// Nothing at this path in this folder, now or after the sync.
    case blank
    /// The item as this folder holds it, printed in the folder's ink.
    case ink
    /// Where the other folder's copy lands: its name, faint until the
    /// fold fills it with that folder's ink.
    case impression
    /// An item the sync takes out of this folder into its archive.
    case lift
    /// The item stays; only its metadata (dates, tags, permissions)
    /// changes.
    case retouch
    /// Printed faintly: it waits for a conflict or stays untouched.
    case faint
}

/// Which way a line crosses the crease.
enum FoldFlow: Equatable, Sendable {
    /// Folder A's version, or its absence, carries over to Folder B.
    case toB
    case toA
    /// A conflict pins the line: nothing moves until a person chooses.
    case held
    /// Waits for a conflict in an enclosing folder.
    case waiting
    /// Left as it is.
    case still
    /// Could not be checked, or failed in the last run.
    case failed
}

/// One change as a line across the sheet.
struct FoldLine: Identifiable, Equatable {
    /// The change row's ID, so selection stays the main window's.
    let id: ChangeRow.ID
    let path: String
    /// The last path component, e.g. "IMG_2041.RAW".
    let name: String
    /// The enclosing folders, e.g. "2026/Trip/day2"; empty at the root.
    let parent: String
    let a: FoldMark
    let b: FoldMark
    let flow: FoldFlow
    /// For a move, the name it had; the line shows it struck through.
    let movedFrom: String?
    let sizeText: String
    let actionText: String
    /// What the change is, in a word or two, e.g. "copy", "both changed".
    let verb: String
    /// The commands that turn the line another way, by the flow they
    /// give it, e.g. `.toA: "Archive in Folder A"`. ← and → stage one;
    /// Return runs it.
    let turns: [FoldFlow: String]

    init(row: ChangeRow) {
        id = row.id
        path = row.path
        let split = Self.split(row.path)
        name = split.name
        parent = split.parent
        movedFrom = row.movedFrom
        sizeText = row.sizeText
        actionText = row.actionText
        guard let action = row.action else {
            a = .blank
            b = .blank
            flow = .failed
            verb = String(localized: "failed")
            turns = [:]
            return
        }
        (a, b, flow) = Self.marks(action)
        verb = Self.verb(action)
        turns = Self.turns(row)
    }

    /// The flows the line can turn to, → before ←, as the pip offers them.
    var turnFlows: [FoldFlow] { [FoldFlow.toB, .toA].filter { turns[$0] != nil } }

    /// The mark on `side`'s half.
    func mark(_ side: FolderSide) -> FoldMark { side == .a ? a : b }

    /// The folder whose version a turn to `flow` keeps: carrying A over
    /// to B keeps A's.
    static func keptSide(_ flow: FoldFlow) -> FolderSide? {
        switch flow {
        case .toB: .a
        case .toA: .b
        case .held, .waiting, .still, .failed: nil
        }
    }

    private static func split(_ path: String) -> (name: String, parent: String) {
        guard let slash = path.lastIndex(of: "/") else { return (path, "") }
        return (String(path[path.index(after: slash)...]), String(path[..<slash]))
    }

    private static func marks(_ action: ChangeAction) -> (FoldMark, FoldMark, FoldFlow) {
        switch action {
        case .copy(to: .b): (.ink, .impression, .toB)
        case .copy(to: .a): (.impression, .ink, .toA)
        case .update(to: .b): (.ink, .retouch, .toB)
        case .update(to: .a): (.retouch, .ink, .toA)
        // The other folder no longer has it, and that absence carries over.
        case .archive(on: .b): (.blank, .lift, .toB)
        case .archive(on: .a): (.lift, .blank, .toA)
        // The other folder renamed it; the rename carries over, no data.
        case .move(on: .b): (.ink, .impression, .toB)
        case .move(on: .a): (.impression, .ink, .toA)
        case .conflict: (.ink, .ink, .held)
        case .blocked: (.faint, .faint, .waiting)
        // Which folder holds a kept item is not said; claim neither.
        case .keep: (.faint, .faint, .still)
        }
    }

    private static func verb(_ action: ChangeAction) -> String {
        switch action {
        case .copy: String(localized: "copy")
        case .update: String(localized: "metadata")
        case .archive: String(localized: "archive")
        case .move: String(localized: "rename")
        case .conflict(.bothChanged): String(localized: "both changed")
        case .conflict(.deletedVersusEdited): String(localized: "deleted vs. changed")
        case .conflict(.typeMismatch): String(localized: "file vs. folder")
        case .conflict(.metadataOnly): String(localized: "metadata differs")
        case .conflict(.nameClash): String(localized: "name clash")
        case .blocked: String(localized: "waiting")
        case .keep: String(localized: "kept")
        }
    }

    /// A resolvable conflict turns either way; another steerable change
    /// turns toward the folder it would have changed, under the name the
    /// context menu gives that command.
    private static func turns(_ row: ChangeRow) -> [FoldFlow: String] {
        if row.resolvable != nil {
            return [.toB: String(localized: "Keep Folder A’s Version"),
                    .toA: String(localized: "Keep Folder B’s Version")]
        }
        guard let side = row.otherWay, let title = row.otherWayTitle else { return [:] }
        return [side == .a ? .toB : .toA: title]
    }
}

/// What the sheet lists: lines grouped under the folder that holds
/// them, every entry one line tall so scrolling stays exact.
enum FoldEntry: Identifiable, Equatable {
    /// A folder heading over its changes, e.g. "2026/Trip/day2 · 3".
    case folder(path: String, count: Int)
    case line(FoldLine)

    /// By path, not row: a new plan renumbers its rows, and a line that
    /// keeps its path must stay the same line on screen.
    var id: String {
        switch self {
        case .folder(let path, _): "folder:\(path)"
        case .line(let line) where line.flow == .failed: "failed:\(line.path)"
        case .line(let line): "line:\(line.path)"
        }
    }

    /// Lines grouped by their folder, folders in the order their first
    /// line arrived. Path order puts a subfolder between its parent's
    /// files ("Website/index.html", "Website/old/a", "Website/style.css");
    /// grouping keeps each folder's lines under one heading. Lines at
    /// the root have no heading.
    static func entries(_ lines: [FoldLine]) -> [FoldEntry] {
        var folders: [String] = []
        var grouped: [String: [FoldLine]] = [:]
        for line in lines {
            if grouped[line.parent] == nil { folders.append(line.parent) }
            grouped[line.parent, default: []].append(line)
        }
        return folders.flatMap { parent -> [FoldEntry] in
            let lines = grouped[parent] ?? []
            let heading: [FoldEntry] = parent.isEmpty ? [] : [.folder(path: parent, count: lines.count)]
            return heading + lines.map(FoldEntry.line)
        }
    }
}

/// A plan as the sheet last showed it. The sheet keeps it while a run
/// presses it, so the leaf has lines to fold; it is never read as what
/// the run did.
struct FoldPlan: Equatable {
    let token: String
    let lines: [FoldLine]
}

/// What the leaf leaves on the sheet when it opens: the run's own
/// counts, on a card set across the crease. Undo lifts it.
struct FoldImprint: Equatable {
    /// One count, e.g. 5 "copied".
    struct Figure: Equatable {
        let value: String
        let label: String
    }

    let title: String
    let figures: [Figure]
    /// What the run left for a person, e.g. "3 conflicts left as they were".
    let notes: [String]

    init(result: RunSummary) {
        title = result.cancelled ? String(localized: "Stopped Partway") : String(localized: "Folded")
        figures = Self.figures([
            (result.copied, String(localized: "copied")),
            (result.updated, String(localized: "updated")),
            (result.archived, String(localized: "archived")),
            (result.moved, String(localized: "renamed")),
        ], none: String(localized: "changes"))
        var notes: [String] = []
        if result.conflicts > 0 {
            notes.append(String(localized: "\(result.conflicts) conflicts left as they were"))
        }
        if result.failureCount > 0 {
            notes.append(String(localized: "\(result.failureCount) items failed; the next sync retries them"))
        }
        if result.recovered > 0 {
            notes.append(String(localized: "\(result.recovered) items recovered from an interrupted sync"))
        }
        self.notes = notes
    }

    init(undo: UndoSummary) {
        title = undo.cancelled ? String(localized: "Unfolded Partway") : String(localized: "Unfolded")
        figures = Self.figures([
            (undo.restored, String(localized: "put back")),
            (undo.removed, String(localized: "archived")),
            (undo.beside, String(localized: "beside newer")),
        ], none: String(localized: "changes"))
        var notes: [String] = []
        if undo.notUndoneCount > 0 {
            notes.append(String(localized: "\(undo.notUndoneCount) items left as they are"))
        }
        notes.append(String(localized: "Automatic syncs wait until you sync again"))
        self.notes = notes
    }

    /// The non-zero counts; a run that changed nothing reads "0 changes".
    private static func figures(_ counts: [(UInt64, String)], none: String) -> [Figure] {
        let figures = counts.filter { $0.0 > 0 }.map { Figure(value: "\($0.0)", label: $0.1) }
        return figures.isEmpty ? [Figure(value: "0", label: none)] : figures
    }
}

/// Which half folds over the other.
enum FoldLeaf {
    /// The halves a person may fold: a one-way mode's source, or either
    /// half of a two-way sync, where both inks transfer.
    static func sides(_ mode: PairMode) -> [FolderSide] {
        switch mode {
        case .sync: [.a, .b]
        case .mirrorLeft, .backupLeft: [.a]
        case .mirrorRight, .backupRight: [.b]
        }
    }

    /// The half that folds when a run starts without a gesture (⌘S, the
    /// menu bar, Shortcuts): a one-way mode's source; in a two-way sync,
    /// the half with more changes to carry over, Folder A on a tie.
    static func automatic(_ mode: PairMode, lines: [FoldLine]) -> FolderSide {
        let sides = sides(mode)
        guard sides.count > 1 else { return sides.first ?? .a }
        let toA = lines.count { $0.flow == .toA }
        let toB = lines.count { $0.flow == .toB }
        return toA > toB ? .b : .a
    }
}

/// How the sheet moves: the leaf's swing while a person folds it, and
/// the springs it settles with.
enum FoldMotion {
    /// A press moves nothing until the pointer travels this far, so a
    /// stray click never starts a fold.
    static let deadZone: Double = 8
    /// Past upright, the leaf falls onto the other half and the sync
    /// starts; short of it, the leaf falls back open.
    static let tippingPoint: Double = 0.5
    /// Folding with no plan to press resists, then springs back.
    static let resistance: Double = 0.07

    /// How far the leaf has swung, 0 open to 1 folded flat, for a drag
    /// of `travel` points toward the crease on a half `width` wide.
    static func swing(travel: Double, width: Double) -> Double {
        guard width > 0, travel > deadZone else { return 0 }
        return min(1, (travel - deadZone) / width)
    }

    /// Whether letting go of the leaf here folds it: only past upright.
    /// No flick: a fold commits a sync, so it takes the full travel.
    static func commits(swing: Double) -> Bool { swing >= tippingPoint }
}
