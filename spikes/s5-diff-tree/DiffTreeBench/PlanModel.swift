// The shared synthetic plan tree (ADR docs/adr/0003-diff-tree-view.md):
// a deterministic 200,000-row folder plan, delivered to the model in
// pre-order batches like a streaming scan. Both tree implementations
// read this one model — the NSOutlineView through its data source, the
// SwiftUI `List(children:)` through its children key path.

import Foundation
import Observation

/// One plan row as both lists see it. A folder's `children` grows as
/// batches land; `nil` means leaf (a file — the distinction
/// `List(children:)` keys its disclosure on). Observed because the
/// SwiftUI list re-renders the subtree that reads it.
@MainActor
@Observable
final class PlanNode: Identifiable {
    /// The node's stable identity — its pre-order index, so a node's
    /// `id` is also its flattened row rank when all folders expand.
    let nodeID: Int

    /// Nonisolated so the `Identifiable` conformance doesn't cross
    /// into main-actor code; `nodeID` is an immutable `Int`.
    nonisolated var id: Int { nodeID }

    let name: String
    let isFolder: Bool
    let action: PlanAction
    let sizeBytes: Int64
    let daysAgo: Int

    /// Filled by the node's delivery — the presentation is shared, so
    /// the strings are produced once by `RowText` rather than per view.
    let sizeText: String
    let dateText: String
    let accessibilityText: String

    /// `nil` for files; a folder is a branch even while empty.
    var children: [PlanNode]?

    /// The detach/reattach slot for the model-driven collapse the
    /// SwiftUI list needs — `List(children:)` has no expansion API, so
    /// "collapse all" detaches every folder's children instead.
    @ObservationIgnored
    var stashedChildren: [PlanNode]?

    @ObservationIgnored
    weak var parent: PlanNode?

    /// Full paths keep same-named files distinguishable in the flat fallback.
    var relativePath: String {
        var names = [name]
        var ancestor = parent
        while let node = ancestor {
            names.append(node.name)
            ancestor = node.parent
        }
        return names.reversed().joined(separator: "/")
    }

    init(row: PreparedRow) {
        let record = row.record
        nodeID = record.id
        name = record.name
        isFolder = record.isFolder
        action = record.action
        sizeBytes = record.sizeBytes
        daysAgo = record.daysAgo
        children = record.isFolder ? [] : nil
        sizeText = row.sizeText
        dateText = row.dateText
        accessibilityText = "\(record.action.accessibilityVerb) "
            + "\(record.name), \(sizeText), modified \(dateText)"
    }
}

/// What one delivered batch changed, grouped per parent in stream
/// order: the AppKit data source turns each entry into one
/// `insertItems` call. A batch boundary splitting a parent's children
/// lands them in a later entry of the next batch — indexes always run
/// from the parent's current child count, so both sides agree.
struct BatchJournal {
    struct Entry {
        /// The receiving parent; `nil` is the top level.
        var parent: PlanNode?
        /// The inserted positions among the parent's children.
        var indexes: IndexSet
        /// The inserted nodes, in order.
        var nodes: [PlanNode]
    }

    var entries: [Entry] = []
    /// Every folder this batch created — the AppKit list expands each
    /// so the streamed preview reveals the tree as it grows.
    var newFolders: [PlanNode] = []
}

/// The model both implementations sit on: the row index, the per-batch
/// delivery that attaches children under their parents, and the
/// model-driven collapse the SwiftUI side's expand/collapse phase uses.
@MainActor
@Observable
final class PlanModel {
    /// The top-level rows — `List(data, children:)`'s data argument and
    /// the outline view's `nil`-parent children.
    private(set) var topNodes: [PlanNode] = []

    /// Rows delivered so far — the stream counter the UI would show.
    private(set) var deliveredCount = 0

    /// Set once streaming finishes; views don't read it, the harness does.
    private(set) var streamComplete = false

    /// Every folder, in delivery order — the collapse/expand pair's
    /// work list.
    private(set) var folders: [PlanNode] = []

    /// id → node; `id` is the pre-order index, so a flat array is the
    /// fastest possible lookup and its size is known up front.
    private var nodesByID: [PlanNode?]

    /// The AppKit sink: the outline coordinator applies each journal as
    /// row inserts. Unset for the SwiftUI run, which needs nothing
    /// imperative — the list observes `children`.
    @ObservationIgnored
    var onBatch: (@MainActor (BatchJournal) -> Void)?

    init(expectedCount: Int) {
        nodesByID = [PlanNode?](repeating: nil, count: expectedCount)
    }

    /// Attach one batch of pre-order records. Contiguous children of the
    /// same parent become one journal entry; a parent's children can
    /// span batches, in which case each batch contributes its own entry.
    func deliver(_ batch: [PreparedRow]) {
        var journal = BatchJournal()
        var i = batch.startIndex
        while i < batch.endIndex {
            let parentID = batch[i].record.parentID
            var children: [PlanNode] = []
            while i < batch.endIndex, batch[i].record.parentID == parentID {
                let record = batch[i].record
                let node = PlanNode(row: batch[i])
                nodesByID[record.id] = node
                if node.isFolder {
                    folders.append(node)
                    journal.newFolders.append(node)
                }
                children.append(node)
                i += 1
            }
            let parent = parentID == -1 ? nil : nodesByID[parentID]
            let start = parent.flatMap { $0.children?.count } ?? topNodes.count
            if let parent {
                parent.children = (parent.children ?? []) + children
                for child in children {
                    child.parent = parent
                }
            } else {
                topNodes += children
            }
            journal.entries.append(BatchJournal.Entry(
                parent: parent,
                indexes: IndexSet(integersIn: start ..< start + children.count),
                nodes: children
            ))
        }
        deliveredCount += batch.count
        // Re-assigning `topNodes` (same contents, new array value) forces
        // the observed-property notification that makes `List(data,
        // children:)` re-evaluate its data argument each batch.
        topNodes = Array(topNodes)
        onBatch?(journal)
    }

    /// The last batch landed.
    func markComplete() {
        streamComplete = true
    }

    /// `node(id:)` — the delivered node for a pre-order index.
    func node(id: Int) -> PlanNode? {
        guard id >= 0, id < nodesByID.count else { return nil }
        return nodesByID[id]
    }

    /// The model-driven "collapse all": every folder keeps its identity
    /// but shows no children — the only whole-tree collapse a
    /// `List(children:)` can be driven through, since SwiftUI offers no
    /// expansion API. Detached children stay alive in `stashedChildren`.
    func collapseAllChildren() {
        for folder in folders where folder.stashedChildren == nil {
            folder.stashedChildren = folder.children
            folder.children = []
        }
        topNodes = Array(topNodes)
    }

    /// Re-attach what `collapseAllChildren` detached.
    func expandAllChildren() {
        for folder in folders {
            guard let stashed = folder.stashedChildren else { continue }
            folder.children = stashed
            folder.stashedChildren = nil
        }
        topNodes = Array(topNodes)
    }
}

/// Row geometry both lists share.
enum DiffTreeMetrics {
    /// One line per plan row — the preview's dense look.
    static let pitch: CGFloat = 24
    /// The fixed window content size every run measures against.
    static let contentSize = NSSize(width: 960, height: 640)
}

/// The deterministic synthetic plan — exactly `rowCount` nodes, all
/// leaves at depths 3 or 5, laid out so the count closes:
///   depth 1: 16 folders
///   depth 2: 10 folders each (160), plus 25 files directly (400)
///   depth 3: 10 folders each (1,600), plus 15 files directly (2,400)
///   depth 4: 10 folders each (16,000), plus 5 files directly (8,000)
///   depth 5: 11 or 10 files per depth-4 folder (171,424)
/// 17,776 folders + 182,224 files = 200,000.
enum PlanGenerator {
    static let rowCount = 200_000

    /// File names rotate through a realistic mix.
    private static let fileStems = [
        "IMG_%05d.RAW", "clip-%05d.mov", "scan-%05d.tiff",
        "notes-%05d.md", "meta-%05d.json",
    ]

    /// The action mix: mostly copies, deletes every 13th, conflicts rare.
    private static func action(for id: Int) -> PlanAction {
        if id % 211 == 0 { return .conflict }
        if id % 13 == 0 { return .archive }
        if id % 7 == 0 { return .copyToMac }
        return .copyToTarget
    }

    /// A deterministic 1 KB–900 MB spread (a Knuth multiplier scramble,
    /// not seeded randomness — every run makes the same plan).
    private static func size(for id: Int) -> Int64 {
        Int64((id &* 2_654_435_761) % 900_000_000) + 1_024
    }

    /// Emit the whole plan as a pre-order record list; `parentID` names
    /// the already-emitted parent's `id` (−1 for top level).
    static func makePlan() -> [RowRecord] {
        var records: [RowRecord] = []
        records.reserveCapacity(rowCount)
        var nextID = 0
        var depth4Ordinal = 0

        func file(parentID: Int) {
            let stem = fileStems[nextID % fileStems.count]
            records.append(RowRecord(
                id: nextID,
                parentID: parentID,
                name: String(format: stem, nextID % 100_000),
                isFolder: false,
                action: action(for: nextID),
                sizeBytes: size(for: nextID),
                daysAgo: nextID % 365
            ))
            nextID += 1
        }

        func folder(parentID: Int, name: String) -> Int {
            let id = nextID
            records.append(RowRecord(
                id: id,
                parentID: parentID,
                name: name,
                isFolder: true,
                action: action(for: id),
                sizeBytes: 0,
                daysAgo: id % 365
            ))
            nextID += 1
            return id
        }

        for a in 0 ..< 16 {
            let f1 = folder(parentID: -1, name: String(format: "folder-%02d", a))
            for _ in 0 ..< 25 {
                file(parentID: f1)
            }
            for b in 0 ..< 10 {
                let f2 = folder(parentID: f1, name: String(format: "day-%02d-%02d", a, b))
                for _ in 0 ..< 15 {
                    file(parentID: f2)
                }
                for c in 0 ..< 10 {
                    let f3 = folder(parentID: f2, name: String(format: "set-%02d-%02d-%02d", a, b, c))
                    for _ in 0 ..< 5 {
                        file(parentID: f3)
                    }
                    for d in 0 ..< 10 {
                        let f4 = folder(
                            parentID: f3,
                            name: String(format: "batch-%02d-%02d-%02d-%02d", a, b, c, d)
                        )
                        // 11 files for the first 11,424 depth-4 folders,
                        // 10 for the rest — 171,424 in total.
                        let fileCount = depth4Ordinal < 11_424 ? 11 : 10
                        depth4Ordinal += 1
                        for _ in 0 ..< fileCount {
                            file(parentID: f4)
                        }
                    }
                }
            }
        }
        return records
    }
}
