// Shared steps for the UI tour: launch the Debug app on its canned
// agent answers and keep a screenshot of each stop as a result-bundle
// attachment named "<appearance>-<stop>" (the ui-tour workflow
// exports them as PNGs under those names).

import XCTest

/// The system appearance a tour runs in.
enum TourAppearance: String {
    case light = "Light"
    case dark = "Dark"
}

/// What a launch brings back from the last one.
enum TourLaunchState {
    /// A fresh window: no restored windows or scene storage.
    case fresh
    /// The windows, selection and inspector the last run left, as at a
    /// person's launch.
    case restored
}

@MainActor
struct Tour {
    let app = XCUIApplication()
    let test: XCTestCase
    let appearance: TourAppearance

    /// `scenario` names a launch state from `FixtureScenario`, such as an
    /// agent that answers slowly; none is the tour's usual state.
    init(_ test: XCTestCase, appearance: TourAppearance, scenario: String? = nil,
         launchState: TourLaunchState = .fresh) {
        self.test = test
        self.appearance = appearance
        app.launchArguments = ["-foldover-fixtures", "-foldover-skip-update-check"]
        switch launchState {
        case .fresh:
            app.launchArguments += ["-ApplePersistenceIgnoreState", "YES", "-foldover-reset-defaults"]
        case .restored:
            // Restore after a quit too, not only after a crash or logout.
            app.launchArguments += ["-NSQuitAlwaysKeepsWindows", "YES"]
        }
        if appearance == .dark { app.launchArguments.append("-foldover-dark") }
        if let scenario { app.launchArguments += ["-foldover-scenario", scenario] }
    }

    func launch() {
        app.launch()
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 30), "No window appeared")
        showSidebarIfHidden()
        pause(1)
    }

    /// AppKit remembers a collapsed sidebar from one launch to the next,
    /// which the tours' saved-state flag does not cover; every tour starts
    /// with it shown.
    func showSidebarIfHidden() {
        guard !app.outlines.firstMatch.waitForExistence(timeout: 2) else { return }
        key("s", [.command, .control])
        // The first look can miss a sidebar that was shown but not yet
        // in the accessibility tree, and the toggle then hid it.
        if !app.outlines.firstMatch.waitForExistence(timeout: 2) {
            key("s", [.command, .control])
        }
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
