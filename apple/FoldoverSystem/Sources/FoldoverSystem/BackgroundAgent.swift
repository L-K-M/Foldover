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
    private static let labelPrefix = "ch.lkmc.foldover.agent"
    /// How long launchd may take to drop the job after it is unregistered.
    private static let forgetTimeout: Duration = .seconds(10)
    private static let forgetPoll: Duration = .milliseconds(250)
    /// Pauses before registering again when registering straight after
    /// unregistering fails: the system can refuse it ("Operation not
    /// permitted") until it has finished unregistering.
    private static let registerRetries: [Duration] = [.milliseconds(500), .seconds(1), .seconds(2)]
    private let service: SMAppService
    private let label: String?
    private let labelPrefix: String

    public convenience init() {
        self.init(plistName: "ch.lkmc.foldover.agent.plist", labelPrefix: Self.labelPrefix)
    }

    /// Also used by the native lifecycle fixture, which owns a separate
    /// service namespace and never registers the installed agent.
    init(plistName: String, labelPrefix: String) {
        self.labelPrefix = labelPrefix
        service = SMAppService.agent(plistName: plistName)
        let url = Bundle.main.bundleURL.appendingPathComponent("Contents/Library/LaunchAgents/\(plistName)")
        let plist = try? Data(contentsOf: url)
        let contents = plist.flatMap { try? PropertyListSerialization.propertyList(from: $0, format: nil) }
        let candidate = (contents as? [String: Any])?["Label"] as? String
        label = candidate.flatMap { Self.owns($0, prefix: labelPrefix) ? $0 : nil }
    }

    public var registration: AgentRegistration {
        guard label != nil else { return .unavailable }
        return switch service.status {
        case .notRegistered: .disabled
        case .enabled: .enabled
        case .requiresApproval: .requiresApproval
        case .notFound: .unavailable
        @unknown default: .unavailable
        }
    }

    /// The app's code directory hash; nil when the code signature cannot be
    /// read. The app's signature seals the embedded agent and CLI, so it
    /// changes whenever any of them does. A running agent refuses a changed
    /// app until it restarts (ADR 0006), and launchd refuses a changed
    /// agent under its old registration.
    public var appBuild: Data? {
        let url = Bundle.main.bundleURL
        var code: SecStaticCode?
        guard SecStaticCodeCreateWithPath(url as CFURL, [], &code) == errSecSuccess, let code else { return nil }
        var information: CFDictionary?
        let flags = SecCSFlags(rawValue: kSecCSSigningInformation)
        guard SecCodeCopySigningInformation(code, flags, &information) == errSecSuccess,
              let information = information as? [String: Any] else { return nil }
        return information[kSecCodeInfoUnique as String] as? Data
    }

    /// A new helper gets a new launchd label because Service Management
    /// retains an ad-hoc helper's old code constraint even after it has been
    /// unregistered. Only an existing enabled job authorizes migration;
    /// saved preferences alone must not undo the person's Login Items choice.
    public func registrationForUpdate() async -> AgentRegistration {
        let current = registration
        guard current == .disabled || current == .unavailable, let label,
              let previous = try? await ownedJobs(),
              let disabled = await Self.launchctl("print-disabled", Self.domain),
              disabled.status == 0 else { return current }
        let enabled = Set(disabled.output.split(separator: "\n").compactMap { row -> String? in
            let fields = row.split(whereSeparator: \.isWhitespace)
            guard fields.count == 3, fields[1] == "=>",
                  fields[2] == "enabled" || fields[2] == "false",
                  fields[0].first == "\"", fields[0].last == "\"" else { return nil }
            return String(fields[0].dropFirst().dropLast())
        })
        let older = previous.filter { $0 != label }
        // Mixed old states cannot establish the person's latest choice.
        if !older.isEmpty, older.allSatisfy({ enabled.contains($0) }) { return .enabled }
        return current
    }

    public func register() async throws -> AgentRegistration {
        switch registration {
        case .enabled, .requiresApproval:
            return registration
        case .disabled, .unavailable:
            try await removeJobs(except: label)
            try service.register()
            return registration
        }
    }

    /// Register the agent anew, which also restarts it from this app.
    /// Stop the current and previous build's jobs before registering. A
    /// changed helper has a new label, so Service Management derives a fresh
    /// exact code constraint instead of reusing the previous build's hash.
    public func reregister() async throws -> AgentRegistration {
        await unregister()
        try await removeJobs()
        for pause in Self.registerRetries {
            do {
                try service.register()
                return registration
            } catch {
                try await Task.sleep(for: pause)
            }
        }
        try service.register()
        return registration
    }

    private enum RegistrationError: LocalizedError {
        case invalidLabel
        case cannotReadJobs
        case didNotStop

        var errorDescription: String? {
            switch self {
            case .invalidLabel: "The background agent's launch configuration is invalid. Install Foldover again."
            case .cannotReadJobs: "Foldover could not check the previous background agent. Try Turn On again."
            case .didNotStop: "The previous background agent did not stop. Quit Foldover and try again."
            }
        }
    }

    /// `launchctl list` has documented tabular output. Admit only this
    /// application's legacy label and labels containing a full code hash.
    private func ownedJobs() async throws -> [String] {
        guard let jobs = await Self.launchctl("list"), jobs.status == 0 else {
            throw RegistrationError.cannotReadJobs
        }
        return jobs.output.split(separator: "\n").compactMap { row in
            let columns = row.split(whereSeparator: \.isWhitespace)
            guard columns.count == 3 else { return nil }
            let candidate = String(columns[2])
            return Self.owns(candidate, prefix: labelPrefix) ? candidate : nil
        }
    }

    private nonisolated static func owns(_ label: String, prefix: String) -> Bool {
        if label == prefix { return true }
        guard label.hasPrefix(prefix + ".") else { return false }
        let suffix = label.dropFirst(prefix.count + 1)
        return suffix.utf8.count == 40 && suffix.utf8.allSatisfy {
            (48...57).contains($0) || (97...102).contains($0)
        }
    }

    private func removeJobs(except kept: String? = nil) async throws {
        guard let label else { throw RegistrationError.invalidLabel }
        var jobs = Set(try await ownedJobs())
        jobs.insert(label)
        for job in jobs where job != kept {
            let target = "\(Self.domain)/\(job)"
            _ = await Self.launchctl("bootout", target)
            let deadline = ContinuousClock.now.advanced(by: Self.forgetTimeout)
            while true {
                guard let check = await Self.launchctl("print", target) else {
                    throw RegistrationError.cannotReadJobs
                }
                if check.status != 0 {
                    // A failed print can also mean an OS/permission error.
                    // Require a successful inventory confirming absence.
                    let remaining = try await ownedJobs()
                    guard !remaining.contains(job) else {
                        throw RegistrationError.didNotStop
                    }
                    break
                }
                guard ContinuousClock.now < deadline else { throw RegistrationError.didNotStop }
                try await Task.sleep(for: Self.forgetPoll)
            }
        }
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
    private static var domain: String { "gui/\(getuid())" }

    private struct LaunchctlResult: Sendable {
        let status: Int32
        let output: String
    }

    /// Run launchctl quietly; its exit status, or nil when it cannot run.
    private nonisolated static func launchctl(_ arguments: String...) async -> LaunchctlResult? {
        await withCheckedContinuation { continuation in
            DispatchQueue.global().async {
                let process = Process()
                let output = Pipe()
                process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
                process.arguments = arguments
                process.standardOutput = output
                process.standardError = FileHandle.nullDevice
                do {
                    try process.run()
                    let data = output.fileHandleForReading.readDataToEndOfFile()
                    process.waitUntilExit()
                    continuation.resume(returning: LaunchctlResult(
                        status: process.terminationStatus, output: String(decoding: data, as: UTF8.self)))
                } catch {
                    continuation.resume(returning: nil)
                }
            }
        }
    }
}
