// The benchmark harness (ADR docs/adr/0003-diff-tree-view.md): one run
// per implementation, each in a fresh window over the same generated
// 200,000-row plan — stream 5,000-row batches every 50 ms, scroll
// through the whole tree in 200 steps, then collapse/expand all. A
// `FrameSampler` ticks on the window's display for the run's whole
// life; every tick is attributed to the phase it landed in. A check
// that fails or a phase that overruns fails its run; the run's numbers
// are still reported, because a stalled implementation is the
// measurement.
//
// Adapted from L-K-M/Sweetbriar (ListBenchmarkRunner.swift): the
// per-run window, settle and foreground waits, phase attribution and
// stderr progress lines carry over — simplified for phases instead of
// per-event latency.

import AppKit
import SwiftUI

@MainActor
final class BenchRunner {
    /// Each run's time budget: past it, the run stops posting work and
    /// fails, so a tree that stalls for minutes still leaves a report.
    private static let runBudget: Duration = .seconds(300)

    /// Stream cadence and the scroll sweep's shape.
    private static let batchSize = 5_000
    private static let batchInterval: Duration = .milliseconds(50)
    private static let scrollSteps = 200
    private static let scrollStepInterval: Duration = .milliseconds(50)

    private let clock = ContinuousClock()
    private let startedAt = ContinuousClock.now

    /// The whole benchmark: generate the plan once, run both
    /// implementations over it, reduce to the report. Never throws.
    func run() async -> DiffTreeBenchReport {
        var environment = BenchEnvironment.collect()
        let records = PlanGenerator.makePlan()
        environment.rowCount = records.count
        environment.batchSize = Self.batchSize
        environment.batchIntervalMilliseconds = 50
        environment.baselineFootprintBytes = SystemProbes.physicalFootprint()
        var runs: [BenchRun] = []
        var failures: [String] = []
        for implementation in [Implementation.appkit, .swiftui] {
            let unsettled = await settle()
            let run = await runOnce(
                implementation,
                records: records,
                inheritedFailures: unsettled.map { [$0] } ?? []
            )
            runs.append(run)
        }
        for run in runs {
            failures.append(contentsOf: run.failures.map {
                "\(run.implementation): \($0)"
            })
        }
        let checks = evaluate(runs: runs)
        let verdict: String
        if runs.count != 2 {
            verdict = "incomplete"
        } else {
            verdict = checks.allSatisfy { !$0.gate || $0.pass } ? "pass" : "fail"
        }
        return DiffTreeBenchReport(
            harnessOK: true,
            failures: failures,
            environment: environment,
            runs: runs,
            checks: checks,
            verdict: verdict
        )
    }

    // MARK: - One run

    /// One implementation, one pass: open the window, stream, scroll,
    /// collapse/expand, close; reduce the tick log to the run's report.
    private func runOnce(
        _ implementation: Implementation,
        records: [RowRecord],
        inheritedFailures: [String]
    ) async -> BenchRun {
        var failures = inheritedFailures
        let baseline = SystemProbes.physicalFootprint()
        let deadline = clock.now + Self.runBudget
        func overBudget() -> Bool { clock.now >= deadline }

        let model = PlanModel(expectedCount: records.count)
        let text = RowText()
        let appkitRefs = AppKitTreeRefs()
        let swiftUISeam = SwiftUITreeSeam()
        let window = makeWindow(
            implementation, model: model,
            appkitRefs: appkitRefs, swiftUISeam: swiftUISeam
        )
        guard let content = window.contentView else {
            return failedRun(
                implementation, failures: failures
                    + ["the benchmark window couldn't be created"],
                baseline: baseline
            )
        }

        NSApp.activate()
        window.makeKeyAndOrderFront(nil)
        let foreground = await awaitSteadyForeground(window)
        if !foreground {
            failures.append("the window wasn't key and the app active")
        }
        let windowWasKey = window.isKeyWindow
        let appWasActive = NSApp.isActive
        let openedAt = CACurrentMediaTime()

        let sampler = FrameSampler()
        let footprint = FootprintSampler()
        sampler.extras = { [weak appkitRefs, weak swiftUISeam] in
            switch implementation {
            case .appkit:
                (appkitRefs?.outlineView?.numberOfRows ?? 0) > 0
            case .swiftui:
                swiftUISeam?.firstRowAppearedAt != nil
            }
        }
        sampler.start(on: content)
        footprint.start()

        var callWalls: [String: Double] = [:]

        // Phase "stream": the 200k plan lands in 5,000-row batches
        // every 50 ms, while the sampler watches the frames.
        sampler.currentPhase = "stream"
        var delivered = 0
        var batchProfiles: [BenchBatchProfile] = []
        while delivered < records.count, !overBudget() {
            let end = min(delivered + Self.batchSize, records.count)
            let before = CACurrentMediaTime()
            model.deliver(records[delivered ..< end], text: text)
            let delivery = (CACurrentMediaTime() - before) * 1_000
            let outline = appkitRefs.lastBatchProfile
            batchProfiles.append(BenchBatchProfile(
                rows: end - delivered,
                deliveryMilliseconds: delivery,
                modelMilliseconds: max(0, delivery - (outline?.insertionMilliseconds ?? 0)
                    - (outline?.expansionMilliseconds ?? 0)),
                insertionMilliseconds: outline?.insertionMilliseconds,
                expansionMilliseconds: outline?.expansionMilliseconds,
                parentGroups: outline?.parentGroups,
                newFolders: outline?.newFolders,
                visibleRows: outline?.visibleRows
            ))
            delivered = end
            try? await Task.sleep(for: Self.batchInterval)
        }
        model.markComplete()
        if delivered < records.count {
            failures.append(
                "the stream overran its budget after \(delivered) rows"
            )
        }
        await waitForTicks(2, sampler: sampler)
        let afterLoad = SystemProbes.physicalFootprint()

        // Phase "scroll": 200 evenly spaced programmatic jumps through
        // the flattened tree, about one per 50 ms.
        sampler.currentPhase = "scroll"
        let rowsOnScreen = rowsOnScreen(
            implementation, appkitRefs: appkitRefs, swiftUISeam: swiftUISeam
        )
        let total = model.deliveredCount
        if total > 1 {
            for step in 0 ..< Self.scrollSteps where !overBudget() {
                let rank = step * (total - 1) / (Self.scrollSteps - 1)
                scrollToRank(
                    rank, implementation,
                    appkitRefs: appkitRefs, swiftUISeam: swiftUISeam
                )
                try? await Task.sleep(for: Self.scrollStepInterval)
            }
        }
        let scrollReached = scrollPosition(
            implementation, appkitRefs: appkitRefs, swiftUISeam: swiftUISeam
        )
        let afterScroll = SystemProbes.physicalFootprint()

        // Phases "collapseAll"/"expandAll": AppKit's native pair vs
        // the model-driven pair the SwiftUI API forces.
        sampler.currentPhase = "collapseAll"
        let collapseAt = CACurrentMediaTime()
        collapseAll(
            implementation, model: model,
            appkitRefs: appkitRefs, swiftUISeam: swiftUISeam
        )
        callWalls["collapseAll"] = (CACurrentMediaTime() - collapseAt) * 1_000
        await waitForTicks(4, sampler: sampler)

        sampler.currentPhase = "expandAll"
        let expandAt = CACurrentMediaTime()
        expandAll(
            implementation, model: model,
            appkitRefs: appkitRefs, swiftUISeam: swiftUISeam
        )
        callWalls["expandAll"] = (CACurrentMediaTime() - expandAt) * 1_000
        await waitForTicks(4, sampler: sampler)

        sampler.currentPhase = "drain"
        await waitForTicks(2, sampler: sampler)
        sampler.stop()
        let peakBytes = footprint.stop()

        if overBudget() {
            failures.append("the run overran its time budget")
        }

        // "First visible rows": window open → the first tick that saw
        // rows on screen, per the implementation's own flag.
        let firstRows = sampler.ticks.first { $0.tick.anyVisibleRows }
            .map { ($0.tick.firedAt - openedAt) * 1_000 }
        if firstRows == nil {
            failures.append("no tick ever saw rows on screen")
        }

        window.close()
        log("\(implementation.rawValue): closed")

        let phases = reduce(
            ticks: sampler.ticks, callWalls: callWalls
        )
        let growth = afterLoad > 0
            ? (Double(afterScroll) - Double(afterLoad)) / Double(afterLoad) * 100
            : 0
        return BenchRun(
            implementation: implementation.rawValue,
            ok: failures.isEmpty,
            failures: failures,
            windowWasKey: windowWasKey,
            appWasActive: appWasActive,
            firstRowsMilliseconds: firstRows,
            phases: phases,
            memoryBaselineBytes: baseline,
            memoryAfterLoadBytes: afterLoad,
            memoryAfterScrollBytes: afterScroll,
            memoryPeakBytes: max(peakBytes, afterScroll),
            memoryGrowthPercent: growth,
            rowsOnScreenAtScroll: rowsOnScreen,
            scrollReachedPoints: scrollReached,
            batches: batchProfiles
        )
    }

    // MARK: - Driving one implementation

    /// A fresh fixed-size window hosting the SwiftUI root — the
    /// AppKit tree rides in its `NSViewRepresentable` inside it.
    private func makeWindow(
        _ implementation: Implementation,
        model: PlanModel,
        appkitRefs: AppKitTreeRefs,
        swiftUISeam: SwiftUITreeSeam
    ) -> NSWindow {
        let size = DiffTreeMetrics.contentSize
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: size),
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        // A code-built window defaults to releasing itself on close,
        // which over-releases it under ARC and crashes after `close()`.
        window.isReleasedWhenClosed = false
        window.title = "Diff Tree: \(implementation.rawValue)"
        window.contentView = NSHostingView(
            rootView: BenchRootView(
                implementation: implementation,
                model: model,
                appkitRefs: appkitRefs,
                swiftUISeam: swiftUISeam
            )
        )
        return window
    }

    /// Rows the view reports on screen: AppKit's expanded row count,
    /// or the SwiftUI laid-out content's row equivalent.
    private func rowsOnScreen(
        _ implementation: Implementation,
        appkitRefs: AppKitTreeRefs,
        swiftUISeam: SwiftUITreeSeam
    ) -> Int {
        switch implementation {
        case .appkit:
            appkitRefs.outlineView?.numberOfRows ?? 0
        case .swiftui:
            Int(swiftUISeam.metrics.contentHeight / DiffTreeMetrics.pitch)
        }
    }

    /// The scroll offset the seam reports, in content points.
    private func scrollPosition(
        _ implementation: Implementation,
        appkitRefs: AppKitTreeRefs,
        swiftUISeam: SwiftUITreeSeam
    ) -> Double {
        switch implementation {
        case .appkit:
            Double(appkitRefs.scrollView?.contentView.bounds.minY ?? 0)
        case .swiftui:
            Double(swiftUISeam.metrics.top)
        }
    }

    /// Put `rank` — a flattened row index — at the viewport's top.
    /// AppKit scrolls the clip view to the row's top edge; SwiftUI
    /// goes through the `ScrollViewReader` proxy (the node's `id` is
    /// its rank).
    private func scrollToRank(
        _ rank: Int,
        _ implementation: Implementation,
        appkitRefs: AppKitTreeRefs,
        swiftUISeam: SwiftUITreeSeam
    ) {
        switch implementation {
        case .appkit:
            guard let outline = appkitRefs.outlineView,
                  let scroll = appkitRefs.scrollView,
                  rank >= 0, rank < outline.numberOfRows
            else {
                return
            }
            let top = outline.rect(ofRow: rank).minY
            let clip = scroll.contentView
            clip.scroll(to: NSPoint(x: 0, y: top))
            scroll.reflectScrolledClipView(clip)
        case .swiftui:
            swiftUISeam.scrollProxy?.scrollTo(rank, anchor: .top)
        }
    }

    /// "Collapse all": the platform's own whole-tree collapse —
    /// `collapseItem` for AppKit, detached children for SwiftUI
    /// (`List(children:)` has no expansion API).
    private func collapseAll(
        _ implementation: Implementation,
        model: PlanModel,
        appkitRefs: AppKitTreeRefs,
        swiftUISeam: SwiftUITreeSeam
    ) {
        switch implementation {
        case .appkit:
            appkitRefs.outlineView?.collapseItem(nil, collapseChildren: true)
        case .swiftui:
            model.collapseAllChildren()
        }
    }

    /// The matching "expand all".
    private func expandAll(
        _ implementation: Implementation,
        model: PlanModel,
        appkitRefs: AppKitTreeRefs,
        swiftUISeam: SwiftUITreeSeam
    ) {
        switch implementation {
        case .appkit:
            appkitRefs.outlineView?.expandItem(nil, expandChildren: true)
        case .swiftui:
            model.expandAllChildren()
        }
    }

    // MARK: - Waiting

    /// Wait, bounded, until the window has been key and the app active
    /// for a quarter second without a break: the run measures only a
    /// steady foreground.
    private func awaitSteadyForeground(_ window: NSWindow) async -> Bool {
        let steady: Duration = .milliseconds(250)
        let deadline = clock.now + .seconds(5)
        var since: ContinuousClock.Instant?
        while clock.now < deadline {
            guard window.isKeyWindow, NSApp.isActive else {
                since = nil
                try? await Task.sleep(for: .milliseconds(10))
                continue
            }
            let start = since ?? clock.now
            since = start
            if clock.now - start >= steady {
                return true
            }
            try? await Task.sleep(for: .milliseconds(10))
        }
        return false
    }

    /// Wait, bounded, until the sampler logs `count` more ticks — how
    /// a phase lets the last change reach the screen.
    private func waitForTicks(_ count: Int, sampler: FrameSampler) async {
        let target = sampler.ticks.count + count
        let deadline = clock.now + .seconds(2)
        while sampler.ticks.count < target, clock.now < deadline {
            try? await Task.sleep(for: .milliseconds(5))
        }
    }

    /// Wait, bounded at thirty seconds, until the main thread answers
    /// promptly ten times in a row: a run starts only once the last
    /// one's teardown has finished.
    private func settle() async -> String? {
        let started = clock.now
        let deadline = started + .seconds(30)
        var prompt = 0
        while prompt < 10, clock.now < deadline {
            let before = clock.now
            try? await Task.sleep(for: .milliseconds(100))
            prompt = clock.now - before < .milliseconds(150) ? prompt + 1 : 0
        }
        let waited = (clock.now - started).components.seconds
        guard prompt >= 10 else {
            log("not settled after \(waited) s")
            return "the previous run's teardown hadn't settled after \(waited) s"
        }
        return nil
    }

    /// One progress line on stderr — where a CI run spends its time.
    private func log(_ message: String) {
        let elapsed = (ContinuousClock.now - startedAt).components.seconds
        FileHandle.standardError.write(
            Data("s5-diff-tree: \(elapsed) s: \(message)\n".utf8)
        )
    }

    // MARK: - Reducing the tick log

    /// Group the tick log into contiguous phases and reduce each:
    /// frame intervals (tick-to-tick vsync deltas), the longest stall
    /// (the largest gap between tick callbacks — the display link
    /// fires on the main run loop, so a blocked main thread shows as
    /// a gap), and the end footprint.
    private func reduce(
        ticks: [(phase: String, tick: FrameTick)],
        callWalls: [String: Double]
    ) -> [BenchPhase] {
        var phases: [BenchPhase] = []
        var i = 0
        while i < ticks.count {
            let phase = ticks[i].phase
            var intervals: [Double] = []
            var gaps: [Double] = []
            var j = i
            while j < ticks.count, ticks[j].phase == phase {
                if j > i {
                    intervals.append(
                        (ticks[j].tick.timestamp - ticks[j - 1].tick.timestamp) * 1_000
                    )
                    gaps.append(
                        (ticks[j].tick.firedAt - ticks[j - 1].tick.firedAt) * 1_000
                    )
                }
                j += 1
            }
            // A synchronous call's own wall counts as a stall too —
            // it blocked the ticks the same way.
            let stall = max(
                gaps.max() ?? 0,
                callWalls[phase] ?? 0
            )
            phases.append(BenchPhase(
                name: phase,
                tickCount: j - i,
                frameCount: intervals.count,
                frameP50Milliseconds: BenchStatistics.percentile(intervals, p: 50) ?? 0,
                frameP95Milliseconds: BenchStatistics.percentile(intervals, p: 95) ?? 0,
                frameMaxMilliseconds: intervals.max() ?? 0,
                maxStallMilliseconds: stall,
                wallMilliseconds: callWalls[phase],
                footprintEndBytes: ticks[j - 1].tick.footprintBytes,
                frameIntervalsMilliseconds: intervals
            ))
            i = j
        }
        return phases
    }

    /// A run that can't start still produces a report.
    private func failedRun(
        _ implementation: Implementation,
        failures: [String],
        baseline: UInt64
    ) -> BenchRun {
        BenchRun(
            implementation: implementation.rawValue,
            ok: false,
            failures: failures,
            windowWasKey: false,
            appWasActive: false,
            firstRowsMilliseconds: nil,
            phases: [],
            memoryBaselineBytes: baseline,
            memoryAfterLoadBytes: baseline,
            memoryAfterScrollBytes: baseline,
            memoryPeakBytes: baseline,
            memoryGrowthPercent: 0,
            rowsOnScreenAtScroll: 0,
            scrollReachedPoints: 0
        )
    }

    /// The pass criteria (PLAN.md §8 S5, made precise): the AppKit run
    /// gates — first rows under a second, frame-interval p95 at or
    /// under the 60 Hz frame while streaming and scrolling, no stall
    /// past 100 ms, memory flat while scrolling. The same numbers are
    /// evaluated for SwiftUI, recorded without gating.
    private func evaluate(runs: [BenchRun]) -> [BenchCheck] {
        var checks: [BenchCheck] = []
        for run in runs {
            let gate = run.implementation == Implementation.appkit.rawValue
            let stream = run.phases.first { $0.name == "stream" }
            let scroll = run.phases.first { $0.name == "scroll" }
            let worstStall = run.phases.map(\.maxStallMilliseconds).max()
            checks.append(BenchCheck(
                name: "\(run.implementation).firstRows<1000ms",
                implementation: run.implementation,
                pass: run.firstRowsMilliseconds.map { $0 < 1_000 } ?? false,
                detail: "window open to first visible rows under 1 s",
                value: run.firstRowsMilliseconds,
                limit: 1_000,
                gate: gate
            ))
            checks.append(BenchCheck(
                name: "\(run.implementation).stream.p95<=16.7ms",
                implementation: run.implementation,
                pass: stream.map { $0.frameP95Milliseconds <= 16.7 } ?? false,
                detail: "frame-interval p95 at or under the 60 Hz frame while streaming",
                value: stream?.frameP95Milliseconds,
                limit: 16.7,
                gate: gate
            ))
            checks.append(BenchCheck(
                name: "\(run.implementation).scroll.p95<=16.7ms",
                implementation: run.implementation,
                pass: scroll.map { $0.frameP95Milliseconds <= 16.7 } ?? false,
                detail: "frame-interval p95 at or under the 60 Hz frame while scrolling",
                value: scroll?.frameP95Milliseconds,
                limit: 16.7,
                gate: gate
            ))
            checks.append(BenchCheck(
                name: "\(run.implementation).maxStall<100ms",
                implementation: run.implementation,
                pass: worstStall.map { $0 < 100 } ?? false,
                detail: "longest main-thread stall under 100 ms",
                value: worstStall,
                limit: 100,
                gate: gate
            ))
            checks.append(BenchCheck(
                name: "\(run.implementation).memoryGrowth<10%",
                implementation: run.implementation,
                pass: run.memoryGrowthPercent < 10,
                detail: "phys_footprint growth while scrolling under 10 %",
                value: run.memoryGrowthPercent,
                limit: 10,
                gate: gate
            ))
        }
        return checks
    }
}

extension BenchEnvironment {
    /// The machine description: sysctls for the model, CPU brand and
    /// the virtualized flag; the OS and Xcode versions; the main
    /// screen's refresh rate; the plan's fixed shape.
    @MainActor
    static func collect() -> BenchEnvironment {
        let info = Bundle.main.infoDictionary
        let fps = Double(NSScreen.main?.maximumFramesPerSecond ?? 0)
        let size = DiffTreeMetrics.contentSize
        return BenchEnvironment(
            hwModel: SystemProbes.sysctlString("hw.model"),
            cpuBrand: SystemProbes.sysctlString("machdep.cpu.brand_string"),
            virtualized: SystemProbes.sysctlInt("kern.hv_vmm_present") != 0,
            osVersion: ProcessInfo.processInfo.operatingSystemVersionString,
            xcodeVersion: info?["DTXcode"] as? String ?? "",
            xcodeBuild: info?["DTXcodeBuild"] as? String ?? "",
            refreshRateHertz: fps,
            nominalIntervalMilliseconds: fps > 0 ? 1_000 / fps : 0,
            rowCount: 0,
            batchSize: 0,
            batchIntervalMilliseconds: 0,
            windowSize: "\(Int(size.width))x\(Int(size.height))",
            baselineFootprintBytes: 0
        )
    }
}
