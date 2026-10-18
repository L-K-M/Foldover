// The app shell (ADR docs/adr/0003-diff-tree-view.md): launched with
// `-runBench <report.json>` — a launch argument `open --args` lands in
// the argument domain — the app runs each implementation through
// `BenchRunner`, writes the JSON report, prints the summary to stdout
// and quits. Without it the app shows a status window only.
//
// Adapted from L-K-M/Sweetbriar (AppDelegate.swift): the
// launch-argument run, the write-then-print-then-terminate finish and
// the run-loop quit carry over; the session and menus do not exist
// here.

import AppKit
import SwiftUI

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    /// Each five-minute run gets a minute for bounded waits; reporting gets
    /// another minute. Adding a candidate must not truncate later runs.
    private static let runAndWaitSeconds: TimeInterval = 360
    private static let reportGraceSeconds: TimeInterval = 60
    private static let watchdogSeconds = TimeInterval(Implementation.allCases.count)
        * runAndWaitSeconds + reportGraceSeconds

    func applicationDidFinishLaunching(_ notification: Notification) {
        guard let reportPath = UserDefaults.standard.string(forKey: "runBench")
        else {
            showIdleWindow()
            return
        }
        armWatchdog(reportPath: reportPath)
        Task { @MainActor in
            let report = await BenchRunner().run()
            finish(report: report, at: reportPath)
        }
    }

    /// `-runBench` finished: write the report, print the summary, quit.
    private func finish(report: DiffTreeBenchReport, at path: String) {
        let data = (try? report.encoded()) ?? Data(
            #"{"harnessOK":false,"failures":["the report failed to encode"],"verdict":"incomplete"}"#
                .utf8
        )
        do {
            try data.write(
                to: URL(fileURLWithPath: path),
                options: .atomic
            )
        } catch {
            FileHandle.standardError.write(
                Data(
                    ("s5-diff-tree: couldn't write \(path): "
                        + "\(error.localizedDescription)\n").utf8
                )
            )
        }
        FileHandle.standardOutput.write(
            Data((report.stdoutSummary + "\n").utf8)
        )
        // Not from this task: it runs as a main-queue job, and the run
        // loop `.terminateLater` spins doesn't drain the main queue
        // inside one. A run-loop block runs outside the queue.
        RunLoop.main.perform {
            MainActor.assumeIsolated {
                NSApp.terminate(nil)
            }
        }
    }

    /// No `-runBench`: a status window and nothing else — a human
    /// launching the app sees what it is for.
    private func showIdleWindow() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 480, height: 140),
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.isReleasedWhenClosed = false
        window.title = "DiffTreeBench"
        window.contentView = NSHostingView(
            rootView: Text(
                "Launch with -runBench <report.json> to measure"
                    + " the diff-tree implementations."
            )
            .padding()
        )
        window.center()
        window.makeKeyAndOrderFront(nil)
    }

    /// The watchdog fires on a global queue — it must run even with
    /// the main thread wedged, which is exactly the case it exists
    /// for. If the report still isn't written, the run is recorded as
    /// wedged and the app dies so `open -W` unblocks.
    private func armWatchdog(reportPath: String) {
        DispatchQueue.global(qos: .utility).asyncAfter(
            deadline: .now() + Self.watchdogSeconds
        ) {
            if FileManager.default.fileExists(atPath: reportPath) {
                exit(0)
            }
            let wedged = Data(
                """
                {"checks":[],"failures":["watchdog: the benchmark did \
                not finish inside its ceiling; the stall itself is \
                the measurement"],"harnessOK":false,"runs":[],\
                "verdict":"wedged"}
                """.utf8
            )
            try? wedged.write(
                to: URL(fileURLWithPath: reportPath),
                options: .atomic
            )
            FileHandle.standardError.write(
                Data("s5-diff-tree: watchdog fired; report written as wedged\n".utf8)
            )
            exit(2)
        }
    }
}
