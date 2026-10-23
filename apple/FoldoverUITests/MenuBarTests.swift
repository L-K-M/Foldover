import AppKit
import XCTest

final class MenuBarTests: XCTestCase {
    @MainActor
    func testManyPairsScrollWithoutHidingTheCommands() {
        let tour = Tour(self, appearance: .light, scenario: "many-pairs")
        tour.launch()
        tour.click(tour.app.statusItems.firstMatch, "menu bar extra")
        let pairs = tour.app.scrollViews["menu-pairs"]
        XCTAssertTrue(pairs.waitForExistence(timeout: 5))
        let quit = tour.app.buttons["Quit Foldover"]
        XCTAssertTrue(quit.isHittable, "Commands must stay visible above a long catalog")
        for _ in 0..<5 where !tour.app.staticTexts["Pair 40"].isHittable {
            pairs.scroll(byDeltaX: 0, deltaY: -400)
            tour.pause(0.2)
        }
        XCTAssertTrue(tour.app.staticTexts["Pair 40"].isHittable)
        XCTAssertTrue(quit.isHittable, "Scrolling pairs must not move the commands")
        tour.snap("menu-bar-many-pairs")
        tour.app.terminate()
    }

    @MainActor
    func testAWorkingPairCanBeStoppedFromTheMenuBar() {
        let tour = Tour(self, appearance: .light, scenario: "syncing")
        tour.launch()
        tour.click(tour.app.statusItems.firstMatch, "menu bar extra")
        let stop = tour.app.buttons["Stop Photos"]
        XCTAssertTrue(stop.waitForExistence(timeout: 5))
        tour.click(stop, "stop Photos")
        XCTAssertTrue(tour.app.buttons["Sync Photos"].waitForExistence(timeout: 5))
        tour.snap("menu-bar-stopped")
        tour.app.terminate()
    }

    @MainActor
    func testTheMenuBarPulsesWithEveryWindowClosed() {
        let tour = Tour(self, appearance: .light, scenario: "syncing")
        let item = backgroundStatusItem(tour)

        let first = item.screenshot()
        attach(first, name: "light-menu-bar-pulse-first")
        var changed = false
        for _ in 0..<4 {
            tour.pause(0.45)
            let next = item.screenshot()
            if next.pngRepresentation == first.pngRepresentation { continue }
            attach(next, name: "light-menu-bar-pulse-next")
            changed = true
            break
        }
        XCTAssertTrue(changed, "The native status item should advance frames without an open window")
        tour.app.terminate()
    }

    @MainActor
    func testReducedMotionKeepsTheWorkingIconStill() {
        let tour = Tour(self, appearance: .light, scenario: "syncing-reduced")
        let item = backgroundStatusItem(tour)
        let first = item.screenshot()
        tour.pause(1.7)
        let next = item.screenshot()
        XCTAssertEqual(next.pngRepresentation, first.pngRepresentation)
        attach(next, name: "light-menu-bar-reduced-motion")
        tour.app.terminate()
    }

    @MainActor
    func testTheErrorIsColorfulInBothAppearances() throws {
        for appearance in [TourAppearance.light, .dark] {
            let tour = Tour(self, appearance: appearance, scenario: "agent-failed")
            tour.launch()
            let item = tour.app.statusItems.firstMatch
            XCTAssertTrue(item.waitForExistence(timeout: 10))
            let shot = item.screenshot()
            let pixels = try XCTUnwrap(NSBitmapImageRep(data: shot.pngRepresentation))
            let hasRed = (0..<pixels.pixelsHigh).contains { y in
                (0..<pixels.pixelsWide).contains { x in
                    guard let color = pixels.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB) else {
                        return false
                    }
                    return color.redComponent > color.greenComponent + 0.3
                        && color.redComponent > color.blueComponent + 0.3
                }
            }
            XCTAssertTrue(hasRed, "The native status item must preserve the error color")
            tour.click(item, "error menu bar extra")
            tour.snap("menu-bar-error")
            tour.app.terminate()
        }
    }

    @MainActor
    private func backgroundStatusItem(_ tour: Tour) -> XCUIElement {
        tour.launch()
        // MenuBarExtra's Label title is not a status-item query identifier.
        let item = tour.app.statusItems.firstMatch
        XCTAssertTrue(item.waitForExistence(timeout: 10))
        tour.click(item, "working menu bar extra")
        let working = tour.app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@", "Syncing…")).firstMatch
        XCTAssertTrue(working.waitForExistence(timeout: 5), "The fixture should have a running sync")
        tour.key(.escape)
        tour.key("w", .command)
        XCTAssertTrue(tour.app.windows["Foldover"].waitForNonExistence(timeout: 5))
        return item
    }

    @MainActor
    private func attach(_ shot: XCUIScreenshot, name: String) {
        let attachment = XCTAttachment(screenshot: shot)
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
