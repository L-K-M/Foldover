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
    @ObservationIgnored private var motion: MotionStore?

    init(interval: Duration = .milliseconds(400)) {
        self.interval = interval
    }

    convenience init(viewModel: MainViewModel) {
        self.init()
        let motion = MotionStore()
        self.motion = motion
        track(viewModel: viewModel, motion: motion)
    }

    deinit { task?.cancel() }

    private func track(viewModel: MainViewModel, motion: MotionStore) {
        // MenuBarExtra scene callbacks are not a window-independent clock.
        let (status, preference) = withObservationTracking {
            (viewModel.menuBarStatus, motion.preference)
        } onChange: { [weak self, weak viewModel, weak motion] in
            Task { @MainActor [weak self, weak viewModel, weak motion] in
                guard let self, let viewModel, let motion else { return }
                self.track(viewModel: viewModel, motion: motion)
            }
        }
        update(status: status, motion: preference)
    }

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
