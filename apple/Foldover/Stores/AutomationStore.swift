import FoldoverCore
import Foundation
import Observation

/// Whether automatic syncs run; manual syncs always do.
enum AutomationState: Equatable, Sendable {
    case running
    /// Paused until the date, or until resumed when nil.
    case paused(until: Date?)
}

/// The person's choice for automatic syncs in Low Power Mode (ADR 0036).
enum LowPowerSyncs: Equatable, Sendable {
    case pause
    case run
}

/// Why an automatic sync would wait right now (PLAN.md §3.5).
enum AutomationHold: Equatable, Sendable {
    case lowPowerMode
    case expensiveNetwork
    case constrainedNetwork
}

/// The agent's automation report: the pause plus the environment policy.
struct AutomationStatus: Equatable, Sendable {
    var state: AutomationState
    var lowPower: LowPowerSyncs = .pause
    /// Why an automatic sync would wait now, if one would.
    var heldBy: AutomationHold?
}

protocol AutomationClient: Sendable {
    func state() async throws -> AutomationStatus
    /// Pause for `minutes`, or until resumed when nil.
    func pause(minutes: UInt32?) async throws -> AutomationStatus
    func resume() async throws -> AutomationStatus
    /// Set the choice for automatic syncs in Low Power Mode.
    func setLowPower(_ syncs: LowPowerSyncs) async throws -> AutomationStatus
}

// Keep generated async values inside one actor; only native Sendable records leave it.
private actor CoreAutomationClient: AutomationClient {
    func state() async throws -> AutomationStatus { convert(try await FoldoverCore.automationState()) }

    func pause(minutes: UInt32?) async throws -> AutomationStatus {
        convert(try await FoldoverCore.pauseAutomation(minutes: minutes))
    }

    func resume() async throws -> AutomationStatus {
        convert(try await FoldoverCore.resumeAutomation())
    }

    func setLowPower(_ syncs: LowPowerSyncs) async throws -> AutomationStatus {
        let syncs: FoldoverCore.LowPowerSyncs = syncs == .run ? .run : .pause
        return convert(try await FoldoverCore.setLowPowerSyncs(syncs: syncs))
    }

    private func convert(_ state: FoldoverCore.AutomationState) -> AutomationStatus {
        AutomationStatus(
            state: state.paused
                ? .paused(until: state.untilMs.map { Date(timeIntervalSince1970: Double($0) / 1000) })
                : .running,
            lowPower: state.lowPower == .run ? .run : .pause,
            heldBy: state.heldBy.map { held in
                switch held {
                case .lowPowerMode: .lowPowerMode
                case .expensiveNetwork: .expensiveNetwork
                case .constrainedNetwork: .constrainedNetwork
                }
            })
    }
}

/// The agent's automation state, as last read.
@Observable
@MainActor
final class AutomationStore {
    private let client: any AutomationClient
    private var generation = 0
    private(set) var state: AutomationState = .running
    private(set) var lowPower: LowPowerSyncs = .pause
    /// Why an automatic sync would wait now, if one would.
    private(set) var heldBy: AutomationHold?
    private(set) var error: String?

    init(client: any AutomationClient = CoreAutomationClient()) {
        self.client = client
    }

    /// A status read is informational: it keeps the last state on
    /// failure and never touches `error`, so a failed pause or resume
    /// stays reported until the next request answers. A reply only
    /// applies when no pause or resume overlapped the read.
    func refresh() async {
        generation += 1
        let request = generation
        guard let current = try? await client.state(), generation == request else { return }
        apply(current)
    }

    func pause(minutes: UInt32?) async {
        await perform { try await $0.pause(minutes: minutes) }
    }

    func resume() async {
        await perform { try await $0.resume() }
    }

    /// Shows the choice at once, so the toggle does not bounce while the
    /// agent saves it; a failed save puts the previous choice back unless
    /// a newer choice replaced it meanwhile.
    func setLowPower(_ syncs: LowPowerSyncs) async {
        let previous = lowPower
        lowPower = syncs
        let saved = await perform { try await $0.setLowPower(syncs) }
        if !saved, lowPower == syncs { lowPower = previous }
    }

    private func apply(_ status: AutomationStatus) {
        state = status.state
        lowPower = status.lowPower
        heldBy = status.heldBy
    }

    /// Whether this request succeeded; `error` is shared by requests
    /// that overlap, so it cannot tell.
    @discardableResult
    private func perform(_ request: @Sendable (any AutomationClient) async throws -> AutomationStatus) async -> Bool {
        generation += 1
        // Completing an action invalidates reads issued while it was
        // on the wire, so they cannot overwrite this reply.
        defer { generation += 1 }
        error = nil
        do {
            apply(try await request(client))
            return true
        } catch {
            self.error = PairStore.message(for: error)
            return false
        }
    }
}
