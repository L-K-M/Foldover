import FoldoverCore
import FoldoverSystem
import Foundation
import Observation

/// What the agent says about itself.
struct AgentReport: Equatable, Sendable {
    let version: String
    /// The unclean stops behind crash-loop safe mode (PLAN.md §4.2), while
    /// syncing waits in it.
    var safeModeStops: UInt32? = nil
}

enum AgentConnection: Equatable {
    case disconnected
    case connecting
    case approvalRequired
    case connected(version: String)
    case failed(message: String)
}

@Observable
@MainActor
final class AgentStore {
    private let system = BackgroundAgent()
    private let statusRequest: @Sendable () async throws -> AgentReport
    private let leaveSafeModeRequest: @Sendable () async throws -> AgentReport
    private let alert: @MainActor (String, String) async -> Void
    private let startupTimeout: Duration = .seconds(10)
    private let startupPoll: Duration = .milliseconds(250)
    private var generation = 0
    private(set) var connection: AgentConnection = .disconnected
    /// The unclean stops behind crash-loop safe mode; nil while the agent
    /// syncs.
    private(set) var safeModeStops: UInt32?
    private(set) var safeModeError: String?

    init(statusRequest: @escaping @Sendable () async throws -> AgentReport = {
        AgentStore.report(try await FoldoverCore.agentStatus())
    }, leaveSafeModeRequest: @escaping @Sendable () async throws -> AgentReport = {
        AgentStore.report(try await FoldoverCore.leaveSafeMode())
    }, alert: @escaping @MainActor (String, String) async -> Void = AgentStore.postAlert) {
        self.statusRequest = statusRequest
        self.leaveSafeModeRequest = leaveSafeModeRequest
        self.alert = alert
    }

    nonisolated private static func report(_ health: AgentHealth) -> AgentReport {
        AgentReport(version: health.coreVersion, safeModeStops: health.safeModeStops)
    }

    private static let alerts = UserAlerts()

    private static func postAlert(title: String, body: String) async {
        await alerts.post(identifier: "safe-mode", title: title, body: body)
    }

    /// Sync again after a crash loop.
    func leaveSafeMode() async {
        do {
            note(try await leaveSafeModeRequest())
            safeModeError = nil
        } catch {
            safeModeError = message(for: error)
        }
    }

    /// Take the agent's report; entering safe mode is worth a
    /// notification, since nothing syncs until a person resumes.
    private func note(_ report: AgentReport) {
        let entering = safeModeStops == nil && report.safeModeStops != nil
        safeModeStops = report.safeModeStops
        guard entering, let stops = report.safeModeStops else { return }
        let title = String(localized: "Foldover paused syncing")
        let body = String(localized: "The background agent stopped unexpectedly \(stops) times in a few minutes. Open Foldover to resume.")
        Task { await alert(title, body) }
    }

    func refresh() async {
        generation += 1
        let request = generation
        connection = .connecting
        do {
            let report = try await statusRequest()
            guard generation == request else { return }
            connection = .connected(version: report.version)
            note(report)
        } catch {
            guard generation == request else { return }
            if case AgentError.NotRunning = error,
               case .requiresApproval = system.registration {
                connection = .approvalRequired
                return
            }
            connection = .failed(message: message(for: error))
        }
    }

    func enable() async {
        generation += 1
        let request = generation
        do {
            let registration = try system.register()
            if case .requiresApproval = registration {
                connection = .approvalRequired
                return
            }
            connection = .connecting
            let deadline = ContinuousClock.now.advanced(by: startupTimeout)
            while ContinuousClock.now < deadline {
                do {
                    let report = try await statusRequest()
                    guard generation == request else { return }
                    connection = .connected(version: report.version)
                    note(report)
                    return
                } catch AgentError.NotRunning {
                    // Registration returns before launchd has bound the socket.
                    try await Task.sleep(for: startupPoll)
                    guard generation == request else { return }
                }
            }
            connection = .failed(message: String(localized: "The registered agent did not start. Check Login Items and refresh."))
        } catch {
            guard generation == request else { return }
            connection = .failed(message: message(for: error))
        }
    }

    func openLoginItems() {
        system.openLoginItems()
    }

    private func message(for error: Error) -> String {
        switch error {
        case AgentError.Connection(let message), AgentError.NotRunning(let message),
             AgentError.Request(let message): message
        default: error.localizedDescription
        }
    }
}
