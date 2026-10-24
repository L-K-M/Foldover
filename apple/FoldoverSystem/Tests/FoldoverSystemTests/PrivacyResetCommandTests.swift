import Darwin
import Foundation
import Testing
@testable import FoldoverSystem

@Suite(.serialized)
struct PrivacyResetCommandTests {
    @Test
    func successfulCommandReturns() async throws {
        try await PrivacyResetCommand.run(
            executable: URL(fileURLWithPath: "/usr/bin/true"), arguments: [])
    }

    @Test
    func unsuccessfulCommandReportsFailure() async {
        do {
            try await PrivacyResetCommand.run(
                executable: URL(fileURLWithPath: "/usr/bin/false"), arguments: [])
            Issue.record("A nonzero exit must report a reset failure")
        } catch PrivacyResetCommand.Failure.unsuccessful {
        } catch {
            Issue.record("Unexpected command failure: \(error)")
        }
    }

    @Test
    func launchFailureHasRecoveryGuidance() async {
        do {
            try await PrivacyResetCommand.run(
                executable: URL(fileURLWithPath: "/no-such-foldover-reset-command"), arguments: [])
            Issue.record("A command that cannot launch must report a reset failure")
        } catch PrivacyResetCommand.Failure.unsuccessful {
        } catch {
            Issue.record("Unexpected launch failure: \(error)")
        }
    }

    @Test
    func timeoutReturnsPromptlyAndReapsItsChild() async throws {
        let started = ContinuousClock.now
        let command = Task {
            do {
                try await PrivacyResetCommand.run(
                    executable: URL(fileURLWithPath: "/bin/sleep"),
                    arguments: ["2"], timeout: 0.25)
                Issue.record("A command past its deadline must report a timeout")
            } catch PrivacyResetCommand.Failure.timedOut {
            } catch {
                Issue.record("Unexpected timeout failure: \(error)")
            }
        }
        defer { command.cancel() }

        var child: pid_t?
        let findDeadline = started.advanced(by: .seconds(1))
        repeat {
            child = try sleepChild()
            if child != nil { break }
            try await Task.sleep(for: .milliseconds(10))
        } while ContinuousClock.now < findDeadline
        let pid = try #require(child, "The timeout test must observe its own sleep child")

        await command.value
        #expect(started.duration(to: .now) < .seconds(2))

        let reapDeadline = ContinuousClock.now.advanced(by: .seconds(1))
        while kill(pid, 0) == 0, ContinuousClock.now < reapDeadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        // A zombie still has a PID; ESRCH proves the child has been reaped.
        let exists = kill(pid, 0)
        let error = errno
        #expect(exists == -1 && error == ESRCH)
    }

    private func sleepChild() throws -> pid_t? {
        let process = Process()
        let output = Pipe()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/pgrep")
        process.arguments = ["-P", String(getpid()), "-x", "sleep"]
        process.standardOutput = output
        process.standardError = FileHandle.nullDevice
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else { return nil }
        let text = String(decoding: output.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        return text.split(whereSeparator: \.isWhitespace).first.flatMap { pid_t($0) }
    }
}
