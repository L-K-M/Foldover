import AppKit
import Darwin

/// System Settings panes Foldover sends people to.
@MainActor
public enum PrivacySettings {
    private static let fullDiskAccess =
        URL(string: "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles")

    /// The Full Disk Access list, where Foldover can be allowed to read
    /// protected folders and external or network volumes.
    public static func openFullDiskAccess() {
        guard let fullDiskAccess else { return }
        NSWorkspace.shared.open(fullDiskAccess)
    }

    /// Clears only Foldover's grant. The user must grant access again in
    /// System Settings; this never edits the privacy database or grants access.
    public static func resetFullDiskAccess() async throws {
        try await PrivacyResetCommand.run(
            executable: URL(fileURLWithPath: "/usr/bin/tccutil"),
            arguments: ["reset", "SystemPolicyAllFiles", "ch.lkmc.foldover"])
    }
}

/// Keep the system tool off the main actor and bound its lifetime. Tests use
/// harmless executables through this internal boundary, never real TCC grants.
enum PrivacyResetCommand {
    enum Failure: LocalizedError {
        case unsuccessful
        case timedOut

        var errorDescription: String? {
            switch self {
            case .unsuccessful:
                String(localized: "macOS could not reset Foldover's Full Disk Access. Open Settings and remove the old Foldover entry, then add the app from Applications again.")
            case .timedOut:
                String(localized: "Resetting Full Disk Access took too long. Open Settings and remove the old Foldover entry, then add the app from Applications again.")
            }
        }
    }

    static func run(executable: URL, arguments: [String], timeout: TimeInterval = 10) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            DispatchQueue.global(qos: .userInitiated).async {
                let process = Process()
                process.executableURL = executable
                process.arguments = arguments
                process.standardOutput = FileHandle.nullDevice
                process.standardError = FileHandle.nullDevice
                let finished = DispatchSemaphore(value: 0)
                process.terminationHandler = { _ in finished.signal() }
                do {
                    do { try process.run() } catch { throw Failure.unsuccessful }
                    guard finished.wait(timeout: .now() + timeout) == .success else {
                        process.terminate()
                        if finished.wait(timeout: .now() + 1) == .timedOut {
                            kill(process.processIdentifier, SIGKILL)
                        }
                        throw Failure.timedOut
                    }
                    guard process.terminationStatus == 0 else { throw Failure.unsuccessful }
                    continuation.resume()
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
}
