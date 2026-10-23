import Foundation
import Testing
@testable import Foldover

@MainActor
private func waitForPulse(_ animation: MenuBarAnimation) async throws -> Bool {
    let deadline = ContinuousClock.now + .seconds(3)
    while animation.imageName(for: .working) == MenuBarStatus.working.rawValue {
        if ContinuousClock.now >= deadline { return false }
        try await Task.sleep(for: .milliseconds(1))
    }
    return true
}

@MainActor
@Test(arguments: [MenuBarStatus.idle, .paused, .attention])
func thePulseStopsAndResetsOutsideWorking(_ status: MenuBarStatus) async throws {
    let animation = MenuBarAnimation(interval: .milliseconds(10))
    animation.update(status: .working, motion: .full)
    #expect(try await waitForPulse(animation))

    // Repeated state notifications must not restart an active pulse.
    let frame = animation.imageName(for: .working)
    animation.update(status: .working, motion: .full)
    #expect(animation.imageName(for: .working) == frame)

    animation.update(status: status, motion: .full)
    #expect(animation.imageName(for: status) == status.rawValue)
    try await Task.sleep(for: .milliseconds(50))
    #expect(animation.imageName(for: .working) == MenuBarStatus.working.rawValue)
}

@MainActor
@Test
func reduceMotionStopsThePulseAndAllowsItToResume() async throws {
    let animation = MenuBarAnimation(interval: .milliseconds(10))
    animation.update(status: .working, motion: .full)
    #expect(try await waitForPulse(animation))

    animation.update(status: .working, motion: .reduced)
    try await Task.sleep(for: .milliseconds(50))
    #expect(animation.imageName(for: .working) == MenuBarStatus.working.rawValue)

    animation.update(status: .working, motion: .full)
    #expect(try await waitForPulse(animation))
    animation.update(status: .idle, motion: .full)
}

@MainActor
@Test
func anActivePulseDoesNotRetainItsOwner() {
    var animation: MenuBarAnimation? = MenuBarAnimation()
    weak var owner = animation
    animation?.update(status: .working, motion: .full)
    animation = nil
    #expect(owner == nil)
}
