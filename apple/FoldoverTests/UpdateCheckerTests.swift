import FoldoverSystem
import Foundation
import Testing

@Test
func releaseVersionsParse() {
    #expect(ReleaseVersion(parsing: "1.2.3") != nil)
    #expect(ReleaseVersion(parsing: "v1.2.3") != nil)
    #expect(ReleaseVersion(parsing: "1.2.3-beta.1")?.prerelease == "beta.1")
    #expect(ReleaseVersion(parsing: "v1.2.3-beta.1")?.prerelease == "beta.1")
    #expect(ReleaseVersion(parsing: "1.2.3")?.isPrerelease == false)
    #expect(ReleaseVersion(parsing: "1.2") == nil)
    #expect(ReleaseVersion(parsing: "1.2.3.4") == nil)
    #expect(ReleaseVersion(parsing: "1.2.+3") == nil)
    #expect(ReleaseVersion(parsing: "1.2.3-") == nil)
    #expect(ReleaseVersion(parsing: "1.2.3-beta..1") == nil)
    #expect(ReleaseVersion(parsing: "1.2.3-beta 1") == nil)
    #expect(ReleaseVersion(parsing: "beta") == nil)
    #expect(ReleaseVersion(parsing: "") == nil)
    // Semver forbids leading zeros in numeric fields.
    #expect(ReleaseVersion(parsing: "1.02.3") == nil)
    #expect(ReleaseVersion(parsing: "1.2.3-beta.01") == nil)
    #expect(ReleaseVersion(parsing: "1.2.3-01") == nil)
    #expect(ReleaseVersion(parsing: "1.2.3-0") != nil)
    #expect(ReleaseVersion(parsing: "1.2.3-2024.1.15") != nil)
}

@Test
func releaseVersionsOrder() {
    #expect(ReleaseVersion(parsing: "1.2.3")! > ReleaseVersion(parsing: "1.2.0")!)
    #expect(ReleaseVersion(parsing: "0.10.0")! > ReleaseVersion(parsing: "0.9.9")!)
    #expect(ReleaseVersion(parsing: "1.0.0-beta.1")! < ReleaseVersion(parsing: "1.0.0")!)
    #expect(ReleaseVersion(parsing: "1.0.0-beta.1")! < ReleaseVersion(parsing: "1.0.0-beta.2")!)
    #expect(ReleaseVersion(parsing: "1.0.0-beta.2")! < ReleaseVersion(parsing: "1.0.0-beta.10")!)
    #expect(ReleaseVersion(parsing: "1.0.0-alpha")! < ReleaseVersion(parsing: "1.0.0-alpha.1")!)
    #expect(ReleaseVersion(parsing: "1.0.0-alpha.1")! < ReleaseVersion(parsing: "1.0.0-alpha.beta")!)
    #expect(ReleaseVersion(parsing: "1.0.0-rc.1")! < ReleaseVersion(parsing: "1.0.0")!)
    #expect(ReleaseVersion(parsing: "2.0.0-beta.1")! > ReleaseVersion(parsing: "1.9.9")!)
    #expect(ReleaseVersion(parsing: "1.0.0")! == ReleaseVersion(parsing: "v1.0.0")!)
    // A numeric field too long for Int still orders numerically.
    #expect(ReleaseVersion(parsing: "1.0.0-beta.99999999999999999999")!
        > ReleaseVersion(parsing: "1.0.0-beta.2")!)
}

@Test
func releaseVersionsDescribeThemselves() {
    #expect(ReleaseVersion(parsing: "v1.2.3")?.description == "1.2.3")
    #expect(ReleaseVersion(parsing: "v1.2.3-beta.1")?.description == "1.2.3-beta.1")
}

@Test
func theFeedAnswersItsNewestTag() {
    let feed = """
        <?xml version="1.0"?>
        <feed><entry>
          <link rel="alternate" type="text/html"
            href="https://github.com/L-K-M/Foldover/releases/tag/v1.2.0"/>
        </entry><entry>
          <link rel="alternate" type="text/html"
            href="https://github.com/L-K-M/Foldover/releases/tag/v1.3.0-beta.2"/>
        </entry><entry>
          <link rel="alternate" type="text/html"
            href="https://github.com/L-K-M/Foldover/releases/tag/v1.3.0-beta.1"/>
        </entry></feed>
        """
    let newest = UpdateChecker.newestRelease(in: feed)
    #expect(newest?.version == "1.3.0-beta.2")
    #expect(newest?.downloadURL.absoluteString
        == "https://github.com/L-K-M/Foldover/releases/tag/v1.3.0-beta.2")
}

@Test
func theFeedPrefersAStableOverItsOwnBetas() {
    let feed = """
        <feed>
          <entry><link href="https://github.com/L-K-M/Foldover/releases/tag/v1.0.0-beta.2"/></entry>
          <entry><link href="https://github.com/L-K-M/Foldover/releases/tag/v1.0.0"/></entry>
        </feed>
        """
    #expect(UpdateChecker.newestRelease(in: feed)?.version == "1.0.0")
}

@Test
func theFeedSkipsTagsThatAreNotVersions() {
    let feed = """
        <feed>
          <entry><link href="https://github.com/L-K-M/Foldover/releases/tag/nightly"/></entry>
          <entry><link href="https://github.com/L-K-M/Foldover/releases/tag/v2.1.0"/></entry>
        </feed>
        """
    #expect(UpdateChecker.newestRelease(in: feed)?.version == "2.1.0")
    #expect(UpdateChecker.newestRelease(in: "no tags here") == nil)
}

@Test
func theFeedIgnoresReleaseLinksInReleaseNotes() {
    // A release body linking to another repo's tag — or a tag we never
    // published — must not pass as a newer Foldover release.
    let feed = """
        <feed>
          <entry>
            <link href="https://github.com/L-K-M/Foldover/releases/tag/v1.2.0"/>
            <content type="html">Now using
              &lt;a href="https://github.com/other/repo/releases/tag/v9.9.9"&gt;
              repo 9.9.9&lt;/a&gt;.</content>
          </entry>
        </feed>
        """
    #expect(UpdateChecker.newestRelease(in: feed)?.version == "1.2.0")
}
