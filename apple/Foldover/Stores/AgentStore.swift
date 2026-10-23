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

/// The agent did not answer because it is not running: its socket is
/// missing or refuses connections.
struct AgentNotRunning: Error, Equatable {
    let message: String
}

/// The agent's login item: the system's (`BackgroundAgent`), or a stand-in
/// in tests and UI tours.
@MainActor
protocol AgentRegistering {
    var registration: AgentRegistration { get }
    /// What identifies the agent's build; nil when it cannot be read.
    var agentBuild: Data? { get }
    func register() throws -> AgentRegistration
    /// Register the agent anew, for a job launchd keeps but cannot start.
    func reregister() async throws -> AgentRegistration
    func openLoginItems()
}

extension BackgroundAgent: AgentRegistering {}

@Observable
@MainActor
final class AgentStore {
    private let system: any AgentRegistering
    private let statusRequest: @Sendable () async throws -> AgentReport
    private let leaveSafeModeRequest: @Sendable () async throws -> AgentReport
    private let alert: @MainActor (String, String) async -> Void
    private let defaults: UserDefaults
    private let startupTimeout: Duration
    private let startupPoll: Duration
    /// Registrations anew before turning on gives up: the first can leave
    /// launchd's old job in place.
    private static let repairAttempts = 2
    /// The agent build last registered and seen answering.
    private static let registeredBuildKey = "agent.registeredBuild"
    private var generation = 0
    /// The request turning the agent on; health checks wait it out rather
    /// than report the states it passes through.
    @ObservationIgnored private var startingRequest: Int?
    private(set) var connection: AgentConnection = .disconnected {
        didSet {
            if connection != .connecting { lastSettled = connection }
        }
    }
    /// The newest answer that was not `.connecting`; nil until one
    /// arrives. A health check in flight can show this instead of
    /// flickering to "connecting" on every check.
    private(set) var lastSettled: AgentConnection?
    /// The unclean stops behind crash-loop safe mode; nil while the agent
    /// syncs.
    private(set) var safeModeStops: UInt32?
    private(set) var safeModeError: String?

    init(statusRequest: @escaping @Sendable () async throws -> AgentReport = {
        do {
            return AgentStore.report(try await FoldoverCore.agentStatus())
        } catch AgentError.NotRunning(let message) {
            throw AgentNotRunning(message: message)
        }
    }, leaveSafeModeRequest: @escaping @Sendable () async throws -> AgentReport = {
        AgentStore.report(try await FoldoverCore.leaveSafeMode())
    }, alert: @escaping @MainActor (String, String) async -> Void = AgentStore.postAlert,
         system: (any AgentRegistering)? = nil, defaults: UserDefaults = .standard,
         startupTimeout: Duration = .seconds(10), startupPoll: Duration = .milliseconds(250)) {
        self.statusRequest = statusRequest
        self.leaveSafeModeRequest = leaveSafeModeRequest
        self.alert = alert
        self.system = system ?? BackgroundAgent()
        self.defaults = defaults
        self.startupTimeout = startupTimeout
        self.startupPoll = startupPoll
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
        // A status asked for before resuming must not put the app back in
        // safe mode: this reply is newer.
        generation += 1
        let request = generation
        do {
            let report = try await leaveSafeModeRequest()
            guard generation == request else { return }
            connection = .connected(version: report.version)
            note(report)
            safeModeError = nil
        } catch {
            guard generation == request else { return }
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
        guard startingRequest == nil else { return }
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
            if error is AgentNotRunning,
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
        startingRequest = request
        defer { if startingRequest == request { startingRequest = nil } }
        do {
            let wasEnabled = system.registration == .enabled
            let registration = try system.register()
            if case .requiresApproval = registration {
                connection = .approvalRequired
                return
            }
            // Turning the agent on is not a background check: show that it
            // is connecting rather than the answer it replaces.
            lastSettled = nil
            connection = .connecting
            // An agent registered before should already answer. One that does
            // not is a job launchd cannot start, so it is registered anew at
            // once rather than after the wait.
            try await start(registration, request: request, firstWait: wasEnabled ? .zero : startupTimeout,
                            repairs: Self.repairAttempts, registered: !wasEnabled)
        } catch {
            guard generation == request else { return }
            connection = .failed(message: message(for: error))
        }
    }

    /// Register the agent anew once the app holds a build it has not run:
    /// launchd cannot start a new ad-hoc-signed build under the old
    /// registration (see `BackgroundAgent.reregister`). A login item that is
    /// off or awaits approval is left as it is.
    func reregisterIfUpdated() async {
        guard system.registration == .enabled, let build = system.agentBuild,
              build != defaults.data(forKey: Self.registeredBuildKey) else { return }
        generation += 1
        let request = generation
        startingRequest = request
        defer { if startingRequest == request { startingRequest = nil } }
        connection = .connecting
        do {
            let registration = try await system.reregister()
            guard generation == request else { return }
            try await start(registration, request: request, firstWait: startupTimeout,
                            repairs: Self.repairAttempts - 1, registered: true)
        } catch {
            guard generation == request else { return }
            connection = .failed(message: message(for: error))
        }
    }

    /// Wait for the agent behind `registration` to answer, registering it
    /// anew while it does not, up to `repairs` times. `registered` says the
    /// registration is this request's own, so an answer confirms the build.
    private func start(_ registration: AgentRegistration, request: Int, firstWait: Duration,
                       repairs: Int, registered: Bool) async throws {
        var registration = registration
        var wait = firstWait
        var repairsLeft = repairs
        var registered = registered
        while true {
            if case .requiresApproval = registration {
                connection = .approvalRequired
                return
            }
            if let report = try await answer(within: wait, request: request) {
                connection = .connected(version: report.version)
                note(report)
                if registered { defaults.set(system.agentBuild, forKey: Self.registeredBuildKey) }
                return
            }
            guard generation == request else { return }
            guard repairsLeft > 0 else {
                connection = .failed(message: String(localized: "The background agent did not start. In System Settings › General › Login Items, turn Foldover off and on, then choose Turn On again."))
                return
            }
            repairsLeft -= 1
            registration = try await system.reregister()
            guard generation == request else { return }
            registered = true
            wait = startupTimeout
        }
    }

    /// The agent's report once it answers within `timeout`, after at least
    /// one try; nil when it does not, or when a newer request took over.
    /// Registration returns before launchd has started the agent.
    private func answer(within timeout: Duration, request: Int) async throws -> AgentReport? {
        let deadline = ContinuousClock.now.advanced(by: timeout)
        while true {
            do {
                let report = try await statusRequest()
                return generation == request ? report : nil
            } catch is AgentNotRunning {
                guard ContinuousClock.now < deadline, generation == request else { return nil }
                try await Task.sleep(for: startupPoll)
            }
        }
    }

    func openLoginItems() {
        system.openLoginItems()
    }

    private func message(for error: Error) -> String {
        switch error {
        case let error as AgentNotRunning: error.message
        case AgentError.Connection(let message), AgentError.NotRunning(let message),
             AgentError.Request(let message): message
        default: error.localizedDescription
        }
    }
}
