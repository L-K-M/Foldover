// The main window in the states a real agent produces around launch
// (`FixtureScenario`), pushed through the widths its split view can
// take. A split view column whose minimum size changes from one layout
// pass to the next makes AppKit raise an exception that quits the app,
// so every step checks that the app still runs. The ui-tour workflow
// keeps each scenario's screenshots and any crash report.

import XCTest

final class LayoutStressTests: XCTestCase {
    @MainActor func testSlowAgent() { stress("slow-agent") }
    @MainActor func testSafeMode() { stress("safe-mode") }
    @MainActor func testAgentFailed() { stress("agent-failed") }
    @MainActor func testNoPairs() { stress("no-pairs") }
    @MainActor func testPairsFail() { stress("pairs-fail") }

    @MainActor
    private func stress(_ scenario: String) {
        continueAfterFailure = true
        let tour = Tour(self, appearance: .light, scenario: scenario)
        let app = tour.app
        // No settling pause: the first steps run while a slow agent is
        // still connecting, as they would at a real launch.
        app.launch()
        guard app.windows.firstMatch.waitForExistence(timeout: 30) else {
            return XCTFail("\(scenario): no window appeared")
        }
        let stress = Stress(tour: tour, scenario: scenario)
        tour.snap("stress-\(scenario)-1-launch")

        guard stress.sweep("connecting") else { return }
        guard stress.step("settled", { tour.pause(3) }) else { return }
        tour.snap("stress-\(scenario)-2-settled")
        guard stress.sweep("settled") else { return }

        let photos = app.outlines.firstMatch.staticTexts["Photos"].firstMatch
        if photos.exists {
            guard stress.step("select pair", { photos.click() }) else { return }
            guard stress.step("preview", {
                tour.key("r", .command)
                tour.pause(5)
            }) else { return }
            tour.snap("stress-\(scenario)-3-preview")
            guard stress.sweep("preview") else { return }
        }
        app.terminate()
    }
}

/// Steps that change the main window's column widths, each followed by
/// a check that the app survived it.
@MainActor
private struct Stress {
    let tour: Tour
    let scenario: String

    private var app: XCUIApplication { tour.app }

    /// Run `action`, give layout a moment, and check the app still runs.
    @discardableResult
    func step(_ name: String, _ action: () -> Void) -> Bool {
        action()
        tour.pause(0.8)
        let window = app.windows.firstMatch
        print("stress \(scenario) \(name): window \(window.frame), dividers \(dividerPositions())")
        guard app.state == .runningForeground else {
            XCTFail("\(scenario): the app quit at “\(name)”")
            return false
        }
        return true
    }

    /// Every width change the window offers: both dividers to their
    /// limits, each side column hidden and shown, and the window
    /// narrowed and widened.
    func sweep(_ phase: String) -> Bool {
        let steps: [(String, () -> Void)] = [
            ("sidebar narrow", { dragDivider(.sidebar, by: -160) }),
            ("sidebar wide", { dragDivider(.sidebar, by: 320) }),
            ("sidebar middle", { dragDivider(.sidebar, by: -110) }),
            ("inspector wide", { dragDivider(.inspector, by: -160) }),
            ("inspector narrow", { dragDivider(.inspector, by: 320) }),
            ("inspector hidden", { tour.key("i", [.command, .control]) }),
            ("inspector shown", { tour.key("i", [.command, .control]) }),
            ("sidebar hidden", { tour.key("s", [.command, .control]) }),
            ("sidebar shown", { tour.key("s", [.command, .control]) }),
            ("window narrow", { resizeWindow(by: -400) }),
            ("window wide", { resizeWindow(by: 400) }),
        ]
        for (name, action) in steps {
            guard step("\(phase), \(name)", action) else { return false }
        }
        return true
    }

    private enum Divider {
        case sidebar
        case inspector
    }

    /// The split view's dividers, left to right: the sidebar's, then the
    /// inspector's when it shows.
    private func dividers() -> [XCUIElement] {
        app.windows.firstMatch.splitters.allElementsBoundByIndex
            .filter { $0.exists && $0.frame.width < 40 }
            .sorted { $0.frame.minX < $1.frame.minX }
    }

    private func dividerPositions() -> [Int] {
        dividers().map { Int($0.frame.midX) }
    }

    private func dragDivider(_ divider: Divider, by dx: CGFloat) {
        let all = dividers()
        let target: XCUIElement?
        switch divider {
        case .sidebar: target = all.count >= 1 ? all.first : nil
        case .inspector: target = all.count >= 2 ? all.last : nil
        }
        guard let target else {
            print("stress \(scenario): no \(divider) divider among \(all.count)")
            return
        }
        let start = target.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        start.click(forDuration: 0.3, thenDragTo: start.withOffset(CGVector(dx: dx, dy: 0)))
    }

    /// Drag the window's bottom-right corner sideways.
    private func resizeWindow(by dx: CGFloat) {
        let corner = app.windows.firstMatch
            .coordinate(withNormalizedOffset: CGVector(dx: 1, dy: 1))
            .withOffset(CGVector(dx: -2, dy: -2))
        corner.click(forDuration: 0.3, thenDragTo: corner.withOffset(CGVector(dx: dx, dy: 0)))
    }
}
