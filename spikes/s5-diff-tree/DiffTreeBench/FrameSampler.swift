// The display-frame sampler (ADR docs/adr/0003-diff-tree-view.md): a
// `CADisplayLink` on the run's window, ticking for the run's whole
// life. Each tick records its phase, the frame's vsync timestamp, when
// the callback ran, and the process's `phys_footprint`. Consecutive
// timestamp deltas are the frame intervals; a main-thread stall shows
// up as a gap between `firedAt`s, because the link fires on the main
// run loop and cannot fire while it is blocked.
//
// Adapted from L-K-M/Sweetbriar (BenchmarkTickSampler.swift): same
// link, same sampling — the per-event commit-point machinery is dropped
// because this spike measures phases, not input latency.

import AppKit

/// What the sampler learned at one tick.
struct FrameTick {
    /// `CADisplayLink.timestamp` — the frame's vsync time. Frame
    /// intervals are deltas of these.
    var timestamp: Double
    /// `CACurrentMediaTime()` when the tick ran. A stall shows as a
    /// gap here: the link's callback is main-run-loop work.
    var firedAt: Double
    /// `task_info(TASK_VM_INFO)` `phys_footprint` at the tick.
    var footprintBytes: UInt64
    /// Whatever the run reported as "rows on screen" at this tick.
    var anyVisibleRows: Bool
}

/// Ticks once per display frame for a run's life. `currentPhase`
/// attributes each tick to the phase it landed in; `extras` answers
/// the run's per-tick "any rows visible" question.
@MainActor
final class FrameSampler: NSObject {
    /// The live state one tick needs from the run — just the
    /// visibility flag; scroll positions are read at phase ends.
    var extras: (@MainActor () -> Bool)?

    /// The phase new ticks are attributed to.
    var currentPhase = "open"

    /// The tick log — (phase, tick) pairs in order.
    private(set) var ticks: [(phase: String, tick: FrameTick)] = []

    // Weak: the link retains its target and the run loop retains the
    // link, so a strong reference here would be a retain cycle that
    // outlives the run and pollutes the next one's memory baseline.
    private weak var link: CADisplayLink?

    /// Start ticking on `view`'s display.
    func start(on view: NSView) {
        let link = view.displayLink(target: self, selector: #selector(tickFired(_:)))
        // The display's full rate, so an interval past nominal is a
        // missed frame, not a lowered rate.
        let fullRate = Float(view.window?.screen?.maximumFramesPerSecond ?? 0)
        if fullRate > 0 {
            link.preferredFrameRateRange = CAFrameRateRange(
                minimum: fullRate, maximum: fullRate, preferred: fullRate
            )
        }
        link.add(to: .main, forMode: .common)
        self.link = link
    }

    /// Stop the link. Idempotent.
    func stop() {
        link?.invalidate()
        link = nil
    }

    @objc
    private func tickFired(_ link: CADisplayLink) {
        ticks.append((
            currentPhase,
            FrameTick(
                timestamp: link.timestamp,
                firedAt: CACurrentMediaTime(),
                footprintBytes: SystemProbes.physicalFootprint(),
                anyVisibleRows: extras?() ?? false
            )
        ))
    }
}
