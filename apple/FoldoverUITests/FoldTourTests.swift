// The fold in motion, in slow motion: a line turned with the keyboard,
// the leaf let go short of upright and falling back, the leaf folded
// past upright, pressing, opening on the run's imprint, and the imprint
// lifted by Undo. Each stretch of motion keeps a run of frames.

import XCTest

final class FoldTourTests: XCTestCase {
    /// Eight times slower: the fall takes about 3.4 s and the run 24 s.
    private static let slowMotion = 8

    @MainActor
    func testFoldInMotion() {
        continueAfterFailure = true
        let tour = Tour(self, appearance: .light, slowFold: Self.slowMotion)
        let app = tour.app
        tour.launch()
        tour.click(app.outlines.firstMatch.staticTexts["Photos"].firstMatch, "Photos in the sidebar")
        tour.key("r", .command)
        tour.pause(5)
        tour.snap("fold-01-plan")

        // ← stages the turn and names it; Esc lets it go; Return runs it.
        let raw = tour.line("2026/Trip/day2/IMG_2042.RAW")
        guard tour.click(raw, "IMG_2042 line") else { return }
        tour.key(.leftArrow)
        tour.snap("fold-02-staged")
        tour.key(.escape)
        tour.key(.leftArrow)
        tour.key(.return)
        tour.snap("fold-03-rechecking")
        tour.pause(4)
        tour.snap("fold-04-turned")

        let ear = app.windows.descendants(matching: .any)["Fold Folder A’s Half to Sync"].firstMatch
        guard ear.waitForExistence(timeout: 5) else { return XCTFail("Missing: Folder A’s dog-ear") }
        let half = tour.line("2026/Trip/notes.md").frame.width / 2
        XCTAssertGreaterThan(half, 100, "The sheet should be wide enough to fold")
        let corner = ear.coordinate(withNormalizedOffset: CGVector(dx: 0.25, dy: 0.25))

        // Short of upright: the leaf falls back and nothing syncs.
        corner.click(forDuration: 0.2, thenDragTo: corner.withOffset(CGVector(dx: half * 0.35, dy: 0)))
        tour.frames("fold-05-falls-back", count: 6, every: 0.4)
        tour.pause(2)
        XCTAssertTrue(raw.exists, "Letting go short of upright keeps the plan")

        // Past upright: it falls, presses, and the sync runs.
        corner.click(forDuration: 0.2, thenDragTo: corner.withOffset(CGVector(dx: half * 0.85, dy: 0)))
        tour.frames("fold-06-falls", count: 8, every: 0.35)
        tour.snap("fold-07-pressing")
        // The imprint's Undo appears on the desk as the leaf starts to open.
        XCTAssertTrue(app.buttons["Undo Sync"].waitForExistence(timeout: 60), "The run should end on its imprint")
        tour.frames("fold-08-opens", count: 8, every: 0.3)
        tour.pause(2)
        tour.snap("fold-09-imprint")

        tour.key("z", .command)
        tour.pause(3)
        tour.snap("fold-10-lifted")
        app.terminate()
    }
}
