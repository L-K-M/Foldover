import Foundation
import Testing
@testable import Foldover

/// Remembers the last pause like the agent does.
private actor AutomationFixtureClient: AutomationClient {
    private var current: AutomationState = .running
    private(set) var requested: [UInt32?] = []

    func state() async throws -> AutomationState { current }

    func pause(minutes: UInt32?) async throws -> AutomationState {
        requested.append(minutes)
        current = .paused(until: minutes.map { Date(timeIntervalSince1970: Double($0) * 60) })
        return current
    }

    func resume() async throws -> AutomationState {
        current = .running
        return current
    }
}

@MainActor
@Test
func pausingHoldsAutomaticSyncsUntilResumed() async {
    let client = AutomationFixtureClient()
    let viewModel = MainViewModel(appInfo: AppInfoStore(), automation: AutomationStore(client: client))
    #expect(!viewModel.isPaused)
    #expect(viewModel.pauseText == nil)

    await viewModel.pause(.untilResumed)
    #expect(viewModel.pauseText == "Automatic syncs are paused.")
    await viewModel.pause(.oneHour)
    #expect(viewModel.pauseText?.hasPrefix("Automatic syncs are paused until ") == true)
    #expect(await client.requested == [nil, 60])

    #expect(await viewModel.resumeForShortcuts() == "Automatic syncs are running.")
    #expect(!viewModel.isPaused)
}
