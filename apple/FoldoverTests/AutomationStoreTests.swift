import Foundation
import Testing
@testable import Foldover

private enum AutomationFixtureError: Error { case unreachable }

/// Remembers the last pause like the agent does.
private actor AutomationFixtureClient: AutomationClient {
    private var current: AutomationState = .running
    private var failure: Error?
    private(set) var requested: [UInt32?] = []

    func state() async throws -> AutomationState { current }

    func pause(minutes: UInt32?) async throws -> AutomationState {
        requested.append(minutes)
        if let failure { throw failure }
        current = .paused(until: minutes.map { Date(timeIntervalSince1970: Double($0) * 60) })
        return current
    }

    func resume() async throws -> AutomationState {
        if let failure { throw failure }
        current = .running
        return current
    }

    func rejectRequests() { failure = AutomationFixtureError.unreachable }
    func acceptRequests() { failure = nil }
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

@MainActor
@Test
func failedPauseAndResumeRequestsStayVisiblePastStatusRefreshes() async {
    let client = AutomationFixtureClient()
    let automation = AutomationStore(client: client)
    let viewModel = MainViewModel(appInfo: AppInfoStore(), automation: automation)
    await client.rejectRequests()

    // A failed menu-bar request reaches the error the window shows and
    // still lets Shortcuts report it.
    await viewModel.pause(.oneHour)
    #expect(!viewModel.isPaused)
    #expect(automation.error != nil)
    #expect(viewModel.operationError != nil)

    // monitor() refreshes the pause state; that read must not erase it.
    await automation.refresh()
    #expect(viewModel.operationError != nil)

    // The next successful request clears the error again.
    await client.acceptRequests()
    await viewModel.pause(.oneHour)
    #expect(viewModel.operationError == nil)
    #expect(viewModel.isPaused)

    await client.rejectRequests()
    await viewModel.resume()
    #expect(viewModel.operationError != nil)
    await automation.refresh()
    #expect(viewModel.operationError != nil)
    #expect(viewModel.isPaused)
}
