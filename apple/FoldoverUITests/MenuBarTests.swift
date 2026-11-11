import AppKit
import XCTest

final class MenuBarTests: XCTestCase {
    private static let initialPairStateTimeout: TimeInterval = 30

    @MainActor
    func testManyPairsScrollWithoutHidingTheCommands() {
        checkManyPairs()
    }

    @MainActor
    func testManyPairsKeepTheCommandsAtLargerTextSizes() {
        checkManyPairs(launchArguments: ["-foldover-large-menu-text", "YES"])
    }

    @MainActor
    private func checkManyPairs(launchArguments: [String] = []) {
        let tour = Tour(self, appearance: .light, scenario: "many-pairs")
        tour.app.launchArguments += launchArguments
        tour.launch()
        tour.click(tour.app.statusItems.firstMatch, "menu bar extra")
        let pairs = tour.app.scrollViews["menu-pairs"]
        XCTAssertTrue(pairs.waitForExistence(timeout: 5))
        // SwiftUI exposes the combined name/status label as a button.
        let lastPair = pairs.buttons["menu-pair-tour-menu-40"]
        // Hittable alone can include a row outside SwiftUI's clipped viewport.
        let lastPairIsVisible = {
            lastPair.exists && lastPair.isHittable && pairs.frame.contains(lastPair.frame)
        }
        let quit = tour.app.buttons["Quit Foldover"]
        XCTAssertTrue(quit.waitForExistence(timeout: 5))
        XCTAssertTrue(quit.isHittable, "Commands must stay visible above a long catalog")
        // A step larger than the viewport can jump past the target row.
        let scrollStep = pairs.frame.height / 2
        let maximumScrolls = 40
        for _ in 0..<maximumScrolls where !lastPairIsVisible() {
            pairs.scroll(byDeltaX: 0, deltaY: -scrollStep)
            tour.pause(0.2)
        }
        XCTAssertTrue(lastPairIsVisible(), "\(lastPair.debugDescription), viewport: \(pairs.frame)")
        XCTAssertTrue(lastPair.label.contains("Pair 40"), lastPair.debugDescription)
        XCTAssertTrue(quit.isHittable, "Scrolling pairs must not move the commands")
        tour.snap(launchArguments.isEmpty ? "menu-bar-many-pairs" : "menu-bar-many-pairs-large-text")
        tour.app.terminate()
    }

    @MainActor
    func testEnabledCommandsLookDifferentFromDisabledSyncsInBothAppearances() throws {
        for appearance in [TourAppearance.light, .dark] {
            let tour = Tour(self, appearance: appearance, scenario: "syncing-single-pair")
            tour.launch()
            defer { tour.app.terminate() }
            tour.click(tour.app.statusItems.firstMatch, "menu bar extra")
            let stop = tour.app.buttons["Stop Photos"]
            XCTAssertTrue(stop.waitForExistence(timeout: Self.initialPairStateTimeout))
            let pair = tour.app.buttons["menu-pair-f0000000000000000000000000000001"]
            let primaryContrast = try inkContrast(pair)
            let syncAll = tour.app.buttons["Sync All"]
            XCTAssertTrue(syncAll.exists)
            XCTAssertFalse(syncAll.isEnabled, "The only pair is already syncing")
            let disabledContrast = try inkContrast(syncAll)
            let pause = tour.app.descendants(matching: .any)["menu-pause-automatic-syncs"].firstMatch

            for title in ["Stop Photos", "Pause Automatic Syncs", "Open Foldover", "Settings…", "Quit Foldover"] {
                let command = title == "Pause Automatic Syncs" ? pause : tour.app.buttons[title]
                XCTAssertTrue(command.exists, title)
                XCTAssertTrue(command.isEnabled, title)
                XCTAssertTrue(command.isHittable, title)
                let contrast = try inkContrast(command)
                // Glyph coverage and the panel's material vary across rows.
                XCTAssertGreaterThan(contrast, primaryContrast * 0.8,
                                     "\(title) should use primary text in \(appearance.rawValue) Mode")
                XCTAssertGreaterThan(contrast, disabledContrast * 1.25,
                                     "\(title) should not look disabled in \(appearance.rawValue) Mode")
            }
            tour.snap("menu-bar-command-contrast")
            tour.click(pause, "automatic sync pause menu")
            tour.click(pause.menuItems["For 1 Hour"], "pause for one hour")
            let resume = tour.app.buttons["Resume Automatic Syncs"]
            XCTAssertTrue(resume.waitForExistence(timeout: 5))
            XCTAssertTrue(resume.isEnabled)
            XCTAssertGreaterThan(try inkContrast(resume), primaryContrast * 0.8,
                                 "Resume Automatic Syncs should use primary text")
        }
    }

    /// Measure rendered ink, not just accessibility's enabled state. Comparing
    /// with the pair's primary label avoids hard-coded system text colors.
    @MainActor
    private func inkContrast(_ element: XCUIElement) throws -> CGFloat {
        let pixels = try XCTUnwrap(NSBitmapImageRep(data: element.screenshot().pngRepresentation))
        var darkest: CGFloat = 1
        var lightest: CGFloat = 0
        for y in 0..<pixels.pixelsHigh {
            for x in 0..<pixels.pixelsWide {
                let color = try XCTUnwrap(pixels.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB))
                let luminance = 0.2126 * color.redComponent + 0.7152 * color.greenComponent
                    + 0.0722 * color.blueComponent
                darkest = min(darkest, luminance)
                lightest = max(lightest, luminance)
            }
        }
        return lightest - darkest
    }

    @MainActor
    func testAWorkingPairCanBeStoppedFromTheMenuBar() {
        let tour = Tour(self, appearance: .light, scenario: "syncing")
        tour.launch()
        defer { tour.app.terminate() }
        tour.click(tour.app.statusItems.firstMatch, "menu bar extra")
        let stop = tour.app.buttons["Stop Photos"]
        // Pair status can arrive after the window and agent health answer.
        guard tour.click(stop, "stop Photos", timeout: Self.initialPairStateTimeout) else { return }
        XCTAssertTrue(tour.app.buttons["Sync Photos"].waitForExistence(timeout: 5))
        tour.snap("menu-bar-stopped")
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
