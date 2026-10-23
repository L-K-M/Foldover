import Foundation
import Security
import ServiceManagement

public enum AgentRegistration: Sendable {
    case disabled
    case enabled
    case requiresApproval
    case unavailable
}

@MainActor
public final class BackgroundAgent {
    private static let label = "ch.lkmc.foldover.agent"
    private static let program = "Contents/Library/LaunchServices/foldoverd"
    /// How long launchd may take to drop the job after it is unregistered.
    private static let forgetTimeout: Duration = .seconds(10)
    private static let forgetPoll: Duration = .milliseconds(250)
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

    /// The embedded agent's code directory hash. It changes with every build
    /// of the ad-hoc-signed agent, and launchd ties the job's launch
    /// constraint to it; nil when the code signature cannot be read.
    public var agentBuild: Data? {
        let url = Bundle.main.bundleURL.appending(path: Self.program)
        var code: SecStaticCode?
        guard SecStaticCodeCreateWithPath(url as CFURL, [], &code) == errSecSuccess, let code else { return nil }
        var information: CFDictionary?
        let flags = SecCSFlags(rawValue: kSecCSSigningInformation)
        guard SecCodeCopySigningInformation(code, flags, &information) == errSecSuccess,
              let information = information as? [String: Any] else { return nil }
        return information[kSecCodeInfoUnique as String] as? Data
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

    /// Register the agent anew. Registering pins the job's launch
    /// constraint to the agent's code hash, which an ad-hoc-signed agent
    /// changes with every build, so once the app is replaced launchd refuses
    /// the new agent ("spawn failed", exit 78, "needs LWCR update") while the
    /// login item still reads as enabled. Registering again while launchd
    /// still holds the old job keeps its constraint, so the job is
    /// unregistered and stopped, and launchd given time to drop it, first.
    public func reregister() async throws -> AgentRegistration {
        await unregister()
        _ = await Self.launchctl("bootout", Self.target)
        let deadline = ContinuousClock.now.advanced(by: Self.forgetTimeout)
        while await Self.launchctl("print", Self.target) == 0, ContinuousClock.now < deadline {
            try await Task.sleep(for: Self.forgetPoll)
        }
        try service.register()
        return registration
    }

    public func openLoginItems() {
        SMAppService.openSystemSettingsLoginItems()
    }

    /// Unregister without holding up the main actor. It fails for a job
    /// launchd has already dropped, which is as good: whether the agent
    /// answers once registered again is what callers check.
    private func unregister() async {
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            service.unregister { @Sendable _ in done.resume() }
        }
    }

    /// The agent's job in this user's launchd domain.
    private static var target: String { "gui/\(getuid())/\(label)" }

    /// Run launchctl quietly; its exit status, or nil when it cannot run.
    private nonisolated static func launchctl(_ arguments: String...) async -> Int32? {
        await withCheckedContinuation { continuation in
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
            process.arguments = arguments
            process.standardOutput = FileHandle.nullDevice
            process.standardError = FileHandle.nullDevice
            process.terminationHandler = { continuation.resume(returning: $0.terminationStatus) }
            do {
                try process.run()
            } catch {
                continuation.resume(returning: nil)
            }
        }
    }
}
