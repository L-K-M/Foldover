import XCTest

final class FullDiskAccessReminderTests: XCTestCase {
    @MainActor
    func testReminderSurvivesResetAndCanBeDismissed() {
        let tour = Tour(self, appearance: .light)
        tour.app.launchArguments.append("-foldover-access-reminder")
        tour.launch()
        let app = tour.app
        let reminder = app.otherElements["full-disk-access-reminder"]
        XCTAssertTrue(app.staticTexts["Review Full Disk Access"].waitForExistence(timeout: 10))
        attachReminder(app, name: "full-disk-access-reminder")
        // Fixture actions are inert: no TCC reset or System Settings launch.
        app.buttons["Reset and Open Settings…"].click()
        XCTAssertTrue(app.buttons["Not Now"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Review Full Disk Access"].exists)
        let stress = Stress(tour: tour, scenario: "access-reminder")
        XCTAssertTrue(stress.step("narrow", { stress.narrowToMinimum() }))
        attachReminder(app, name: "full-disk-access-reminder-narrow")
        app.buttons["Not Now"].click()
        XCTAssertFalse(reminder.exists)
        XCTAssertFalse(app.staticTexts["Review Full Disk Access"].exists)
        app.terminate()
    }

    @MainActor
    private func attachReminder(_ app: XCUIApplication, name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
