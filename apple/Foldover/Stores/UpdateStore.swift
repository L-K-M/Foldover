// The store boundary (PLAN.md §4.5): update checks go through
// FoldoverSystem's GitHub release checker.

import FoldoverSystem
import Foundation
import Observation

/// Asking GitHub whether a newer release than this build exists
/// (PLAN.md §9, ADR 0045). The app cannot replace itself; a newer
/// release is a message with a link to its download.
@MainActor
@Observable
final class UpdateStore {
    /// Which outcomes a check announces: everything for the menu item's
    /// check, only a newer release for the launch check.
    enum Announcement {
        case everyOutcome
        case onlyNewer
    }

    @ObservationIgnored private let fetch: @Sendable () async throws -> AppRelease
    @ObservationIgnored private let present: @MainActor (UpdateOutcome, String) -> Void
    @ObservationIgnored private let installed: String
    @ObservationIgnored private var launchChecked = false
    private(set) var checking = false

    init(fetch: (@Sendable () async throws -> AppRelease)? = nil,
         installed: String = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "",
         present: @escaping @MainActor (UpdateOutcome, String) -> Void = UpdatePrompt.present) {
        self.installed = installed
        self.present = present
        self.fetch = fetch ?? Self.feedFetcher(forInstalled: installed)
    }

    /// The one check made when the app opens.
    func checkAtLaunch() async {
        guard !launchChecked else { return }
        launchChecked = true
        await check(announcing: .onlyNewer)
    }

    /// The check behind the app menu's Check for Updates….
    func checkForUpdates() async {
        await check(announcing: .everyOutcome)
    }

    private func check(announcing: Announcement) async {
        guard !checking else { return }
        checking = true
        defer { checking = false }
        let outcome: UpdateOutcome
        if ReleaseVersion(parsing: installed) == nil {
            // Reporting "up to date" here would claim a comparison that
            // never ran.
            outcome = .failed
        } else {
            do {
                let release = try await fetch()
                outcome = Self.isNewer(release.version, than: installed) ? .available(release) : .upToDate
            } catch {
                outcome = .failed
            }
        }
        switch (announcing, outcome) {
        case (.everyOutcome, _), (.onlyNewer, .available):
            present(outcome, installed)
        case (.onlyNewer, _):
            break
        }
    }

    /// Whether `installed` asks the feed that names pre-releases — a
    /// pre-release install does, since `/releases/latest` only ever
    /// names a stable one.
    nonisolated static func checksPrereleases(installed: String) -> Bool {
        ReleaseVersion(parsing: installed)?.isPrerelease ?? false
    }

    /// The default feed a build of `installed` reads: pre-releases
    /// appear only in the release feed, never at `releases/latest`.
    nonisolated static func feedFetcher(
        forInstalled installed: String
    ) -> @Sendable () async throws -> AppRelease {
        let checker = UpdateChecker()
        if checksPrereleases(installed: installed) {
            return { try await checker.checkIncludingPrereleases() }
        }
        return { try await checker.check() }
    }

    /// Whether `candidate` sorts after `installed`; both must be X.Y.Z
    /// with an optional `-suffix` (a leading `v` is allowed). Unparseable
    /// versions report nothing.
    nonisolated static func isNewer(_ candidate: String, than installed: String) -> Bool {
        guard let newer = ReleaseVersion(parsing: candidate),
              let current = ReleaseVersion(parsing: installed) else { return false }
        return newer > current
    }
}
