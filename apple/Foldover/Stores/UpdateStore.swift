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

    init(fetch: @escaping @Sendable () async throws -> AppRelease = { try await UpdateChecker().check() },
         installed: String = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "",
         present: @escaping @MainActor (UpdateOutcome, String) -> Void = UpdatePrompt.present) {
        self.fetch = fetch
        self.installed = installed
        self.present = present
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
        if UpdateChecker.components(of: installed) == nil {
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

    /// Whether `candidate` sorts after `installed`; both must be X.Y.Z
    /// (a leading `v` is allowed). Unparseable versions report nothing.
    nonisolated static func isNewer(_ candidate: String, than installed: String) -> Bool {
        guard let newer = UpdateChecker.components(of: candidate),
              let current = UpdateChecker.components(of: installed) else { return false }
        for (part, other) in zip(newer, current) where part != other {
            return part > other
        }
        return false
    }
}
