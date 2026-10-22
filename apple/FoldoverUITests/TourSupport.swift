// Shared steps for the UI tour: launch the Debug app on its canned
// agent answers and keep a screenshot of each stop. Screenshots go to
// the folder in FOLDOVER_TOUR_DIR (xcodebuild passes it on from
// TEST_RUNNER_FOLDOVER_TOUR_DIR) and into the result bundle.

import XCTest

/// The system appearance a tour runs in.
enum TourAppearance: String {
    case light = "Light"
    case dark = "Dark"
}

@MainActor
struct Tour {
    let app = XCUIApplication()
    let test: XCTestCase
    let appearance: TourAppearance

    init(_ test: XCTestCase, appearance: TourAppearance) {
        self.test = test
        self.appearance = appearance
        app.launchArguments = ["-foldover-fixtures", "-foldover-skip-update-check",
                               "-ApplePersistenceIgnoreState", "YES",
                               "-AppleInterfaceStyle", appearance == .dark ? "Dark" : "Light"]
    }

    func launch() {
        app.launch()
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 30), "No window appeared")
        pause(1)
    }

    /// Let animations and polled state settle before a screenshot.
    func pause(_ seconds: TimeInterval) {
        RunLoop.current.run(until: Date(timeIntervalSinceNow: seconds))
    }

    /// Keep what the screen shows now, named for this stop.
    func snap(_ name: String) {
        let shot = XCUIScreen.main.screenshot()
        let fileName = "\(appearance.rawValue.lowercased())-\(name)"
        let attachment = XCTAttachment(screenshot: shot)
        attachment.name = fileName
        attachment.lifetime = .keepAlways
        test.add(attachment)
        guard let folder = ProcessInfo.processInfo.environment["FOLDOVER_TOUR_DIR"] else { return }
        let url = URL(fileURLWithPath: folder).appendingPathComponent("\(fileName).png")
        try? shot.pngRepresentation.write(to: url)
    }

    /// Click `element` when it appears; a missing one fails the step but
    /// lets the tour go on to its next stop.
    @discardableResult
    func click(_ element: XCUIElement, _ what: String, timeout: TimeInterval = 5) -> Bool {
        guard element.waitForExistence(timeout: timeout) else {
            XCTFail("Missing: \(what)")
            return false
        }
        element.click()
        pause(0.6)
        return true
    }

    func key(_ key: XCUIKeyboardKey, _ modifiers: XCUIElement.KeyModifierFlags = []) {
        app.typeKey(key, modifierFlags: modifiers)
        pause(0.6)
    }

    func key(_ key: String, _ modifiers: XCUIElement.KeyModifierFlags = []) {
        app.typeKey(key, modifierFlags: modifiers)
        pause(0.6)
    }

    /// Open a menu-bar menu, keep a screenshot of it, and close it.
    func snapMenu(_ title: String) {
        let item = app.menuBars.menuBarItems[title]
        guard click(item, "\(title) menu") else { return }
        snap("menu-\(title.lowercased())")
        key(.escape)
    }
}
