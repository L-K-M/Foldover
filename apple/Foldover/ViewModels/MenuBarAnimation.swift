import Foundation
import Observation

@Observable
@MainActor
final class MenuBarAnimation {
    private static let workingFrames = [
        "MenuBarArrowWorking", "MenuBarArrowWorkingSoft",
        "MenuBarArrowWorkingDim", "MenuBarArrowWorkingSoft",
    ]
    private var frame = 0
    private let interval: Duration
    @ObservationIgnored private var task: Task<Void, Never>?

    init(interval: Duration = .milliseconds(400)) {
        self.interval = interval
    }

    deinit { task?.cancel() }

    func imageName(for status: MenuBarStatus) -> String {
        status == .working ? Self.workingFrames[frame] : status.rawValue
    }

    func update(status: MenuBarStatus, motion: MotionPreference) {
        guard status == .working, motion == .full else {
            task?.cancel()
            task = nil
            frame = 0
            return
        }
        guard task == nil else { return }

        // Sleep before advancing; idle and Reduce Motion need no timer wakeups.
        task = Task { [weak self, interval] in
            while !Task.isCancelled {
                do { try await Task.sleep(for: interval) }
                catch { return }

                guard !Task.isCancelled, let self else { return }
                self.frame = (self.frame + 1) % Self.workingFrames.count
            }
        }
    }
}
