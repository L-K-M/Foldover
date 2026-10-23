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
    case starting
    case restarting
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
    /// What identifies this build of the app and the agent it embeds; nil
    /// when it cannot be read.
    var appBuild: Data? { get }
    func register() async throws -> AgentRegistration
    /// The installed service, or a still-enabled service from the build
    /// this app replaced.
    func registrationForUpdate() async -> AgentRegistration
    /// Register the agent anew, for a job launchd keeps but cannot start.
    func reregister() async throws -> AgentRegistration
    func openLoginItems()
}

extension AgentRegistering {
    func registrationForUpdate() async -> AgentRegistration { registration }
}

extension BackgroundAgent: AgentRegistering {}

@Observable
@MainActor
final class AgentStore {
    private enum StatusError: Error {
        case timedOut
    }

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
    /// The app build the agent was last registered from, once seen
    /// answering or awaiting approval.
    private static let registeredBuildKey = "agent.registeredBuild"
    /// Where that is kept: apart from the app's own domain, which a UI tour
    /// resets, so a tour does not make the next launch restart the agent.
    private static let registrationSuite = "ch.lkmc.foldover.login-item"
    private var generation = 0
    /// Turning the agent on in flight. Health checks wait it out rather than
    /// report the states it passes through, and turning on again waits for
    /// it, since both change the login item.
    @ObservationIgnored private var starting: Task<Void, Never>?
    private(set) var connection: AgentConnection = .disconnected {
        didSet {
            switch connection {
            case .connecting, .starting, .restarting: break
            default: lastSettled = connection
            }
        }
    }
    /// The newest answer that was not progress; nil until one
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
         system: (any AgentRegistering)? = nil, defaults: UserDefaults? = nil,
         startupTimeout: Duration = .seconds(10), startupPoll: Duration = .milliseconds(250)) {
        self.statusRequest = statusRequest
        self.leaveSafeModeRequest = leaveSafeModeRequest
        self.alert = alert
        self.system = system ?? BackgroundAgent()
        self.defaults = defaults ?? UserDefaults(suiteName: Self.registrationSuite) ?? .standard
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
        guard starting == nil, !Task.isCancelled else { return }
        generation += 1
        let request = generation
        connection = .connecting
        do {
            let report = try await status(within: startupProbe)
            guard generation == request else { return }
            connection = .connected(version: report.version)
            note(report)
        } catch {
            guard generation == request else { return }
            guard !Task.isCancelled else {
                connection = lastSettled ?? .disconnected
                return
            }
            if error is AgentNotRunning || error is StatusError,
               case .requiresApproval = system.registration {
                connection = .approvalRequired
                return
            }
            connection = .failed(message: message(for: error))
        }
    }

    func enable() async {
        await afterStarting { [self] in
            generation += 1
            let request = generation
            do {
                let before = system.registration
                let registration = try await system.register()
                // Turning the agent on is not a background check: show that
                // it is connecting rather than the answer it replaces.
                lastSettled = nil
                connection = .starting
                // An agent registered before answers within a moment, even
                // while it starts. One that does not is a job launchd cannot
                // start, so it is registered anew rather than waited on.
                // `register` registers only an agent that is off or missing.
                try await start(registration, request: request,
                                firstWait: before == .enabled ? startupProbe : startupTimeout,
                                repairs: Self.repairAttempts,
                                registered: before == .disabled || before == .unavailable)
            } catch {
                guard generation == request else { return }
                connection = .failed(message: message(for: error))
            }
        }
    }

    /// Register the agent anew, which restarts it, once the app is a build
    /// the agent was not registered from: a running agent refuses a changed
    /// app until it restarts (ADR 0006), and launchd cannot start a changed
    /// ad-hoc-signed agent under the old registration (see
    /// `BackgroundAgent.reregister`). A login item that is off or awaits
    /// approval is left as it is. Only a real launch calls this; a test host
    /// must leave the developer's login item alone.
    func reregisterIfUpdated() async {
        await afterStarting { [self] in
            guard await system.registrationForUpdate() == .enabled, let build = system.appBuild,
                  build != defaults.data(forKey: Self.registeredBuildKey) else { return }
            generation += 1
            let request = generation
            connection = .restarting
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
    }

    /// Run `body` once any turning on in flight has finished: both change
    /// the login item, so they must not interleave.
    private func afterStarting(_ body: @escaping @MainActor @Sendable () async -> Void) async {
        let previous = starting
        let task = Task {
            await previous?.value
            await body()
        }
        starting = task
        await task.value
        if starting == task { starting = nil }
    }

    /// Give cold code-identity checks five seconds before repairing an enabled
    /// agent; the full launch window also covers the agent starting.
    private var startupProbe: Duration { startupTimeout / 2 }

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
                // Registering tied the agent to this build; approving it
                // only lets launchd start it.
                if registered { defaults.set(system.appBuild, forKey: Self.registeredBuildKey) }
                connection = .approvalRequired
                return
            }
            connection = .starting
            if let report = try await answer(within: wait, request: request) {
                connection = .connected(version: report.version)
                note(report)
                if registered { defaults.set(system.appBuild, forKey: Self.registeredBuildKey) }
                return
            }
            guard generation == request else { return }
            guard repairsLeft > 0 else {
                connection = .failed(message: String(localized: "The background agent did not start. In System Settings › General › Login Items, turn Foldover off and on, then choose Turn On again."))
                return
            }
            repairsLeft -= 1
            connection = .restarting
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
                let report = try await status(within: ContinuousClock.now.duration(to: deadline))
                return generation == request ? report : nil
            } catch is AgentNotRunning, AgentError.Connection, is StatusError {
                guard ContinuousClock.now < deadline, generation == request else { return nil }
                try await Task.sleep(for: min(startupPoll, ContinuousClock.now.duration(to: deadline)))
            }
        }
    }

    /// UniFFI status calls ignore Swift cancellation. Race the reply without
    /// a task group, which would wait for the cancelled call to finish.
    private func status(within timeout: Duration) async throws -> AgentReport {
        let request = statusRequest
        let (replies, reply) = AsyncStream<Result<AgentReport, Error>>.makeStream(
            bufferingPolicy: .bufferingOldest(1))
        let checking = Task {
            do {
                reply.yield(.success(try await request()))
            } catch {
                reply.yield(.failure(error))
            }
            reply.finish()
        }
        let timer = Task {
            do { try await Task.sleep(for: timeout) } catch { return }
            reply.yield(.failure(StatusError.timedOut))
            reply.finish()
        }
        defer {
            checking.cancel()
            timer.cancel()
            reply.finish()
        }
        for await result in replies { return try result.get() }
        throw CancellationError()
    }

    func openLoginItems() {
        system.openLoginItems()
    }

    private func message(for error: Error) -> String {
        switch error {
        case StatusError.timedOut:
            String(localized: "The background agent is not responding. Choose Turn On to restart it.")
        case let error as AgentNotRunning: error.message
        case AgentError.Connection(let message), AgentError.NotRunning(let message),
             AgentError.Request(let message): message
        default: error.localizedDescription
        }
    }
}
