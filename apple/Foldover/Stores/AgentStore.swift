import FoldoverCore
import FoldoverSystem
import Foundation
import Observation

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
    private let statusRequest: @Sendable () async throws -> String
    private let startupTimeout: Duration = .seconds(10)
    private let startupPoll: Duration = .milliseconds(250)
    private var generation = 0
    private(set) var connection: AgentConnection = .disconnected

    init(statusRequest: @escaping @Sendable () async throws -> String = {
        let status = try await FoldoverCore.agentStatus()
        return status.coreVersion
    }) {
        self.statusRequest = statusRequest
    }

    func refresh() async {
        generation += 1
        let request = generation
        connection = .connecting
        do {
            let version = try await statusRequest()
            guard generation == request else { return }
            connection = .connected(version: version)
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
                    let version = try await statusRequest()
                    guard generation == request else { return }
                    connection = .connected(version: version)
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
        case AgentError.Connection(let message), AgentError.NotRunning(let message): message
        default: error.localizedDescription
        }
    }
}
