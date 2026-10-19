import FoldoverCore
import Foundation
import Observation

/// Whether automatic syncs run; manual syncs always do.
enum AutomationState: Equatable, Sendable {
    case running
    /// Paused until the date, or until resumed when nil.
    case paused(until: Date?)
}

protocol AutomationClient: Sendable {
    func state() async throws -> AutomationState
    /// Pause for `minutes`, or until resumed when nil.
    func pause(minutes: UInt32?) async throws -> AutomationState
    func resume() async throws -> AutomationState
}

// Keep generated async values inside one actor; only native Sendable records leave it.
private actor CoreAutomationClient: AutomationClient {
    func state() async throws -> AutomationState { state(try await FoldoverCore.automationState()) }

    func pause(minutes: UInt32?) async throws -> AutomationState {
        state(try await FoldoverCore.pauseAutomation(minutes: minutes))
    }

    func resume() async throws -> AutomationState { state(try await FoldoverCore.resumeAutomation()) }

    private func state(_ state: FoldoverCore.AutomationState) -> AutomationState {
        guard state.paused else { return .running }
        return .paused(until: state.untilMs.map { Date(timeIntervalSince1970: Double($0) / 1000) })
    }
}

/// The agent's automation pause, as last read.
@Observable
@MainActor
final class AutomationStore {
    private let client: any AutomationClient
    private(set) var state: AutomationState = .running
    private(set) var error: String?

    init(client: any AutomationClient = CoreAutomationClient()) {
        self.client = client
    }

    /// A status read is informational: it keeps the last state on
    /// failure and never touches `error`, so a failed pause or resume
    /// stays reported until the next request answers.
    func refresh() async {
        if let current = try? await client.state() { state = current }
    }

    func pause(minutes: UInt32?) async {
        await perform { try await $0.pause(minutes: minutes) }
    }

    func resume() async {
        await perform { try await $0.resume() }
    }

    private func perform(_ request: @Sendable (any AutomationClient) async throws -> AutomationState) async {
        error = nil
        do {
            state = try await request(client)
        } catch {
            self.error = PairStore.message(for: error)
        }
    }
}
