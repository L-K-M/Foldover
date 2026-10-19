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

/// Whether the status request reaches the client before the action
/// starts or while the action is still unanswered.
private enum StatusReadTiming: Sendable {
    case beforeAction
    case duringAction
}

private enum AutomationCommand: Sendable {
    case pause
    case resume
}

/// Parks status reads and action requests until the test answers them,
/// so a read can be delivered after an action's reply was already
/// applied. `finishReads` installs a standing reply for calls issued
/// later and `finishActions` drains held requests, so a failed wait
/// never leaves a task suspended.
private actor DeferredAutomationClient: AutomationClient {
    private(set) var parkedReads = 0
    private(set) var parkedActions = 0
    private var actionsHeld = false
    private var standingRead: AutomationState?
    private var pendingReads: [CheckedContinuation<AutomationState, Never>?] = []
    private var pendingActions: [CheckedContinuation<AutomationState, Never>?] = []

    func state() async throws -> AutomationState {
        if let standingRead { return standingRead }
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                pendingReads.append(continuation)
                parkedReads += 1
                if Task.isCancelled { answer(&pendingReads, at: pendingReads.count - 1, .running) }
            }
        } onCancel: {
            Task { await self.finishReads(.running) }
        }
    }

    func pause(minutes: UInt32?) async throws -> AutomationState {
        await act(.paused(until: minutes.map { Date(timeIntervalSince1970: Double($0) * 60) }))
    }

    func resume() async throws -> AutomationState { await act(.running) }

    private func act(_ reply: AutomationState) async -> AutomationState {
        guard actionsHeld else { return reply }
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                pendingActions.append(continuation)
                parkedActions += 1
                if Task.isCancelled { answer(&pendingActions, at: pendingActions.count - 1, .running) }
            }
        } onCancel: {
            Task { await self.finishActions(.running) }
        }
    }

    /// Park further action requests until `answerAction`/`finishActions`.
    func holdActions() { actionsHeld = true }

    /// Answers the oldest queued read, if one is parked.
    func answerRead(_ state: AutomationState) {
        if let index = pendingReads.firstIndex(where: { $0 != nil }) {
            answer(&pendingReads, at: index, state)
        }
    }

    /// Answers the oldest queued action request, if one is parked.
    func answerAction(_ state: AutomationState) {
        if let index = pendingActions.firstIndex(where: { $0 != nil }) {
            answer(&pendingActions, at: index, state)
        }
    }

    /// Answers every queued read and every read issued afterwards.
    func finishReads(_ state: AutomationState) {
        standingRead = state
        while let index = pendingReads.firstIndex(where: { $0 != nil }) {
            answer(&pendingReads, at: index, state)
        }
    }

    /// Stops holding actions and answers every queued one.
    func finishActions(_ state: AutomationState) {
        actionsHeld = false
        while let index = pendingActions.firstIndex(where: { $0 != nil }) {
            answer(&pendingActions, at: index, state)
        }
    }

    private func answer(_ queue: inout [CheckedContinuation<AutomationState, Never>?],
                        at index: Int, _ state: AutomationState) {
        guard queue.indices.contains(index), let continuation = queue[index] else { return }
        queue[index] = nil
        continuation.resume(returning: state)
    }

    /// Bounded: a call that never parks reports false instead of
    /// hanging the suite.
    func waitForParkedReads(_ count: Int, timeout: Duration = .seconds(3)) async -> Bool {
        let deadline = ContinuousClock.now + timeout
        while parkedReads < count {
            if ContinuousClock.now >= deadline { return false }
            try? await Task.sleep(for: .milliseconds(1))
        }
        return true
    }

    func waitForParkedActions(_ count: Int, timeout: Duration = .seconds(3)) async -> Bool {
        let deadline = ContinuousClock.now + timeout
        while parkedActions < count {
            if ContinuousClock.now >= deadline { return false }
            try? await Task.sleep(for: .milliseconds(1))
        }
        return true
    }
}

private extension AutomationStore {
    /// Runs the parameterized pause or resume.
    func act(_ command: AutomationCommand) async {
        switch command {
        case .pause: await pause(minutes: nil)
        case .resume: await resume()
        }
    }
}

// Regression: refresh() applies its answer whenever it lands, so a
// status read on the wire across a pause or resume overwrites the
// accepted reply with the agent's older state — the indicator lies
// until the next poll. Delivered after the ack, a stale read must not
// roll the state back.
@MainActor
@Test(arguments: [
    (StatusReadTiming.beforeAction, AutomationCommand.pause),
    (.beforeAction, .resume),
    (.duringAction, .pause),
    (.duringAction, .resume),
])
private func aStatusReadDeliveredAfterAnActionCannotOverwriteItsReply(
    timing: StatusReadTiming, command: AutomationCommand
) async {
    let client = DeferredAutomationClient()
    let store = AutomationStore(client: client)
    // Lift a pause first so resume also has an accepted change to keep.
    if command == .resume { await store.pause(minutes: nil) }
    // What a read issued now would report: the state before the action.
    let stale = store.state
    let accepted: AutomationState = command == .pause ? .paused(until: nil) : .running

    switch timing {
    case .beforeAction:
        let read = Task { await store.refresh() }
        // A read requested before the action must reach the client.
        #expect(await client.waitForParkedReads(1))
        await store.act(command)
        await client.answerRead(stale)
        await client.finishReads(accepted)
        await read.value
    case .duringAction:
        await client.holdActions()
        let action = Task { await store.act(command) }
        #expect(await client.waitForParkedActions(1))
        let read = Task { await store.refresh() }
        // A store that skips a read while an action is unanswered is
        // also correct; only a parked read gets the stale answer.
        _ = await client.waitForParkedReads(1, timeout: .milliseconds(500))
        await client.answerAction(accepted)
        await client.finishActions(.running)
        await action.value
        await client.answerRead(stale)
        await client.finishReads(accepted)
        await read.value
    }
    await client.finishActions(.running)
    #expect(store.state == accepted)
}
