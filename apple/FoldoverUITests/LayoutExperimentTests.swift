// TEMPORARY, removed before merge: how the detail beside the inspector
// pane should get its minimum. Each variant runs an unreachable agent,
// and a selected and previewed pair, through the narrowing sweep from a
// fresh install.

import XCTest

final class LayoutExperimentTests: XCTestCase {
    @MainActor func testV01PaneFrame() { run("") }
    @MainActor func testV02PaneColumnMin() { run("paneColumnMin") }
    @MainActor func testV03PaneNoMin() { run("paneNoMin") }

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
