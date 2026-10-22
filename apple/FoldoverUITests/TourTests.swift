// A screenshot tour of the app's main surfaces, run against canned
// agent answers: `xcodebuild test -scheme FoldoverUITour`.

import XCTest

final class TourTests: XCTestCase {
    @MainActor
    func testTourLight() {
        tour(.light)
    }

    @MainActor
    func testTourDark() {
        tour(.dark)
    }

    @MainActor
    private func tour(_ appearance: TourAppearance) {
        continueAfterFailure = true
        let tour = Tour(self, appearance: appearance)
        let app = tour.app
        tour.launch()
        tour.snap("01-launch")

        tour.click(app.staticTexts["Photos"].firstMatch, "Photos in the sidebar")
        tour.pause(1)
        tour.snap("02-photos")

        tour.click(app.buttons["Preview"].firstMatch, "Preview button")
        tour.pause(5)
        tour.snap("03-photos-preview")

        for menu in ["Foldover", "File", "Edit", "View", "Window", "Help"] {
            tour.snapMenu(menu)
        }

        tour.click(app.buttons["Edit Pair"].firstMatch, "Edit Pair button")
        tour.pause(1)
        tour.snap("04-editor")
        tour.key(.escape)

        tour.click(app.buttons["Browse Archive…"].firstMatch, "Browse Archive button")
        tour.pause(1)
        tour.snap("05-archive")
        tour.key(.escape)

        tour.click(app.staticTexts["Projects"].firstMatch, "Projects in the sidebar")
        tour.pause(1)
        tour.snap("06-projects")

        tour.click(app.staticTexts["Music"].firstMatch, "Music in the sidebar")
        tour.pause(1)
        tour.snap("07-music")

        tour.key("n", .command)
        tour.pause(1)
        tour.snap("08-new-pair")
        tour.key(.escape)

        tour.key(",", .command)
        tour.pause(1.5)
        tour.snap("09-settings-general")
        tour.click(app.buttons["Mac-to-Mac"].firstMatch, "Mac-to-Mac settings tab")
        tour.pause(1.5)
        tour.snap("10-settings-mac-to-mac")
        tour.key("w", .command)

        tour.click(app.statusItems.firstMatch, "menu bar extra")
        tour.pause(1)
        tour.snap("11-menu-bar-extra")
        tour.key(.escape)

        app.terminate()
    }
}
