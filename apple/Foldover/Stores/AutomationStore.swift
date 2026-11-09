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

/// How hard automatic syncs may work the Mac (ADR 0054); syncs a person starts always run at full speed.
enum AutomaticEffort: Equatable, Sendable {
    case gentle
    case full
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
    var effort: AutomaticEffort = .gentle
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
    /// Set how hard automatic syncs may work the Mac.
    func setEffort(_ effort: AutomaticEffort) async throws -> AutomationStatus
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

    func setEffort(_ effort: AutomaticEffort) async throws -> AutomationStatus {
        let effort: FoldoverCore.AutomaticEffort = effort == .full ? .full : .gentle
        return convert(try await FoldoverCore.setAutomaticEffort(effort: effort))
    }

    private func convert(_ state: FoldoverCore.AutomationState) -> AutomationStatus {
        AutomationStatus(
            state: state.paused
                ? .paused(until: state.untilMs.map { Date(timeIntervalSince1970: Double($0) / 1000) })
                : .running,
            lowPower: state.lowPower == .run ? .run : .pause,
            effort: state.effort == .full ? .full : .gentle,
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
    @ObservationIgnored private var mutationTail: Task<Bool, Never>?
    private var mutationSequence = 0
    private var lowPowerSequence = 0
    private var effortSequence = 0
    private var confirmedLowPower: LowPowerSyncs = .pause
    private var confirmedEffort: AutomaticEffort = .gentle
    private var desiredLowPower: LowPowerSyncs?
    private var desiredEffort: AutomaticEffort?
    private(set) var pendingMutations = 0
    private(set) var state: AutomationState = .running
    private(set) var lowPower: LowPowerSyncs = .pause
    private(set) var effort: AutomaticEffort = .gentle
    /// Why an automatic sync would wait now, if one would.
    private(set) var heldBy: AutomationHold?
    private(set) var error: String?

    init(client: any AutomationClient = CoreAutomationClient()) {
        self.client = client
    }

    var mutationPending: Bool { pendingMutations > 0 }

    /// A status read is informational: it keeps the last state on failure
    /// and never touches `error`, so a failed mutation stays reported until
    /// the next request answers. A reply applies only when no mutation
    /// overlapped the read.
    func refresh() async {
        guard !mutationPending else { return }
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

    /// Shows the newest choice at once while requests reach the agent in
    /// invocation order. An older reply cannot make the toggle bounce; if
    /// the newest save fails, the last server-confirmed choice returns.
    func setLowPower(_ syncs: LowPowerSyncs) async {
        lowPowerSequence += 1
        let request = lowPowerSequence
        desiredLowPower = syncs
        lowPower = syncs
        _ = await perform { try await $0.setLowPower(syncs) }
        guard lowPowerSequence == request else { return }
        desiredLowPower = nil
        lowPower = confirmedLowPower
    }

    /// Like `setLowPower`: the newest choice shows at once, and a failed
    /// newest save returns the last server-confirmed choice.
    func setEffort(_ effort: AutomaticEffort) async {
        effortSequence += 1
        let request = effortSequence
        desiredEffort = effort
        self.effort = effort
        _ = await perform { try await $0.setEffort(effort) }
        guard effortSequence == request else { return }
        desiredEffort = nil
        self.effort = confirmedEffort
    }

    private func apply(_ status: AutomationStatus) {
        state = status.state
        confirmedLowPower = status.lowPower
        lowPower = desiredLowPower ?? confirmedLowPower
        confirmedEffort = status.effort
        effort = desiredEffort ?? confirmedEffort
        heldBy = status.heldBy
    }

    /// Run mutations in invocation order. The task is deliberately
    /// unstructured: once the agent may have accepted a choice, cancelling
    /// its caller cannot let a later choice overtake it.
    @discardableResult
    private func perform(
        _ request: @escaping @Sendable (any AutomationClient) async throws -> AutomationStatus
    ) async -> Bool {
        generation += 1
        pendingMutations += 1
        mutationSequence += 1
        let sequence = mutationSequence
        let previous = mutationTail
        let task = Task { @MainActor [self] in
            _ = await previous?.value
            error = nil
            do {
                apply(try await request(client))
                return true
            } catch {
                self.error = PairStore.message(for: error)
                return false
            }
        }
        mutationTail = task
        let succeeded = await task.value
        pendingMutations -= 1
        if mutationSequence == sequence { mutationTail = nil }
        return succeeded
    }
}
