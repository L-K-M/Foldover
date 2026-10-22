import FoldoverSystem
import Foundation
import Testing
@testable import Foldover

private func release(_ version: String) -> AppRelease {
    AppRelease(version: version, downloadURL: URL(string: "https://example.com/release")!)
}

@MainActor
private final class Recorder {
    private(set) var outcomes: [UpdateOutcome] = []

    func record(_ outcome: UpdateOutcome) {
        outcomes.append(outcome)
    }
}

@MainActor
private func store(returning release: AppRelease, installed: String = "1.2.0",
                   recorder: Recorder) -> UpdateStore {
    UpdateStore(fetch: { release }, installed: installed) { outcome, _ in
        recorder.record(outcome)
    }
}

@MainActor
private func store(failing recorder: Recorder) -> UpdateStore {
    UpdateStore(fetch: { throw UpdateError.unavailable }, installed: "1.2.0") { outcome, _ in
        recorder.record(outcome)
    }
}

@MainActor
@Test
func aNewerReleaseIsAnnounced() async {
    let recorder = Recorder()
    let store = store(returning: release("1.2.3"), recorder: recorder)
    await store.checkForUpdates()
    #expect(recorder.outcomes == [.available(release("1.2.3"))])
}

@MainActor
@Test
func theSameOrAnOlderReleaseIsUpToDate() async {
    let recorder = Recorder()
    let store = store(returning: release("1.2.0"), recorder: recorder)
    await store.checkForUpdates()
    #expect(recorder.outcomes == [.upToDate])
}

@MainActor
@Test
func aFailedCheckIsReportedWhenAsked() async {
    let recorder = Recorder()
    let store = store(failing: recorder)
    await store.checkForUpdates()
    #expect(recorder.outcomes == [.failed])
}

@MainActor
@Test
func aFailedLaunchCheckAnnouncesNothing() async {
    let recorder = Recorder()
    let store = store(failing: recorder)
    await store.checkAtLaunch()
    #expect(recorder.outcomes.isEmpty)
}

@MainActor
@Test
func theLaunchCheckAnnouncesOnlyOnce() async {
    let recorder = Recorder()
    let store = store(returning: release("1.3.0"), recorder: recorder)
    await store.checkAtLaunch()
    await store.checkAtLaunch()
    #expect(recorder.outcomes == [.available(release("1.3.0"))])
}

@MainActor
@Test
func anUnparseableInstalledVersionIsAFailureNotUpToDate() async {
    let recorder = Recorder()
    let store = store(returning: release("1.3.0"), installed: "dev-build", recorder: recorder)
    await store.checkForUpdates()
    #expect(recorder.outcomes == [.failed])
}

@Test
func versionsCompareNumerically() {
    #expect(UpdateStore.isNewer("1.2.3", than: "1.2.0"))
    #expect(UpdateStore.isNewer("v1.2.3", than: "1.2.0"))
    #expect(UpdateStore.isNewer("0.10.0", than: "0.9.9"))
    #expect(UpdateStore.isNewer("2.0.0", than: "1.9.9"))
    #expect(!UpdateStore.isNewer("1.2.0", than: "1.2.0"))
    #expect(!UpdateStore.isNewer("1.1.9", than: "1.2.0"))
    #expect(!UpdateStore.isNewer("1.2", than: "1.1.0"))
    #expect(!UpdateStore.isNewer("beta", than: "1.1.0"))
    #expect(!UpdateStore.isNewer("1.2.3", than: ""))
}
