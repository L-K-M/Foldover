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

/// Update checks against the repository's GitHub releases (PLAN.md §9,
/// ADR 0045). The ad-hoc-signed app cannot replace itself, so a newer
/// release is offered as a message with a link to its download.
public struct UpdateChecker: Sendable {
    /// `/releases/latest` redirects to `/releases/tag/<tag>`; the
    /// redirect carries the version, so the check needs no API call and
    /// stays outside the unauthenticated API's per-address rate limit.
    private static let latest = URL(
        string: "https://github.com/L-K-M/Foldover/releases/latest")!

    private let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    /// The newest stable release. GitHub's latest never names a draft or
    /// a pre-release, matching the tags `scripts/release.sh` writes.
    public func check() async throws -> AppRelease {
        var request = URLRequest(url: Self.latest)
        request.setValue("Foldover", forHTTPHeaderField: "User-Agent")
        request.timeoutInterval = 10
        let (_, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200,
              let page = http.url,
              let components = Self.components(of: page.lastPathComponent) else {
            throw UpdateError.unavailable
        }
        return AppRelease(
            version: components.map(String.init).joined(separator: "."),
            downloadURL: page)
    }

    /// The numeric components an X.Y.Z version carries (a leading `v`,
    /// as release tags have, is allowed), or nil for anything else.
    public static func components(of version: String) -> [Int]? {
        let version = version.hasPrefix("v") ? String(version.dropFirst()) : version
        let parts = version.split(separator: ".").compactMap { Int($0) }
        return parts.count == 3 ? parts : nil
    }
}
