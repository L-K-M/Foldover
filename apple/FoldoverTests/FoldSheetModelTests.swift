// The fold's rules: which mark each half of a line carries and which
// page's margin is marked, what a line is called, which ways a person
// may turn it, how lines group under folders, which half folds, which
// page a message keeps to, how print gives way as the leaf turns, and
// what a working job reads out.

import Foundation
import Testing
@testable import Foldover

@MainActor
private func row(_ path: String, _ action: ChangeAction?, id: Int = 0, resolvable: Bool = false,
                 otherWay: FolderSide? = nil, movedFrom: String? = nil) -> ChangeRow {
    var row = ChangeRow(id: id, path: path, description: "", resolvable: resolvable ? Data(path.utf8) : nil)
    row.action = action
    row.otherWay = otherWay
    row.otherWayTitle = action.flatMap(PairSyncViewModel.otherWayTitle)
    row.movedFrom = movedFrom
    return row
}

@MainActor
struct FoldSheetModelTests {
    @Test func copiesInkTheSourceAndImpressTheDestination() {
        let toB = FoldLine(row: row("2026/Trip/day2/IMG_2041.RAW", .copy(to: .b)))
        #expect(toB.a == .ink)
        #expect(toB.b == .impression)
        #expect(toB.flow == .toB)
        #expect(toB.name == "IMG_2041.RAW")
        #expect(toB.parent == "2026/Trip/day2")

        let toA = FoldLine(row: row("notes.md", .copy(to: .a)))
        #expect(toA.a == .impression)
        #expect(toA.b == .ink)
        #expect(toA.flow == .toA)
        #expect(toA.parent.isEmpty)
    }

    @Test func archivesLiftFromTheFolderThatLosesThem() {
        let line = FoldLine(row: row("2024/drafts/old-banner.psd", .archive(on: .b)))
        #expect(line.a == .blank)
        #expect(line.b == .lift)
        // A's absence carries over to B.
        #expect(line.flow == .toB)
    }

    @Test func conflictsHoldBothInks() {
        let line = FoldLine(row: row("header.psd", .conflict(.bothChanged), resolvable: true))
        #expect(line.a == .ink)
        #expect(line.b == .ink)
        #expect(line.flow == .held)
        #expect(line.verb == "both changed")
        #expect(line.turns == [.toB: "Keep Folder A’s Version", .toA: "Keep Folder B’s Version"])
    }

    @Test func movesKeepTheirOldName() {
        let line = FoldLine(row: row("2026/Trip/day1/IMG_1990.HEIC", .move(on: .b),
                                     movedFrom: "2026/Inbox/IMG_1990.HEIC"))
        #expect(line.b == .impression)
        #expect(line.movedFrom == "2026/Inbox/IMG_1990.HEIC")
    }

    @Test func failuresPlanNothing() {
        let line = FoldLine(row: row("locked.txt", nil, id: -1))
        #expect(line.a == .blank)
        #expect(line.b == .blank)
        #expect(line.flow == .failed)
        #expect(line.verb == "failed")
        #expect(line.turns.isEmpty)
        // A failure at a path a change also lists stays a separate line.
        #expect(FoldEntry.line(line).id == "failed:locked.txt")
    }

    /// Turning toward the folder a change would have changed keeps that
    /// folder's version, under the context menu's name for it: a copy to
    /// B turns ← into an archive in A.
    @Test func steerableChangesTurnTowardTheFolderTheyChange() {
        #expect(FoldLine(row: row("a", .copy(to: .b), otherWay: .b)).turns == [.toA: "Archive in Folder A"])
        #expect(FoldLine(row: row("a", .archive(on: .a), otherWay: .a)).turns == [.toB: "Restore to Folder B"])
        #expect(FoldLine(row: row("a", .blocked)).turns.isEmpty)
        #expect(FoldLine.keptSide(.toB) == .a)
        #expect(FoldLine.keptSide(.toA) == .b)
        #expect(FoldLine.keptSide(.held) == nil)
    }

    @Test func linesGroupUnderTheirFolder() {
        // In path order a subfolder falls between its parent's files; each
        // folder's lines still gather under one heading, first seen first.
        let lines = [
            FoldLine(row: row("Budget.txt", .update(to: .b), id: 0)),
            FoldLine(row: row("Website/index.html", .copy(to: .b), id: 1)),
            FoldLine(row: row("Website/old/index.html", .keep, id: 2)),
            FoldLine(row: row("Website/style.css", .copy(to: .b), id: 3)),
        ]
        let entries = FoldEntry.entries(lines)
        #expect(entries.map(\.id) == [
            "line:Budget.txt",
            "folder:Website", "line:Website/index.html", "line:Website/style.css",
            "folder:Website/old", "line:Website/old/index.html",
        ])
        #expect(entries[1] == .folder(path: "Website", count: 2))
    }

    @Test func oneWayModesFoldTheirSourceAndTwoWayFoldsTheWetterHalf() {
        #expect(FoldLeaf.sides(.mirrorLeft) == [.a])
        #expect(FoldLeaf.sides(.backupRight) == [.b])
        #expect(FoldLeaf.sides(.sync) == [.a, .b])

        let toA = [FoldLine(row: row("a", .copy(to: .a), id: 0)), FoldLine(row: row("b", .copy(to: .a), id: 1)),
                   FoldLine(row: row("c", .copy(to: .b), id: 2))]
        #expect(FoldLeaf.automatic(.sync, lines: toA) == .b)
        #expect(FoldLeaf.automatic(.sync, lines: []) == .a)
        #expect(FoldLeaf.automatic(.backupRight, lines: []) == .b)
    }

    /// The opened sheet says only what the run reports.
    @Test func theImprintCarriesTheRunsOwnCounts() {
        let run = RunSummary(cancelled: false, copied: 5, updated: 0, archived: 2, recovered: 0, conflicts: 3,
                             failures: [], failureCount: 1, archive: "run", moved: 1)
        let imprint = FoldImprint(result: run)
        #expect(imprint.title == "Folded")
        #expect(imprint.figures == [.init(value: "5", label: "copied"), .init(value: "2", label: "archived"),
                                    .init(value: "1", label: "renamed")])
        #expect(imprint.notes == ["3 conflicts left as they were", "1 item failed; the next sync retries it"])

        let nothing = RunSummary(cancelled: true, copied: 0, updated: 0, archived: 0, recovered: 0, conflicts: 0,
                                 failures: [], failureCount: 0, archive: "run")
        #expect(FoldImprint(result: nothing).title == "Stopped Partway")
        #expect(FoldImprint(result: nothing).figures == [.init(value: "0", label: "changes")])

        let undo = UndoSummary(cancelled: false, removed: 2, restored: 1, beside: 0, notUndone: [],
                               notUndoneCount: 0, archive: "undo")
        #expect(FoldImprint(undo: undo).figures == [.init(value: "1", label: "put back"),
                                                    .init(value: "2", label: "archived")])
    }

    /// The engine lists and checks Folder A, then lists and checks Folder
    /// B, then makes and carries out its plan. Comparing counts the items
    /// checked, which only grows, though the items found jump as B's
    /// listing begins; a share appears only once the plan says how much
    /// there is.
    @Test func theReadoutNeverRunsBackward() {
        let comparing: [(listed: UInt64, checked: UInt64)] = [(0, 0), (1_000, 0), (1_000, 1_000),
                                                             (2_400, 1_000), (2_400, 2_400)]
        for (listed, checked) in comparing {
            let readout = FoldReadout(progress: WorkProgress(listed: listed, checked: checked), cancelling: false)
            #expect(readout.stage == .comparing)
            #expect(readout.title == "Comparing")
            #expect(readout.figure == checked.formatted())
            #expect(readout.caption == "items checked")
            #expect(readout.fraction == nil)
            #expect(readout.details == (listed > 0 ? ["of \(listed.formatted()) found so far"] : []))
        }
        let one = FoldReadout(progress: WorkProgress(listed: 3, checked: 1), cancelling: false)
        #expect(one.caption == "item checked")

        let copying = FoldReadout(progress: WorkProgress(listed: 2_400, checked: 2_400, changes: 4, applied: 1,
                                                         bytes: 4_000_000, copied: 1_000_000, rate: 2_500_000),
                                  cancelling: false)
        #expect(copying.stage == .syncing)
        #expect(copying.title == "Syncing")
        #expect(copying.figure == 0.25.formatted(.percent.precision(.fractionLength(0))))
        #expect(copying.caption == "copied")
        #expect(copying.fraction == 0.25)
        let bytes = "\(PairSyncViewModel.byteText(1_000_000)) of \(PairSyncViewModel.byteText(4_000_000))"
        // 3 MB left at 2.5 MB/s takes 1.2 s, rounded up.
        #expect(copying.details == ["1 of 4 changes", bytes + " · 2.5 MB/s", "about 2 s left"])
        #expect(copying.spoken.hasPrefix("Syncing, \(copying.figure ?? "") copied, 1 of 4 changes, "))
    }

    /// The share is rounded down, so it says 100% only once all of it is
    /// done, and counts changes when the plan copies no data.
    @Test func theReadoutsShareIsRoundedDown() {
        func figure(_ progress: WorkProgress) -> String? {
            FoldReadout(progress: progress, cancelling: false).figure
        }
        func percent(_ value: Double) -> String { value.formatted(.percent.precision(.fractionLength(0))) }
        #expect(figure(WorkProgress(changes: 2, bytes: 1_000, copied: 999)) == percent(0.99))
        #expect(figure(WorkProgress(changes: 2, bytes: 100, copied: 29)) == percent(0.29))
        #expect(figure(WorkProgress(changes: 2, bytes: 100, copied: 100)) == percent(1))

        let archives = FoldReadout(progress: WorkProgress(changes: 3, applied: 1), cancelling: false)
        #expect(archives.figure == percent(0.33))
        #expect(archives.caption == "done")
        #expect(archives.fraction == 1.0 / 3)
        #expect(archives.details == ["1 of 3 changes"])
        #expect(FoldReadout(progress: WorkProgress(changes: 1), cancelling: false).details == ["0 of 1 change"])
    }

    /// The time left is rounded up, so it never says 0 s while data is
    /// still copying: a part second counts as a whole one.
    @Test func theReadoutsTimeLeftIsRoundedUp() {
        func timeLeft(copied: UInt64) -> String? {
            let progress = WorkProgress(changes: 4, applied: 3, bytes: 5_000_000, copied: copied, rate: 2_500_000)
            return FoldReadout(progress: progress, cancelling: false).details.first { $0.hasSuffix(" left") }
        }
        #expect(timeLeft(copied: 4_999_999) == "about 1 s left")
        #expect(timeLeft(copied: 2_500_000) == "about 1 s left")
        #expect(timeLeft(copied: 2_499_999) == "about 2 s left")
        #expect(timeLeft(copied: 5_000_000) == nil)
    }

    /// Waiting on another Mac shows no figure and no bar, since nothing
    /// moves; a stopping job keeps its figure and its counts, and drops
    /// its speed and time left.
    @Test func theReadoutWaitsAndStops() {
        // The engine waits for another Mac after comparing, before it plans.
        let held = WorkProgress(listed: 200, checked: 200, waitingFor: "Studio")
        let waiting = FoldReadout(progress: held, cancelling: false)
        #expect(waiting.stage == .waiting)
        #expect(waiting.figure == nil)
        #expect(waiting.caption == nil)
        #expect(waiting.fraction == nil)
        #expect(waiting.details == ["“Studio” is syncing these folders. This sync goes on once it is done."])
        #expect(waiting.spoken == "Waiting, “Studio” is syncing these folders. This sync goes on once it is done.")

        // Stopped while waiting: the count it reached, still with no share.
        let stoppedWaiting = FoldReadout(progress: held, cancelling: true)
        #expect(stoppedWaiting.stage == .stopping)
        #expect(stoppedWaiting.figure == UInt64(200).formatted())
        #expect(stoppedWaiting.fraction == nil)
        #expect(stoppedWaiting.details == ["of \(UInt64(200).formatted()) found so far"])

        let copying = WorkProgress(listed: 200, checked: 200, changes: 4, applied: 2, bytes: 400, copied: 200,
                                   rate: 100)
        let stopping = FoldReadout(progress: copying, cancelling: true)
        #expect(stopping.stage == .stopping)
        #expect(stopping.title == "Stopping")
        #expect(stopping.figure == 0.5.formatted(.percent.precision(.fractionLength(0))))
        #expect(stopping.fraction == 0.5)
        let data = "\(PairSyncViewModel.byteText(200)) of \(PairSyncViewModel.byteText(400))"
        #expect(stopping.details == ["2 of 4 changes", data])
    }

    /// The page that changes carries the mark, in its outer margin; a
    /// page that stays as it is carries none.
    @Test func marksSitOnThePageThatChanges() {
        let toB = FoldLine(row: row("2026/Trip/day2/IMG_2041.RAW", .copy(to: .b)))
        #expect(toB.badge(.a) == nil)
        #expect(toB.badge(.b) == .arrives(.toB))
        let toA = FoldLine(row: row("notes.md", .copy(to: .a)))
        #expect(toA.badge(.a) == .arrives(.toA))
        #expect(toA.badge(.b) == nil)

        let archive = FoldLine(row: row("2024/drafts/old-banner.psd", .archive(on: .b)))
        #expect(archive.badge(.a) == nil)
        #expect(archive.badge(.b) == .lift)
        let metadata = FoldLine(row: row("IMG_0042.txt", .update(to: .a)))
        #expect(metadata.badge(.a) == .retouch)
        #expect(metadata.badge(.b) == nil)
        let rename = FoldLine(row: row("2026/Trip/day1/IMG_1990.HEIC", .move(on: .b),
                                       movedFrom: "2026/Inbox/IMG_1990.HEIC"))
        #expect(rename.badge(.b) == .arrives(.toB))
        #expect(rename.badge(.a) == nil)

        // What holds or waits marks both folders; a failure marks its line once.
        let conflict = FoldLine(row: row("header.psd", .conflict(.bothChanged), resolvable: true))
        #expect(conflict.badge(.a) == .held)
        #expect(conflict.badge(.b) == .held)
        let waiting = FoldLine(row: row("day3", .blocked))
        #expect(waiting.badge(.a) == .waiting)
        #expect(waiting.badge(.b) == .waiting)
        let kept = FoldLine(row: row("kept.txt", .keep))
        #expect(kept.badge(.a) == nil)
        #expect(kept.badge(.b) == nil)
        let failed = FoldLine(row: row("locked.txt", nil, id: -1))
        #expect(failed.badge(.a) == .failed)
        #expect(failed.badge(.b) == nil)
    }

    /// A staged turn is marked only on the page it would land on.
    @Test func aStagedTurnIsMarkedWhereItWouldLand() {
        let conflict = FoldLine(row: row("header.psd", .conflict(.bothChanged), resolvable: true))
        #expect(conflict.badge(.a, staged: .toB) == nil)
        #expect(conflict.badge(.b, staged: .toB) == .arrives(.toB))
        #expect(conflict.badge(.a, staged: .toA) == .arrives(.toA))
        #expect(conflict.badge(.b, staged: .toA) == nil)

        let toB = FoldLine(row: row("IMG_2042.RAW", .copy(to: .b), otherWay: .b))
        #expect(toB.badge(.a, staged: .toA) == .arrives(.toA))
        #expect(toB.badge(.b, staged: .toA) == nil)
    }

    /// Messages keep to one page: the one a one-way pair writes to, B's in
    /// a two-way pair, or the one that is there when a folder is missing.
    @Test func messagesKeepToOnePage() {
        #expect(FoldLeaf.messagePage(.sync, missing: nil) == .b)
        #expect(FoldLeaf.messagePage(.mirrorLeft, missing: nil) == .b)
        #expect(FoldLeaf.messagePage(.backupRight, missing: nil) == .a)
        #expect(FoldLeaf.messagePage(.mirrorLeft, missing: .b) == .a)
        #expect(FoldLeaf.messagePage(.sync, missing: .a) == .b)
        #expect(FoldLeaf.messagePage(.sync, missing: .b) == .a)
        #expect(FoldLeaf.messagePage(.backupRight, missing: .a) == .b)
        #expect(FoldLeaf.landing(.a) == .b)
        #expect(FoldLeaf.landing(.b) == .a)
    }

    /// The turning leaf's words are gone before perspective squeezes
    /// them, and the page under it keeps its words until it closes over.
    @Test func printGivesWayBeforeTheLeafTurnsFar() {
        #expect(FoldMotion.faceInk(0) == 1)
        #expect(FoldMotion.faceInk(0.03) == 1)
        #expect(FoldMotion.faceInk(0.14) == 0)
        #expect(FoldMotion.faceInk(FoldMotion.heldUpright) == 0)
        #expect(FoldMotion.landingInk(0) == 1)
        #expect(FoldMotion.landingInk(FoldMotion.tippingPoint) == 1)
        #expect(FoldMotion.landingInk(1) < 0.2)
        // The plan held for approval reads complete, and letting go there
        // would not fold it.
        #expect(FoldMotion.peek(0) == 0)
        #expect(FoldMotion.peek(FoldMotion.heldUpright) == 1)
        #expect(!FoldMotion.commits(swing: FoldMotion.heldUpright))
        for step in 0...100 {
            let swing = Double(step) / 100
            #expect((0.0...1.0).contains(FoldMotion.faceInk(swing)))
            #expect((0.0...1.0).contains(FoldMotion.landingInk(swing)))
            #expect((0.0...1.0).contains(FoldMotion.peek(swing)))
        }
    }

    /// Under Reduce Motion one face is gone before the other comes, so
    /// nothing translucent lies over a page's lines, even while held.
    @Test func reducedMotionNeverShowsBothFaces() {
        for step in 0...100 {
            let swing = Double(step) / 100
            #expect(min(FoldMotion.reducedFace(swing), FoldMotion.reducedBack(swing)) == 0)
        }
        #expect(FoldMotion.reducedFace(0) == 1)
        #expect(FoldMotion.reducedBack(1) == 1)
        #expect(FoldMotion.reducedBack(FoldMotion.heldUpright) == 0)
    }

    /// A folder's label names it and says where it is.
    @Test func folderLabelsSplitNameFromPlace() {
        func parts(_ text: String) -> [String] {
            let parts = FoldFolderLabel.parts(text)
            return [parts.name, parts.location]
        }
        #expect(parts("/var/folders/5n/T/Foldover Tour/Photos") == ["Photos", "/var/folders/5n/T/Foldover Tour"])
        #expect(parts("sftp://mynas.local/~/Photos") == ["Photos", "mynas.local/~"])
        #expect(parts("sftp://mynas.local/Photos") == ["Photos", "mynas.local"])
        #expect(parts("/Volumes/Media/Music/") == ["Music", "/Volumes/Media"])
        #expect(parts("/Volumes/Media/Music//") == ["Music", "/Volumes/Media"])
        #expect(parts("/Photos") == ["Photos", "/"])
        #expect(parts("/") == ["/", ""])
        #expect(parts("sftp://mynas.local") == ["sftp://mynas.local", ""])
        #expect(parts("Photos on Studio Mac") == ["Photos on Studio Mac", ""])
    }

    @Test func theLeafWaitsForTheDeadZoneAndTipsPastUpright() {
        #expect(FoldMotion.swing(travel: 6, width: 300) == 0)
        #expect(FoldMotion.swing(travel: 158, width: 300) == 0.5)
        #expect(FoldMotion.swing(travel: 900, width: 300) == 1)
        #expect(FoldMotion.swing(travel: 100, width: 0) == 0)
        // Only real travel past upright folds; there is no flick.
        #expect(!FoldMotion.commits(swing: 0.49))
        #expect(FoldMotion.commits(swing: 0.5))
    }

    /// While the leaf lies folded, its back names the folder whose page it
    /// is and the bare half the folder under it, each with the way its
    /// data goes; a two-way sync's folders both send and receive.
    @Test func aFoldedRunNamesWhichWayEachFolderGoes() {
        #expect(FoldRole(.a, in: .backupLeft) == .source)
        #expect(FoldRole(.b, in: .backupLeft) == .destination)
        #expect(FoldRole(.b, in: .mirrorRight) == .source)
        #expect(FoldRole(.a, in: .mirrorRight) == .destination)
        #expect(FoldRole(.a, in: .sync) == .either)
        #expect(FoldRole(.b, in: .sync) == .either)
        #expect(FoldRole.source.eyebrow(.a) == "From Folder A")
        #expect(FoldRole.destination.eyebrow(.b) == "To Folder B")
        #expect(FoldRole.either.eyebrow(.b) == "Folder B")
    }

    /// The disk under the leaf: its free space, what the run still copies
    /// there, and a warning only when that is more than is free.
    @Test func theDiskUnderTheLeafSaysWhatIsFreeAndWhatIsComing() {
        let disk = FolderVolume(name: "Backup SSD", totalBytes: 2_000_000_000_000, availableBytes: 800_000_000_000)
        let comparing = FoldDisk(volume: disk, toCopy: nil)
        #expect(comparing.name == "Backup SSD")
        #expect(comparing.free == "800 GB free of 2 TB")
        #expect(comparing.toCopy == nil)
        #expect(comparing.stored == 0.6)
        #expect(comparing.arriving == 0)
        #expect(!comparing.mayNotFit)
        #expect(comparing.spoken == "Backup SSD, 800 GB free of 2 TB")

        let copying = FoldDisk(volume: disk, toCopy: 200_000_000_000)
        #expect(copying.toCopy == "200 GB still to copy")
        #expect(copying.arriving == 0.1)
        #expect(!copying.mayNotFit)

        // Nothing left to copy reads as nothing coming, not "Zero KB".
        #expect(FoldDisk(volume: disk, toCopy: 0).toCopy == nil)

        // The bar stops at the free space; the words say the rest.
        let tooMuch = FoldDisk(volume: disk, toCopy: 1_000_000_000_000)
        #expect(tooMuch.arriving == 0.4)
        #expect(tooMuch.mayNotFit)
        #expect(tooMuch.spoken.hasSuffix(FoldDisk.mayNotFitText))
    }
}
