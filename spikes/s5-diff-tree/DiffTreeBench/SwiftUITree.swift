// The SwiftUI implementation (ADR docs/adr/0003-diff-tree-view.md):
// `List(model.topNodes, children: \.children)` over the same
// `PlanModel`, in an `NSHostingView` — the canonical SwiftUI outline.
// Two things the API cannot do are themselves part of the measurement:
// it offers no programmatic expansion (the model-driven
// collapse/expand pair stands in), and scroll reads come only from
// `onScrollGeometryChange`, not from the rows.
//
// Adapted from L-K-M/Sweetbriar (SwiftUIPrototypeList.swift): the
// NSHostingView wrapper, the `ScrollViewReader` seam and the scroll
// metrics pattern carry over from the list benchmark.

import AppKit
import SwiftUI

/// The scroll geometry the list reports through
/// `onScrollGeometryChange`. Equatable so unchanged values don't fire.
struct ScrollMetrics: Equatable {
    /// Content-space `y` of the viewport's top edge.
    var top: CGFloat
    /// The viewport's height.
    var height: CGFloat
    /// The laid-out content's height — how much tree the list believes
    /// is on screen, whether or not rows are visible.
    var contentHeight: CGFloat
}

/// What the harness reads back out of the SwiftUI tree — the proxy it
/// scrolls with, the last reported scroll metrics, and whether any row
/// has ever materialized (`onAppear` fires only for rows the list
/// actually draws).
@MainActor
@Observable
final class SwiftUITreeSeam {
    var scrollProxy: ScrollViewProxy?
    var metrics = ScrollMetrics(top: 0, height: 0, contentHeight: 0)
    /// `CACurrentMediaTime()` of the first row's `onAppear`, if any.
    private(set) var firstRowAppearedAt: Double?

    func noteRowAppeared() {
        if firstRowAppearedAt == nil {
            firstRowAppearedAt = CACurrentMediaTime()
        }
    }
}

/// The window's SwiftUI root: either implementation behind the same
/// `NSHostingView`, so the AppKit tree pays the same hosting cost as
/// the SwiftUI one — `NSOutlineView` in an `NSViewRepresentable` is
/// exactly the shape PLAN.md §4.5 prescribes for the Preview.
struct BenchRootView: View {
    let implementation: Implementation
    let model: PlanModel
    let appkitRefs: AppKitTreeRefs
    let swiftUISeam: SwiftUITreeSeam

    var body: some View {
        switch implementation {
        case .appkit:
            OutlineTreeRepresentable(model: model, refs: appkitRefs)
        case .swiftui:
            SwiftUITreeRoot(model: model, seam: swiftUISeam)
        }
    }
}

/// Which tree a run measures.
enum Implementation: String, Codable {
    case appkit
    case swiftui
}

/// The canonical SwiftUI outline: `List` walks `children` itself,
/// disclosure and all. `topNodes` is the model's observed property,
/// so each batch's re-assignment re-evaluates this body.
struct SwiftUITreeRoot: View {
    let model: PlanModel
    let seam: SwiftUITreeSeam

    var body: some View {
        ScrollViewReader { proxy in
            List(model.topNodes, children: \.children) { node in
                DiffTreeRowView(node: node, seam: seam)
            }
            .listStyle(.plain)
            .environment(\.defaultMinListRowHeight, DiffTreeMetrics.pitch)
            .onScrollGeometryChange(
                for: ScrollMetrics.self,
                of: { geometry in
                    ScrollMetrics(
                        top: geometry.visibleRect.minY,
                        height: geometry.visibleRect.height,
                        contentHeight: geometry.contentSize.height
                    )
                },
                action: { _, new in
                    MainActor.assumeIsolated {
                        seam.metrics = new
                    }
                }
            )
            .onAppear {
                MainActor.assumeIsolated {
                    seam.scrollProxy = proxy
                }
            }
        }
    }
}

/// The one row type — the same fields, order and label as the AppKit
/// cell. `onAppear` marks first-materialized-row for the harness's
/// time-to-first-rows metric.
private struct DiffTreeRowView: View {
    let node: PlanNode
    let seam: SwiftUITreeSeam

    var body: some View {
        HStack(spacing: 6) {
            Text(node.action.glyph)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Self.tint(for: node.action))
                .frame(width: 18)
            Text(node.name)
                .font(.system(size: 13))
                .lineLimit(1)
            Spacer(minLength: 8)
            Text(node.isFolder ? "" : node.sizeText)
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .frame(width: 80, alignment: .trailing)
            Text(node.dateText)
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .frame(width: 96, alignment: .trailing)
        }
        .frame(height: DiffTreeMetrics.pitch)
        // One element, one label — the shared VoiceOver wording.
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(verbatim: node.accessibilityText))
        .onAppear {
            MainActor.assumeIsolated {
                seam.noteRowAppeared()
            }
        }
    }

    /// The same action colors as the AppKit cell.
    private static func tint(for action: PlanAction) -> Color {
        switch action {
        case .copyToTarget: .blue
        case .copyToMac: .green
        case .archive: .orange
        case .conflict: .red
        }
    }
}
