import AppKit
import Combine
import Observation

enum MotionPreference: Equatable, Sendable {
    case full
    case reduced
}

@Observable
@MainActor
final class MotionStore {
    private(set) var preference = MotionPreference.full
    @ObservationIgnored private var changes: AnyCancellable?

    init() {
        refresh()
        // The native status item has no view accessibility environment.
        changes = NSWorkspace.shared.notificationCenter
            .publisher(for: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification)
            .sink { [weak self] _ in
                Task { @MainActor [weak self] in self?.refresh() }
            }
    }

    #if DEBUG
    /// Canned UI scenarios and unit tests use an explicit motion policy.
    init(preference: MotionPreference) {
        self.preference = preference
    }
    #endif

    private func refresh() {
        preference = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion ? .reduced : .full
    }
}
