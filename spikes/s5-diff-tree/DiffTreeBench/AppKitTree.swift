// The AppKit implementation (ADR docs/adr/0003-diff-tree-view.md): a
// view-based `NSOutlineView` over the shared `PlanModel`, hosted in
// SwiftUI through `NSViewRepresentable` — the shape PLAN.md §4.5 chose
// for the Preview. Batches land as grouped `insertItems`; every new
// folder is expanded on arrival so the tree reveals while it streams.
//
// Adapted from L-K-M/Sweetbriar (AppKitPrototypeList.swift): the
// one-column table, the reused hand-laid cell and the clip-view
// scrolling pattern carry over from the list benchmark.

import AppKit
import SwiftUI

/// The harness's handle on the mounted outline — the representable
/// fills it when its view exists, so the runner can drive scrolling
/// and the expand/collapse pair without reaching through SwiftUI.
@MainActor
final class AppKitTreeRefs {
    var outlineView: NSOutlineView?
    var tableView: NSTableView?
    var scrollView: NSScrollView?
    var lastBatchProfile: OutlineBatchProfile?
}

struct OutlineBatchProfile {
    let insertionMilliseconds: Double
    let expansionMilliseconds: Double
    let parentGroups: Int
    let newFolders: Int
    let visibleRows: Int
}

/// The SwiftUI side of the AppKit implementation: the whole tree is a
// representable, so the window's SwiftUI root can host it like any
// other view.
struct OutlineTreeRepresentable: NSViewRepresentable {
    let model: PlanModel
    let refs: AppKitTreeRefs

    func makeCoordinator() -> OutlineCoordinator {
        OutlineCoordinator(model: model, refs: refs)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let outline = NSOutlineView()
        let column = NSTableColumn(
            identifier: NSUserInterfaceItemIdentifier("plan")
        )
        outline.addTableColumn(column)
        outline.outlineTableColumn = column
        outline.headerView = nil
        outline.style = .plain
        outline.usesAutomaticRowHeights = false
        outline.rowHeight = DiffTreeMetrics.pitch
        outline.indentationPerLevel = 14
        outline.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        outline.dataSource = context.coordinator
        outline.delegate = context.coordinator

        let scroll = NSScrollView()
        scroll.documentView = outline
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true

        context.coordinator.outlineView = outline
        // Deliveries arrive on the main actor; the coordinator turns
        // each journal into row inserts on its own view.
        model.onBatch = { [weak coordinator = context.coordinator] journal in
            coordinator?.apply(journal)
        }

        refs.outlineView = outline
        refs.tableView = outline
        refs.scrollView = scroll
        return scroll
    }

    /// Nothing here is driven by SwiftUI state — batches apply
    /// imperatively through `model.onBatch`.
    func updateNSView(_ scroll: NSScrollView, context: Context) {}
}

/// The documented large-plan fallback: one row per full relative path, without
/// outline expansion work. It receives the same model and batch cadence.
struct FlatTableRepresentable: NSViewRepresentable {
    let model: PlanModel
    let refs: AppKitTreeRefs

    func makeCoordinator() -> FlatTableCoordinator {
        FlatTableCoordinator(model: model, refs: refs)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let table = NSTableView()
        table.addTableColumn(NSTableColumn(identifier: NSUserInterfaceItemIdentifier("plan")))
        table.headerView = nil
        table.style = .plain
        table.usesAutomaticRowHeights = false
        table.rowHeight = DiffTreeMetrics.pitch
        table.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        table.dataSource = context.coordinator
        table.delegate = context.coordinator

        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        model.onBatch = { [weak coordinator = context.coordinator] journal in
            coordinator?.apply(journal)
        }
        refs.tableView = table
        refs.scrollView = scroll
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {}
}

@MainActor
final class FlatTableCoordinator: NSObject, NSTableViewDataSource, NSTableViewDelegate {
    private static let cellIdentifier = NSUserInterfaceItemIdentifier("FlatPlanRow")
    private let model: PlanModel
    private weak var refs: AppKitTreeRefs?

    init(model: PlanModel, refs: AppKitTreeRefs) {
        self.model = model
        self.refs = refs
    }

    func apply(_ journal: BatchJournal) {
        guard let table = refs?.tableView else { return }
        let started = CACurrentMediaTime()
        let inserted = IndexSet(integersIn: table.numberOfRows ..< model.deliveredCount)
        table.insertRows(at: inserted, withAnimation: [])
        refs?.lastBatchProfile = OutlineBatchProfile(
            insertionMilliseconds: (CACurrentMediaTime() - started) * 1_000,
            expansionMilliseconds: 0,
            parentGroups: 1,
            newFolders: journal.newFolders.count,
            visibleRows: table.numberOfRows
        )
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        model.deliveredCount
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard let node = model.node(id: row) else { return nil }
        let cell = tableView.makeView(withIdentifier: Self.cellIdentifier, owner: nil)
            as? DiffTreeCellView ?? DiffTreeCellView()
        cell.identifier = Self.cellIdentifier
        cell.configure(node: node, displayName: node.relativePath)
        return cell
    }
}

/// The outline's data source and delegate: reads the model's node tree
/// directly, and applies each delivered batch as grouped inserts plus
/// an `expandItem` per new folder (the streamed reveal).
@MainActor
final class OutlineCoordinator: NSObject {
    private static let cellIdentifier = NSUserInterfaceItemIdentifier(
        "DiffTreeRow"
    )

    private let model: PlanModel
    private weak var refs: AppKitTreeRefs?
    weak var outlineView: NSOutlineView?

    init(model: PlanModel, refs: AppKitTreeRefs) {
        self.model = model
        self.refs = refs
    }

    /// One batch's changes: the inserts, then an expand per new folder
    /// so the tree stays fully expanded while it grows.
    func apply(_ journal: BatchJournal) {
        guard let outlineView else { return }
        let started = CACurrentMediaTime()
        outlineView.beginUpdates()
        for entry in journal.entries {
            outlineView.insertItems(
                at: entry.indexes,
                inParent: entry.parent,
                withAnimation: []
            )
        }
        outlineView.endUpdates()
        let inserted = CACurrentMediaTime()
        for folder in journal.newFolders {
            outlineView.expandItem(folder)
        }
        let expanded = CACurrentMediaTime()
        refs?.lastBatchProfile = OutlineBatchProfile(
            insertionMilliseconds: (inserted - started) * 1_000,
            expansionMilliseconds: (expanded - inserted) * 1_000,
            parentGroups: journal.entries.count,
            newFolders: journal.newFolders.count,
            visibleRows: outlineView.numberOfRows
        )
    }
}

extension OutlineCoordinator: NSOutlineViewDataSource {
    func outlineView(
        _ outlineView: NSOutlineView,
        numberOfChildrenOfItem item: Any?
    ) -> Int {
        guard let node = item as? PlanNode else {
            return model.topNodes.count
        }
        return node.children?.count ?? 0
    }

    func outlineView(
        _ outlineView: NSOutlineView,
        child index: Int,
        ofItem item: Any?
    ) -> Any {
        guard let node = item as? PlanNode else {
            return model.topNodes[index]
        }
        return node.children![index]
    }

    func outlineView(
        _ outlineView: NSOutlineView,
        isItemExpandable item: Any
    ) -> Bool {
        (item as? PlanNode)?.children != nil
    }
}

extension OutlineCoordinator: NSOutlineViewDelegate {
    /// A reused cell, configured from the node's precomputed strings —
    /// or made fresh when the pool is empty.
    func outlineView(
        _ outlineView: NSOutlineView,
        viewFor tableColumn: NSTableColumn?,
        item: Any
    ) -> NSView? {
        guard let node = item as? PlanNode else { return nil }
        let cell = outlineView.makeView(
            withIdentifier: Self.cellIdentifier, owner: nil
        ) as? DiffTreeCellView ?? DiffTreeCellView()
        cell.identifier = Self.cellIdentifier
        cell.configure(node: node)
        return cell
    }
}

/// The reused row cell. Subviews are created once and framed by hand —
// no Auto Layout inside cells (same rule as the Sweetbriar prototype).
private final class DiffTreeCellView: NSTableCellView {
    private let glyphField = NSTextField(labelWithString: "")
    private let nameField = NSTextField(labelWithString: "")
    private let sizeField = NSTextField(labelWithString: "")
    private let dateField = NSTextField(labelWithString: "")

    init() {
        super.init(frame: .zero)
        glyphField.font = .systemFont(ofSize: 12, weight: .semibold)
        glyphField.alignment = .center
        nameField.font = .systemFont(ofSize: 13)
        nameField.lineBreakMode = .byTruncatingTail
        for field in [sizeField, dateField] {
            field.font = .systemFont(ofSize: 12)
            field.textColor = .secondaryLabelColor
            field.alignment = .right
        }
        for field in [glyphField, nameField, sizeField, dateField] {
            field.setAccessibilityElement(false)
            addSubview(field)
        }
    }

    required init?(coder: NSCoder) {
        fatalError("DiffTreeCellView is built in code")
    }

    override var isFlipped: Bool { true }

    func configure(node: PlanNode, displayName: String? = nil) {
        glyphField.stringValue = node.action.glyph
        glyphField.textColor = Self.tint(for: node.action)
        nameField.stringValue = displayName ?? node.name
        sizeField.stringValue = node.isFolder ? "" : node.sizeText
        dateField.stringValue = node.dateText
        setAccessibilityElement(true)
        let accessibility = displayName.map {
            "\(node.action.accessibilityVerb) \($0), \(node.sizeText), modified \(node.dateText)"
        } ?? node.accessibilityText
        setAccessibilityLabel(accessibility)
        needsLayout = true
    }

    /// The action's color, resolved for the cell's appearance so Dark
    /// Mode switches cleanly.
    private static func tint(for action: PlanAction) -> NSColor {
        switch action {
        case .copyToTarget: .systemBlue
        case .copyToMac: .systemGreen
        case .archive: .systemOrange
        case .conflict: .systemRed
        }
    }

    override func layout() {
        super.layout()
        let height = bounds.height
        let width = bounds.width
        glyphField.frame = NSRect(
            x: 2, y: (height - 16) / 2, width: 18, height: 16
        )
        let nameX = glyphField.frame.maxX + 6
        let dateWidth: CGFloat = 96
        let sizeWidth: CGFloat = 80
        dateField.frame = NSRect(
            x: width - dateWidth - 4, y: (height - 15) / 2,
            width: dateWidth, height: 15
        )
        sizeField.frame = NSRect(
            x: dateField.frame.minX - sizeWidth - 12,
            y: (height - 15) / 2, width: sizeWidth, height: 15
        )
        nameField.frame = NSRect(
            x: nameX, y: (height - 16) / 2,
            width: max(0, sizeField.frame.minX - nameX - 8), height: 16
        )
    }
}
