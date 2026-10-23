// A tour of the app's main workflows on canned agent answers, driven
// with the keyboard and mouse like a person would, keeping a screenshot
// of every stop: `xcodebuild test -scheme FoldoverUITour`. The light
// tour walks every workflow; the dark one revisits the main surfaces.

import XCTest

final class TourTests: XCTestCase {
    @MainActor
    func testTourLight() {
        continueAfterFailure = true
        let tour = Tour(self, appearance: .light)
        tour.launch()
        tour.snap("01-launch")
        reviewAndSync(tour)
        editAndArchive(tour)
        otherPairs(tour)
        settingsAndMenuBar(tour)
        windows(tour)
        tour.app.terminate()
    }

    @MainActor
    func testTourDark() {
        continueAfterFailure = true
        let tour = Tour(self, appearance: .dark)
        let app = tour.app
        tour.launch()
        tour.click(sidebarRow(app, "Photos"), "Photos in the sidebar")
        tour.key("r", .command)
        tour.pause(5)
        tour.click(text(app, "2026/Trip/header.txt"), "header.txt change")
        tour.snap("01-preview")
        tour.key("i", .command)
        tour.pause(1)
        checkPairLayout(tour)
        tour.snap("02-editor")
        checkInfoPopover(tour, button: app.sheets.buttons["About Folders"].firstMatch,
                         contains: "open a shared folder in Finder", snap: "02-folders-help")
        tour.key(.escape)
        tour.key("a", [.command, .shift])
        tour.pause(1.5)
        tour.snap("03-archive")
        tour.key("w", .command)
        tour.key(",", .command)
        tour.pause(1.5)
        tour.snap("04-settings")
        tour.key("w", .command)
        tour.click(app.statusItems.firstMatch, "menu bar extra")
        tour.pause(1)
        tour.snap("05-menu-bar-extra")
        tour.key(.escape)
        app.terminate()
    }

    @MainActor
    private func sidebarRow(_ app: XCUIApplication, _ name: String) -> XCUIElement {
        app.outlines.firstMatch.staticTexts[name].firstMatch
    }

    /// A row of a table found by the text it shows, such as a path.
    @MainActor
    private func text(_ app: XCUIApplication, _ label: String) -> XCUIElement {
        app.windows.staticTexts[label].firstMatch
    }

    /// The folders share a row, with the mode between them and actions visible.
    @MainActor
    private func checkPairLayout(_ tour: Tour) {
        let sheet = tour.app.sheets.firstMatch
        let left = sheet.descendants(matching: .any)["Choose Folder A"].firstMatch
        let right = sheet.descendants(matching: .any)["Choose Folder B"].firstMatch
        let mode = sheet.popUpButtons["pair-mode"].firstMatch
        for control in [left, mode, right, sheet.buttons["Cancel"], sheet.buttons["Save Pair"]] {
            guard control.waitForExistence(timeout: 5) else {
                return XCTFail("Missing pair editor control: \(control)")
            }
            XCTAssertTrue(sheet.frame.contains(control.frame), "Pair editor control is clipped: \(control)")
        }
        XCTAssertGreaterThan(sheet.frame.width, sheet.frame.height, "The pair editor should be landscape")
        XCTAssertEqual(left.frame.midY, right.frame.midY, accuracy: 1, "The folders should share a row")
        XCTAssertLessThan(left.frame.maxX, mode.frame.minX, "Mode should follow Folder A")
        XCTAssertLessThan(mode.frame.maxX, right.frame.minX, "Mode should precede Folder B")
        XCTAssertTrue(left.isHittable, "Folder A should be hittable")
        XCTAssertTrue(mode.isHittable, "Mode picker should be hittable")
        XCTAssertTrue(right.isHittable, "Folder B should be hittable")
        XCTAssertEqual(mode.label, "Mode", "The hidden label should remain accessible")
    }

    /// Guidance appears only on request; Escape closes help, not its parent.
    @MainActor
    private func checkInfoPopover(_ tour: Tour, button: XCUIElement,
                                  contains fragment: String, snap name: String) {
        let message = tour.app.staticTexts["info-message"].firstMatch
        XCTAssertFalse(message.exists, "Help should stay out of the form until requested")
        guard tour.click(button, "info button") else { return }
        guard message.waitForExistence(timeout: 5) else {
            return XCTFail("The info button did not show help")
        }
        // Selectable macOS text exposes its contents as AXValue.
        let content = message.value as? String ?? message.label
        XCTAssertTrue(content.contains(fragment), "Help should explain the associated setting: \(content)")
        let popover = tour.app.popovers.firstMatch
        XCTAssertTrue(popover.waitForExistence(timeout: 5), "Help should use a native popover")
        if popover.exists {
            XCTAssertTrue(popover.frame.contains(message.frame), "Help text should not be clipped")
        }
        tour.snap(name)
        tour.key(.escape)
        XCTAssertTrue(message.waitForNonExistence(timeout: 5), "Escape should dismiss help")
        XCTAssertTrue(button.waitForExistence(timeout: 5) && button.isHittable,
                      "Escape should close help, not its parent")
    }

    /// Whether the menu-bar menu `menu` offers `item` enabled; the menu
    /// is opened, captured and closed.
    @MainActor
    @discardableResult
    private func checkMenu(_ tour: Tour, _ menu: String, _ item: String, enabled: Bool,
                           snap name: String? = nil) -> Bool {
        let app = tour.app
        guard tour.click(app.menuBars.menuBarItems[menu], "\(menu) menu") else { return false }
        // By prefix: titles like Undo's name the pair in typographic quotes.
        let element = app.menuBars.menuItems
            .matching(NSPredicate(format: "title BEGINSWITH %@", item)).firstMatch
        let found = element.waitForExistence(timeout: 3)
        XCTAssertTrue(found, "\(menu) › \(item) is missing")
        if found {
            XCTAssertEqual(element.isEnabled, enabled, "\(menu) › \(item) enabled")
        }
        if let name { tour.snap(name) }
        tour.key(.escape)
        return found
    }

    /// Preview, look at changes, filter, search, sync and undo it.
    @MainActor
    private func reviewAndSync(_ tour: Tour) {
        let app = tour.app
        checkMenu(tour, "Pair", "Sync Now", enabled: false, snap: "menu-pair-no-selection")

        tour.click(sidebarRow(app, "Photos"), "Photos in the sidebar")
        tour.pause(1)
        tour.snap("02-photos")

        tour.key("r", .command)
        tour.pause(1)
        tour.snap("03-previewing")
        tour.pause(4)
        tour.snap("04-preview")
        checkMenu(tour, "Pair", "Sync Now", enabled: true, snap: "menu-pair")

        tour.click(text(app, "2026/Trip/header.txt"), "header.txt change")
        tour.snap("05-change-selected")
        checkMenu(tour, "Pair", "Keep Folder A’s Version", enabled: true)

        tour.key(.space)
        tour.pause(1.5)
        tour.snap("06-quick-look")
        tour.key(.escape)

        let row = text(app, "2026/Trip/day2/IMG_2041.txt")
        if row.waitForExistence(timeout: 3) {
            row.rightClick()
            tour.pause(0.6)
            tour.snap("07-change-menu")
            tour.key(.escape)
        } else {
            XCTFail("Missing: IMG_2041 change")
        }

        tour.key("5", .command)
        tour.pause(1)
        tour.snap("08-conflicts")
        tour.key("1", .command)
        tour.pause(1)

        tour.key("f", .command)
        app.typeText("IMG")
        tour.pause(1)
        tour.snap("09-search")
        tour.key("a", .command)
        tour.key(.delete)
        tour.key(.escape)

        tour.key("s", .command)
        tour.pause(1)
        tour.snap("10-syncing")
        tour.pause(4)
        tour.snap("11-synced")

        // AppKit retitles Undo when the menu opens, after the query reads
        // it, so the item is found by its action.
        if tour.click(app.menuBars.menuBarItems["Edit"], "Edit menu") {
            let undo = app.menuBars.menuItems["undo:"].firstMatch
            XCTAssertTrue(undo.waitForExistence(timeout: 3) && undo.isEnabled, "Edit › Undo is enabled")
            tour.snap("menu-edit-undo")
            tour.key(.escape)
        }
        tour.key("z", .command)
        tour.pause(2)
        tour.snap("12-undone")
    }

    /// Edit the pair (typing and native text undo), then the archive.
    @MainActor
    private func editAndArchive(_ tour: Tour) {
        let app = tour.app
        tour.key("i", .command)
        tour.pause(1)
        checkPairLayout(tour)
        tour.snap("13-editor")
        checkInfoPopover(tour, button: app.sheets.buttons["About Folders"].firstMatch,
                         contains: "open a shared folder in Finder", snap: "13-folders-help")
        checkInfoPopover(tour, button: app.sheets.buttons["About Sync Mode"].firstMatch,
                         contains: "Use Mirror for an exact one-way copy", snap: "13-mode-help")
        let name = app.sheets.firstMatch.textFields.firstMatch
        if tour.click(name, "Name field") {
            tour.key(.rightArrow, .command)
            name.typeText(" Library")
            tour.snap("14-editor-typed")
            tour.key("z", .command)
            XCTAssertEqual(name.value as? String, "Photos", "Undo restores the typed name")
        }
        tour.key(.escape)
        tour.pause(1)

        tour.key("a", [.command, .shift])
        tour.pause(1.5)
        tour.snap("15-archive")
        checkInfoPopover(tour, button: app.buttons["About Archive"].firstMatch,
                         contains: "not a separate backup", snap: "15-archive-help")
        let folderB = app.toolbars.buttons["Folder B"].firstMatch
        if folderB.waitForExistence(timeout: 3) {
            folderB.click()
        } else {
            tour.click(app.radioButtons["Folder B"].firstMatch, "Folder B segment")
        }
        tour.pause(1.5)
        tour.snap("16-archive-folder-b")
        tour.click(text(app, "2024/drafts/old-banner.psd"), "first archived item")
        tour.click(app.toolbars.buttons["Put Back"].firstMatch, "Put Back button")
        tour.pause(1)
        tour.snap("17-archive-put-back")
        tour.key("w", .command)
        tour.pause(1)
    }

    /// The other pairs' states, removal by Delete, and a new pair.
    @MainActor
    private func otherPairs(_ tour: Tour) {
        let app = tour.app
        tour.click(sidebarRow(app, "Music"), "Music in the sidebar")
        tour.pause(1)
        tour.snap("18-music")

        tour.click(sidebarRow(app, "Projects"), "Projects in the sidebar")
        tour.pause(1)
        tour.snap("19-projects")

        // Choosing a pair moves the keys to the sidebar, so Delete removes
        // it (after asking) instead of editing the search field.
        checkMenu(tour, "Edit", "Delete", enabled: true)
        tour.key(.delete)
        tour.pause(1)
        tour.snap("20-remove-confirmation")
        tour.key(.escape)

        tour.key("n", .command)
        tour.pause(1)
        checkPairLayout(tour)
        tour.snap("21-new-pair")
        tour.key(.escape)

        tour.key("i", [.command, .control])
        tour.pause(1)
        tour.snap("22-inspector-hidden")
        tour.key("i", [.command, .control])
        tour.pause(1)
    }

    @MainActor
    private func settingsAndMenuBar(_ tour: Tour) {
        let app = tour.app
        tour.key(",", .command)
        tour.pause(1.5)
        tour.snap("23-settings-general")
        let checkbox = app.checkBoxes["Open Foldover at login"].firstMatch
        let login = checkbox.exists ? checkbox : app.switches["Open Foldover at login"].firstMatch
        guard login.waitForExistence(timeout: 5) else {
            return XCTFail("Missing labeled login switch: \(app.debugDescription)")
        }
        let loginValue = login.value as? String
        XCTAssertNotNil(loginValue, "The login switch should expose its state")
        checkInfoPopover(tour, button: app.buttons["About Open Foldover at login"].firstMatch,
                         contains: "background agent", snap: "23-login-help")
        XCTAssertEqual(login.value as? String, loginValue, "Opening help must not toggle the setting")
        tour.click(app.toolbars.buttons["Mac-to-Mac"].firstMatch, "Mac-to-Mac settings tab")
        tour.pause(1.5)
        tour.snap("24-settings-mac-to-mac")
        tour.key("w", .command)

        tour.click(app.statusItems.firstMatch, "menu bar extra")
        tour.pause(1)
        tour.snap("25-menu-bar-extra")
        tour.key(.escape)

        for menu in ["Foldover", "File", "Edit", "View", "Pair", "Window", "Help"] {
            tour.snapMenu(menu)
        }
    }

    /// Close the main window, reopen it from the Dock, and go full screen.
    @MainActor
    private func windows(_ tour: Tour) {
        let app = tour.app
        tour.key("w", .command)
        tour.pause(1)
        tour.snap("26-window-closed")
        XCTAssertTrue(app.menuBars.menuBarItems["Pair"].exists, "The app keeps running")
        // The Dock's own item if the query reaches it, else the menu bar
        // extra's Open Foldover; either must bring the window back.
        let dockItem = XCUIApplication(bundleIdentifier: "com.apple.dock")
            .descendants(matching: .any)["Foldover"].firstMatch
        if dockItem.waitForExistence(timeout: 3) {
            dockItem.click()
        } else if tour.click(app.statusItems.firstMatch, "menu bar extra") {
            tour.click(app.buttons["Open Foldover"].firstMatch, "Open Foldover")
        }
        tour.pause(1.5)
        tour.snap("27-window-reopened")
        XCTAssertTrue(app.windows.count > 0, "The window comes back")

        tour.key("f", [.command, .control])
        tour.pause(2.5)
        tour.snap("28-full-screen")
        tour.key("f", [.command, .control])
        tour.pause(2.5)
    }
}
