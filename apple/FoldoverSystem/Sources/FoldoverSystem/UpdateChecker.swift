import Foundation

/// A release published on GitHub: its version and the page to download
/// it from.
public struct AppRelease: Equatable, Sendable {
    public let version: String
    public let downloadURL: URL

    public init(version: String, downloadURL: URL) {
        self.version = version
        self.downloadURL = downloadURL
    }
}

/// What went wrong reading GitHub's release feed.
public enum UpdateError: Error {
    case unavailable
}

/// A release version: `X.Y.Z` with an optional `-suffix`, as the tags
/// `scripts/release.sh` writes (a leading `v` is allowed). A suffix
/// marks a pre-release, which sorts before the same version without
/// one (`1.0.0-beta.9` < `1.0.0`), following semver.
public struct ReleaseVersion: Comparable, CustomStringConvertible, Sendable {
    public let major: Int
    public let minor: Int
    public let patch: Int
    /// The `-suffix`, or nil for a stable release.
    public let prerelease: String?

    /// `X.Y.Z` or `X.Y.Z-suffix`, or nil for anything else.
    public init?(parsing version: String) {
        let version = version.hasPrefix("v") ? String(version.dropFirst()) : version
        var core = version
        var suffix: String?
        if let at = version.firstIndex(of: "-") {
            core = String(version[..<at])
            suffix = String(version[version.index(after: at)...])
        }
        let parts = core.split(separator: ".")
        guard parts.count == 3,
              parts.allSatisfy(Self.isNumericField),
              let major = Int(parts[0]), let minor = Int(parts[1]),
              let patch = Int(parts[2])
        else { return nil }
        if let suffix {
            let fields = suffix.split(separator: ".", omittingEmptySubsequences: false)
            guard !fields.isEmpty,
                  fields.allSatisfy({ field in
                      !field.isEmpty && field.allSatisfy(Self.isPrereleaseChar)
                          // A purely numeric identifier forbids leading zeros.
                          && (!field.allSatisfy(Self.isDigit)
                              || Self.isNumericField(field))
                  })
            else { return nil }
        }
        self.major = major
        self.minor = minor
        self.patch = patch
        prerelease = suffix
    }

    public var isPrerelease: Bool {
        prerelease != nil
    }

    public var description: String {
        "\(major).\(minor).\(patch)\(prerelease.map { "-\($0)" } ?? "")"
    }

    public static func < (left: Self, right: Self) -> Bool {
        for (part, other) in zip(
            [left.major, left.minor, left.patch],
            [right.major, right.minor, right.patch])
        where part != other {
            return part < other
        }
        return switch (left.prerelease, right.prerelease) {
        case (nil, nil), (nil, .some): false
        case (.some, nil): true
        case let (.some(one), .some(other)): Self.isOrdered(one, before: other)
        }
    }

    /// Semver's pre-release order: numeric fields compare numerically
    /// and before alphanumeric ones, alphanumeric lexically, and a
    /// prefix of another suffix sorts first.
    private static func isOrdered(_ one: String, before other: String) -> Bool {
        let left = one.split(separator: ".")
        let right = other.split(separator: ".")
        for (field, otherField) in zip(left, right) where field != otherField {
            let numeric = field.allSatisfy(isDigit)
            let otherNumeric = otherField.allSatisfy(isDigit)
            switch (numeric, otherNumeric) {
            case (true, true):
                // Leading zeros are forbidden at parse time, so a
                // longer field is the larger number and equal lengths
                // compare lexically — no Int, nothing to overflow.
                return field.count != otherField.count
                    ? field.count < otherField.count
                    : field.lexicographicallyPrecedes(otherField)
            case (true, false): return true
            case (false, true): return false
            case (false, false):
                return field.lexicographicallyPrecedes(otherField)
            }
        }
        return left.count < right.count
    }

    private static func isDigit(_ char: Character) -> Bool {
        char.isASCII && char.isNumber
    }

    /// A numeric version field: all digits, no leading zero.
    private static func isNumericField(_ field: Substring) -> Bool {
        !field.isEmpty && field.allSatisfy(isDigit)
            && (field.count == 1 || field.first != "0")
    }

    private static func isPrereleaseChar(_ char: Character) -> Bool {
        char.isASCII && (char.isLetter || char.isNumber || char == "-")
    }
}

/// Update checks against the repository's GitHub releases (PLAN.md §9,
/// ADR 0045). The ad-hoc-signed app cannot replace itself, so a newer
/// release is offered as a message with a link to its download.
public struct UpdateChecker: Sendable {
    /// `/releases/latest` redirects to `/releases/tag/<tag>`; the
    /// redirect carries the version, so the check needs no API call and
    /// stays outside the unauthenticated API's per-address rate limit.
    private static let latest = URL(
        string: "https://github.com/L-K-M/Foldover/releases/latest")!

    /// The feed listing every published release, stable and pre-release
    /// alike. Also a plain web page, not a rate-limited API call.
    private static let feed = URL(
        string: "https://github.com/L-K-M/Foldover/releases.atom")!

    private let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    /// The newest stable release. GitHub's latest never names a draft or
    /// a pre-release, matching the tags `scripts/release.sh` writes.
    public func check() async throws -> AppRelease {
        let (_, response) = try await session.data(for: request(to: Self.latest))
        guard let http = response as? HTTPURLResponse, http.statusCode == 200,
              let page = http.url,
              let version = ReleaseVersion(parsing: page.lastPathComponent) else {
            throw UpdateError.unavailable
        }
        return AppRelease(version: version.description, downloadURL: page)
    }

    /// The newest release including pre-releases — the check a
    /// pre-release install runs, since `/releases/latest` never names
    /// one. Feed entries arrive newest-published first, not
    /// newest-versioned, so every tag is compared rather than the
    /// first; and the feed serves only a bounded window of the newest
    /// entries, so the stable check runs too and the newer of the two
    /// answers — a newer release cannot age off the page unseen.
    public func checkIncludingPrereleases() async throws -> AppRelease {
        let (data, response) = try await session.data(for: request(to: Self.feed))
        guard let http = response as? HTTPURLResponse, http.statusCode == 200,
              let release = Self.newestRelease(in: String(decoding: data, as: UTF8.self))
        else {
            throw UpdateError.unavailable
        }
        // A repo with no stable release yet answers `latest` with an
        // error; the feed's answer stands alone then.
        if let stable = try? await check(),
           let stableVersion = ReleaseVersion(parsing: stable.version),
           let feedVersion = ReleaseVersion(parsing: release.version),
           stableVersion > feedVersion {
            return stable
        }
        return release
    }

    /// The highest-versioned release `feed` links to, or nil when none
    /// of its tag links carries a parseable version.
    public static func newestRelease(in feed: String) -> AppRelease? {
        var newest: (release: AppRelease, version: ReleaseVersion)?
        var remaining = feed[...]
        // Anchored to this repository so links inside release notes to
        // other repos cannot pose as ours. A same-repo tag URL quoted in
        // note text still matches; notes are maintainer-written, so that
        // residual risk is accepted.
        let marker = "https://github.com/L-K-M/Foldover/releases/tag/"
        while let at = remaining.range(of: marker) {
            remaining = remaining[at.upperBound...]
            let tag = String(remaining.prefix(while: Self.isTagChar))
            guard let version = ReleaseVersion(parsing: tag) else { continue }
            if newest.map({ version > $0.version }) ?? true,
               let page = URL(string: marker + tag) {
                newest = (AppRelease(version: version.description, downloadURL: page), version)
            }
        }
        return newest?.release
    }

    /// What a tag may contain — the characters `release.sh`'s version
    /// check admits plus `_`, which GitHub also allows in tag names.
    private static func isTagChar(_ char: Character) -> Bool {
        char.isASCII && (char.isLetter || char.isNumber || char == "-"
            || char == "." || char == "_")
    }

    private func request(to url: URL) -> URLRequest {
        var request = URLRequest(url: url)
        request.setValue("Foldover", forHTTPHeaderField: "User-Agent")
        request.timeoutInterval = 10
        return request
    }
}
