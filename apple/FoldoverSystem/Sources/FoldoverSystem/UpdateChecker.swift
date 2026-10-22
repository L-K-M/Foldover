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
    private static let feed = URL(
        string: "https://api.github.com/repos/L-K-M/Foldover/releases/latest")!

    private let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    /// The newest stable release. `releases/latest` never names a draft
    /// or a pre-release, matching the tags `scripts/release.sh` writes.
    public func check() async throws -> AppRelease {
        var request = URLRequest(url: Self.feed)
        request.setValue("Foldover", forHTTPHeaderField: "User-Agent")
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse, response.statusCode == 200 else {
            throw UpdateError.unavailable
        }
        guard let release = try? JSONDecoder().decode(LatestRelease.self, from: data),
              let components = Self.components(of: release.tag),
              let url = URL(string: release.page) else {
            throw UpdateError.unavailable
        }
        return AppRelease(
            version: components.map(String.init).joined(separator: "."),
            downloadURL: url)
    }

    /// The numeric components an X.Y.Z version carries (a leading `v`,
    /// as release tags have, is allowed), or nil for anything else.
    public static func components(of version: String) -> [Int]? {
        let version = version.hasPrefix("v") ? String(version.dropFirst()) : version
        let parts = version.split(separator: ".").compactMap { Int($0) }
        return parts.count == 3 ? parts : nil
    }
}

/// The fields `releases/latest` carries that a check needs.
private struct LatestRelease: Decodable {
    let tag: String
    let page: String

    enum CodingKeys: String, CodingKey {
        case tag = "tag_name"
        case page = "html_url"
    }
}
