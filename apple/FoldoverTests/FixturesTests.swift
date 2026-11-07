#if DEBUG
import Testing
@testable import Foldover

@MainActor
@Test(arguments: [FixtureScenario.syncingBackup, .syncingBackupRight])
func seededBackupJobsClaimTheirScenarioSettings(scenario: FixtureScenario) async throws {
    let pairs = FixturePairClient(scenario: scenario)
    let client = FixtureSyncClient(pairs: pairs, scenario: scenario)
    let expected = try await pairs.runningPair(of: Fixtures.projectsID)
    let state = try await client.status(Fixtures.projectsID)
    guard case let .working(job, cancelling, _, claimed) = state else {
        Issue.record("A syncing backup scenario must start with a working job")
        return
    }
    #expect(job == .run)
    #expect(!cancelling)
    #expect(claimed == expected)
    #expect(claimed.mode == (scenario == .syncingBackupRight ? .backupRight : .backupLeft))
}
#endif
