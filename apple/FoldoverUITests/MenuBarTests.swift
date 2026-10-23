import AppKit
import XCTest

final class MenuBarTests: XCTestCase {
    @MainActor
    func testTheMenuBarPulsesWithEveryWindowClosed() {
        let tour = Tour(self, appearance: .light, scenario: "syncing")
        tour.launch()
        // MenuBarExtra's Label title is not a status-item query identifier.
        let item = tour.app.statusItems.firstMatch
        XCTAssertTrue(item.waitForExistence(timeout: 10))
        tour.key("w", .command)
        XCTAssertTrue(tour.app.windows["Foldover"].waitForNonExistence(timeout: 5))

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
    private func attach(_ shot: XCUIScreenshot, name: String) {
        let attachment = XCTAttachment(screenshot: shot)
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
