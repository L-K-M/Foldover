// The fold in motion, in slow motion: a line turned with the keyboard,
// the leaf let go short of upright and falling back, the leaf folded
// past upright, pressing, opening on the run's imprint, and the imprint
// lifted by Undo. Each stretch of motion keeps a run of frames. Runs
// with nothing previewed on the sheet fold a whole page too.

import XCTest

final class FoldTourTests: XCTestCase {
    /// Eight times slower: the fall takes about 3.4 s and the run 24 s.
    private static let slowMotion = 8

    @MainActor
    func testCompletedFailuresRemainInspectable() {
        let tour = Tour(self, appearance: .light, scenario: "failed-run")
        let app = tour.app
        tour.launch()
        defer { app.terminate() }
        tour.click(app.outlines.firstMatch.staticTexts["Photos"].firstMatch, "Photos in the sidebar")

        let failure = tour.line("2026/Trip/notes.md")
        guard tour.click(failure, "completed failure") else { return }
        XCTAssertEqual(failure.value as? String, "The server disconnected.", failure.debugDescription)
        XCTAssertTrue(app.windows.staticTexts["The server disconnected."].firstMatch.waitForExistence(timeout: 5),
                      "Selecting a completed failure must show its explanation in the inspector")
        tour.snap("fold-failure-selected")

        tour.key(.upArrow)
        XCTAssertTrue(app.windows.staticTexts["Permission denied."].firstMatch.waitForExistence(timeout: 5),
                      "Keyboard selection must work on completed failures")
    }

    /// A window opened on a run already under way, as after a schedule
    /// or the menu bar started it: nothing was previewed, so nothing is
    /// listed. The leaf still lies over a whole page, its back reading
    /// out the run's stage. Each folder stays named on its own side: a
    /// two-way sync's Folder A on the bare desk, under two arrows passing,
    /// and its server, which has no disk to read, on the leaf.
    @MainActor
    func testARunOpenedMidwayCoversThePage() {
        continueAfterFailure = true
        for appearance in [TourAppearance.light, .dark] {
            for (scenario, stage) in [("syncing", "Comparing"), ("syncing-waiting", "Waiting")] {
                let tour = Tour(self, appearance: appearance, scenario: scenario)
                tour.launch()
                tour.click(tour.app.outlines.firstMatch.staticTexts["Photos"].firstMatch, "Photos in the sidebar")
                checkPlate(tour, reads: stage)
                checkFolders(tour, desk: "Folder A: ", leaf: "Folder B: sftp://mynas.local/~/Photos",
                             direction: "Sync both ways")
                tour.snap("fold-midway-\(stage.lowercased())")
                tour.app.terminate()
            }
        }
    }

    /// A backup to a disk opened mid-run reads left to right: the folder
    /// it is copied from on the bare desk, an arrow toward the leaf, and
    /// on the leaf the folder it is copied to, with that disk's free space
    /// and what is still to copy there.
    @MainActor
    func testABackupOpenedMidwayNamesBothFoldersAndTheDisk() {
        continueAfterFailure = true
        for appearance in [TourAppearance.light, .dark] {
            let tour = Tour(self, appearance: appearance, scenario: "syncing-backup")
            tour.launch()
            tour.click(tour.app.outlines.firstMatch.staticTexts["Projects"].firstMatch, "Projects in the sidebar")
            checkPlate(tour, reads: "Syncing")
            checkFolders(tour, desk: "From Folder A: ", leaf: "To Folder B: ", direction: "Back up A to B")
            let plate = tour.app.windows.descendants(matching: .any)["fold-plate"].firstMatch
            let disk = plate.descendants(matching: .any)["fold-disk"].firstMatch
            XCTAssertTrue(disk.waitForExistence(timeout: 10), "The leaf should read out the backup disk")
            XCTAssertEqual(disk.label, "Backup SSD, 812.4 GB free of 2 TB, 23.2 GB still to copy")
            tour.snap("fold-midway-backup")
            Stress(tour: tour, scenario: "syncing-backup").narrowToMinimum()
            tour.pause(1)
            // At the minimum with both side panes, the sheet may drop
            // storage to keep sync progress readable. Give it room again.
            XCTAssertTrue(plate.descendants(matching: .any)["fold-readout"].firstMatch.exists)
            tour.snap("fold-midway-backup-narrow")
            tour.key("i", [.command, .control])
            XCTAssertTrue(disk.waitForExistence(timeout: 5), "Storage should return when the inspector is hidden")
            XCTAssertTrue(plate.frame.insetBy(dx: -1, dy: -1).contains(disk.frame),
                          "The storage readout should stay inside the leaf: \(disk.frame) in \(plate.frame)")
            tour.snap("fold-midway-backup-narrow-storage")
            tour.app.terminate()
        }
    }

    /// The same backup made from B to A folds from the right: the bare
    /// desk on the right names Folder B with its arrow pointing left, and
    /// the leaf on the left names Folder A and its disk.
    @MainActor
    func testABackupFromBToAReadsRightToLeft() {
        continueAfterFailure = true
        let tour = Tour(self, appearance: .light, scenario: "syncing-backup-right")
        tour.launch()
        defer { tour.app.terminate() }
        tour.click(tour.app.outlines.firstMatch.staticTexts["Projects"].firstMatch, "Projects in the sidebar")
        checkPlate(tour, reads: "Syncing")
        checkFolders(tour, desk: "From Folder B: ", leaf: "To Folder A: ", direction: "Back up B to A",
                     deskSide: .right)
        let plate = tour.app.windows.descendants(matching: .any)["fold-plate"].firstMatch
        XCTAssertTrue(plate.descendants(matching: .any)["fold-disk"].firstMatch.waitForExistence(timeout: 10),
                      "The leaf should read out the disk it lies on")
        tour.snap("fold-midway-backup-right")
    }

    /// Which half the bare desk is: the half the leaf lifted off.
    private enum DeskSide { case left, right }

    /// Each folder is named on its own side while the leaf lies folded:
    /// the bare desk names the folder that lifted off, with the arrow for
    /// which way the data goes, and the leaf's plate the folder it lies
    /// on. The desk's words keep to its page, clear of the plate.
    @MainActor
    private func checkFolders(_ tour: Tour, desk: String, leaf: String, direction: String,
                              deskSide: DeskSide = .left) {
        let window = tour.app.windows.firstMatch
        let plate = window.descendants(matching: .any)["fold-plate"].firstMatch
        let note = window.descendants(matching: .any)["fold-desk"].firstMatch
        guard note.waitForExistence(timeout: 5) else { return XCTFail("Missing: the bare desk's note") }
        let lifted = note.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH %@", desk))
        XCTAssertTrue(lifted.firstMatch.exists, "The bare desk should name the folder that lifted off, \(desk)")
        let arrow = note.descendants(matching: .any)["fold-direction"].firstMatch
        XCTAssertEqual(arrow.exists ? arrow.label : nil, direction, "The desk's arrow should say the sync's kind")
        let under = plate.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH %@", leaf))
        XCTAssertTrue(under.firstMatch.exists, "The leaf should name the folder it lies on, \(leaf)")
        switch deskSide {
        case .left:
            XCTAssertLessThanOrEqual(note.frame.maxX, plate.frame.minX + 1,
                                     "The bare desk's words should stay off the plate: \(note.frame), \(plate.frame)")
        case .right:
            XCTAssertGreaterThanOrEqual(note.frame.minX, plate.frame.maxX - 1,
                                        "The bare desk's words should stay off the plate: \(note.frame), \(plate.frame)")
        }
    }

    /// Sync Now before any preview folds a blank half; its back reads
    /// the run comparing, then syncing, and the leaf opens on the imprint.
    /// In slow motion the leaf lands about 3 s into the run's 8 s of
    /// comparing, so the first check has room to see it.
    @MainActor
    func testSyncBeforeAnyPreviewCoversThePage() {
        continueAfterFailure = true
        let tour = Tour(self, appearance: .light, slowFold: Self.slowMotion)
        let app = tour.app
        tour.launch()
        defer { app.terminate() }
        tour.click(app.outlines.firstMatch.staticTexts["Photos"].firstMatch, "Photos in the sidebar")
        tour.key("s", .command)
        checkPlate(tour, reads: "Comparing")
        tour.snap("fold-unpreviewed-comparing")
        checkPlate(tour, reads: "Syncing", timeout: 20)
        // The count dissolves into the share as the stage changes.
        tour.pause(1)
        tour.snap("fold-unpreviewed-syncing")
        XCTAssertTrue(app.buttons["Undo Sync"].waitForExistence(timeout: 60), "The run should end on its imprint")
        // In slow motion the peel takes about 3.4 s and the stamp 1.8 s.
        tour.pause(6)
        tour.snap("fold-unpreviewed-imprint")
    }

    /// The plate on the folded leaf's back covers its page and reads out
    /// `stage`. With nothing listed, the sheet once shrank to its folder
    /// labels, leaving the plate a card at the top of a bare field.
    @MainActor
    private func checkPlate(_ tour: Tour, reads stage: String, timeout: TimeInterval = 10) {
        let app = tour.app
        let plate = app.windows.descendants(matching: .any)["fold-plate"].firstMatch
        let readout = plate.descendants(matching: .any)["fold-readout"].firstMatch
        guard readout.waitForExistence(timeout: timeout) else { return XCTFail("Missing: the leaf's readout") }
        let deadline = Date(timeIntervalSinceNow: timeout)
        var label = readout.label
        while !label.hasPrefix(stage), Date() < deadline {
            tour.pause(0.25)
            label = readout.label
        }
        XCTAssertTrue(label.hasPrefix(stage), "The leaf should read \(stage): \(label)")
        let window = app.windows.firstMatch.frame
        XCTAssertGreaterThan(plate.frame.height, window.height * 0.6,
                             "The plate should cover its page while \(stage): \(plate.frame) in \(window)")
    }

    @MainActor
    func testFoldWaitsForApproval() {
        checkApprovalHold(motion: .full)
    }

    @MainActor
    func testReducedMotionFoldWaitsForApproval() {
        checkApprovalHold(motion: .reduced)
    }

    private enum Motion { case full, reduced }

    @MainActor
    private func checkApprovalHold(motion: Motion) {
        let tour = Tour(self, appearance: .light, scenario: "fold-approval")
        let app = tour.app
        switch motion {
        case .full: app.launchArguments += ["-foldover-full-motion", "YES"]
        case .reduced: app.launchArguments += ["-foldover-reduced-motion", "YES"]
        }
        tour.launch()
        defer { app.terminate() }
        tour.click(app.outlines.firstMatch.staticTexts["Photos"].firstMatch, "Photos in the sidebar")

        let line = tour.line("2026/Trip/notes.md")
        XCTAssertTrue(line.waitForExistence(timeout: 5))
        let ear = app.windows.descendants(matching: .any)["Fold Folder A’s Half to Sync"].firstMatch
        guard ear.waitForExistence(timeout: 5) else { return XCTFail("Missing fold handle") }
        let corner = ear.coordinate(withNormalizedOffset: CGVector(dx: 0.25, dy: 0.25))
        corner.click(forDuration: 0.2, thenDragTo: corner.withOffset(CGVector(dx: line.frame.width * 0.45, dy: 0)))

        let approve = app.buttons["Archive and Sync"].firstMatch
        XCTAssertTrue(approve.waitForExistence(timeout: 5), "The fold must ask before applying the plan")
        tour.snap(motion == .full ? "fold-held-for-approval" : "fold-reduced-motion-approval")
        tour.key(.escape)
        XCTAssertTrue(line.exists, "Cancelling approval must keep the reviewed plan")
        XCTAssertFalse(app.buttons["Undo Sync"].exists, "Cancelling must not start a run")
    }

    @MainActor
    func testFoldInMotion() {
        continueAfterFailure = true
        let tour = Tour(self, appearance: .light, slowFold: Self.slowMotion)
        let app = tour.app
        tour.launch()
        defer { app.terminate() }
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
        guard half > 100 else { return XCTFail("The sheet should be wide enough to fold, half is \(half)") }
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
    }
}
