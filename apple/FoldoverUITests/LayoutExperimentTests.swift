// TEMPORARY, removed before merge: which layout change stops the split
// view's constraint loop. Each variant turns changes on with
// -foldover-layout, then runs an unreachable agent, and a selected and
// previewed pair, through the narrowing sweep from a fresh install.

import XCTest

final class LayoutExperimentTests: XCTestCase {
    @MainActor func testV01Baseline() { run("") }
    @MainActor func testV02BarLines() { run("barLines") }
    @MainActor func testV03DetailFrame() { run("detailFrame") }
    @MainActor func testV04DetailColumn() { run("detailColumn") }
    @MainActor func testV05EmptyWrap() { run("emptyWrap") }
    @MainActor func testV06BarLinesEmptyWrap() { run("barLines,emptyWrap") }
    @MainActor func testV07BarLinesDetailColumn() { run("barLines,detailColumn") }
    @MainActor func testV08BarLinesEmptyWrapDetailColumn() { run("barLines,emptyWrap,detailColumn") }
    @MainActor func testV09BarLinesEmptyWrapDetailFrame() { run("barLines,emptyWrap,detailFrame") }
    @MainActor func testV10DetailFrameWindow800() { run("barLines,emptyWrap,detailFrame,windowMin800") }
    @MainActor func testV11DetailColumnWindow800() { run("barLines,emptyWrap,detailColumn,windowMin800") }

    @MainActor
    private func run(_ flags: String) {
        continueAfterFailure = true
        for scenario in ["agent-failed", "slow-agent"] {
            let tour = Tour(self, appearance: .light, scenario: scenario)
            tour.app.launchArguments += ["-foldover-layout", flags]
            let label = "variant [\(flags)] \(scenario)"
            tour.app.launch()
            guard tour.app.windows.firstMatch.waitForExistence(timeout: 30) else {
                XCTFail("\(label): no window appeared")
                continue
            }
            let stress = Stress(tour: tour, scenario: label)
            guard stress.step("settled", { tour.pause(4) }) else { continue }
            if scenario == "slow-agent" {
                let photos = tour.app.outlines.firstMatch.staticTexts["Photos"].firstMatch
                guard stress.step("pair selected", { _ = tour.click(photos, "Photos") }) else { continue }
                guard stress.step("previewed", {
                    tour.key("r", .command)
                    tour.pause(5)
                }) else { continue }
            }
            guard stress.sweep("sweep") else { continue }
            print("RESULT \(label): survived")
            tour.app.terminate()
        }
    }
}
