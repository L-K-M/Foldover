import ServiceManagement

public enum AgentRegistration: Sendable {
    case disabled
    case enabled
    case requiresApproval
    case unavailable
}

@MainActor
public final class BackgroundAgent {
    private let service = SMAppService.agent(plistName: "ch.lkmc.foldover.agent.plist")

    public init() {}

    public var registration: AgentRegistration {
        switch service.status {
        case .notRegistered: .disabled
        case .enabled: .enabled
        case .requiresApproval: .requiresApproval
        case .notFound: .unavailable
        @unknown default: .unavailable
        }
    }

    public func register() throws -> AgentRegistration {
        switch registration {
        case .enabled, .requiresApproval:
            return registration
        case .disabled, .unavailable:
            try service.register()
            return registration
        }
    }

    public func openLoginItems() {
        SMAppService.openSystemSettingsLoginItems()
    }
}
