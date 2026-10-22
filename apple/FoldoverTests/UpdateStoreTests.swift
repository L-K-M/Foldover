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

/// Answers the release feed a stable install reads with a stable tag,
/// and the pre-release feed with a beta tag — so which endpoint a fetch
/// ran is visible in what it returns.
private class FeedStub: URLProtocol {
    /// What `/releases.atom` and `/releases/latest` answer.
    class func tags() -> (feed: String, latest: String) {
        ("v9.9.9-beta.1", "v1.0.0")
    }

    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest {
        request
    }

    override func startLoading() {
        let url = request.url!
        let tag = url.absoluteString.hasSuffix("releases.atom")
            ? Self.tags().feed
            : Self.tags().latest
        let feed = """
            <?xml version="1.0"?>
            <feed><entry>
              <link rel="alternate" type="text/html"
                href="https://github.com/L-K-M/Foldover/releases/tag/\(tag)"/>
            </entry></feed>
            """
        let page = URL(
            string: "https://github.com/L-K-M/Foldover/releases/tag/\(tag)")!
        let response = HTTPURLResponse(
            url: url.absoluteString.hasSuffix("releases.atom") ? url : page,
            statusCode: 200, httpVersion: nil, headerFields: nil)!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        if url.absoluteString.hasSuffix("releases.atom") {
            client?.urlProtocol(self, didLoad: Data(feed.utf8))
        }
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

/// The same feed, but the stable line outran the beta: `latest` names
/// a release newer than anything on the feed page.
private final class StableWinsStub: FeedStub {
    override class func tags() -> (feed: String, latest: String) {
        ("v9.9.9-beta.1", "v9.9.9")
    }
}

private func stubbedChecker(
    _ stub: URLProtocol.Type = FeedStub.self
) -> UpdateChecker {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [stub]
    return UpdateChecker(session: URLSession(configuration: configuration))
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

@Test
func preReleasesCompareAsSemver() {
    #expect(UpdateStore.isNewer("1.0.0", than: "1.0.0-beta.9"))
    #expect(UpdateStore.isNewer("1.0.0-beta.2", than: "1.0.0-beta.1"))
    #expect(UpdateStore.isNewer("1.0.1-beta.1", than: "1.0.0"))
    #expect(!UpdateStore.isNewer("1.0.0-beta.1", than: "1.0.0"))
    #expect(!UpdateStore.isNewer("1.0.0-beta.1", than: "1.0.0-beta.1"))
}

@Test
func onlyPreReleaseInstallsCheckThePreReleaseFeed() {
    #expect(UpdateStore.checksPrereleases(installed: "1.0.0-beta.1"))
    #expect(UpdateStore.checksPrereleases(installed: "v1.0.0-rc.2"))
    #expect(!UpdateStore.checksPrereleases(installed: "1.0.0"))
    #expect(!UpdateStore.checksPrereleases(installed: "dev-build"))
}

/// The stub answers the feed endpoint with a beta tag and the latest
/// endpoint with an older stable one, so the returned version proves
/// which endpoint the default fetch ran — and that the feed was read.
@Test
func aPreReleaseInstallsDefaultFetchReadsThePreReleaseFeed() async throws {
    let beta = try await UpdateStore
        .feedFetcher(forInstalled: "1.0.0-beta.1", checker: stubbedChecker())()
    #expect(beta.version == "9.9.9-beta.1")
    let stable = try await UpdateStore
        .feedFetcher(forInstalled: "1.0.0", checker: stubbedChecker())()
    #expect(stable.version == "1.0.0")
}

/// The feed serves a bounded window, so a pre-release install also asks
/// `latest`: a stable release newer than the page's best still wins.
@Test
func aNewerStableIsNotHiddenByTheFeedsWindow() async throws {
    let release = try await UpdateStore
        .feedFetcher(forInstalled: "9.9.9-beta.1", checker: stubbedChecker(StableWinsStub.self))()
    #expect(release.version == "9.9.9")
    #expect(release.downloadURL.absoluteString
        == "https://github.com/L-K-M/Foldover/releases/tag/v9.9.9")
}

@MainActor
@Test
func aPreReleaseInstallSeesTheNextBeta() async {
    let recorder = Recorder()
    let store = store(
        returning: release("1.0.0-beta.2"), installed: "1.0.0-beta.1", recorder: recorder)
    await store.checkForUpdates()
    #expect(recorder.outcomes == [.available(release("1.0.0-beta.2"))])
}

@MainActor
@Test
func aPreReleaseInstallSeesTheStableItPreceded() async {
    let recorder = Recorder()
    let store = store(
        returning: release("1.0.0"), installed: "1.0.0-beta.9", recorder: recorder)
    await store.checkForUpdates()
    #expect(recorder.outcomes == [.available(release("1.0.0"))])
}
