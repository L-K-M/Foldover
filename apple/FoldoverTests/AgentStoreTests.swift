import Foundation
import FoldoverSystem
import Testing
@testable import Foldover

@MainActor
@Test
func agentRefreshReportsTheConnectedVersion() async {
    let store = AgentStore(statusRequest: { AgentReport(version: "test-version") })
    await store.refresh()
    #expect(store.connection == .connected(version: "test-version"))
}

private actor DeferredStatus {
    private var calls = 0
    private var first: CheckedContinuation<String, Error>?
    private var started: CheckedContinuation<Void, Never>?

    func status() async throws -> String {
        calls += 1
        if calls > 1 { return "new" }
        return try await withCheckedThrowingContinuation { continuation in
            first = continuation
            started?.resume()
            started = nil
        }
    }

    func waitUntilStarted() async {
        if first != nil { return }
        await withCheckedContinuation { started = $0 }
    }

    func finishFirst() {
        first?.resume(returning: "old")
        first = nil
    }
}

@MainActor
@Test
func staleAgentRefreshCannotReplaceANewerResult() async {
    let client = DeferredStatus()
    let store = AgentStore(statusRequest: { AgentReport(version: try await client.status()) })
    let older = Task { await store.refresh() }
    await client.waitUntilStarted()
    await store.refresh()
    await client.finishFirst()
    await older.value
    #expect(store.connection == .connected(version: "new"))
}

private actor SafeModeAgent {
    private(set) var stops: UInt32? = 3

    func status() -> AgentReport { AgentReport(version: "1.0", safeModeStops: stops) }

    func resume() -> AgentReport {
        stops = nil
        return status()
    }
}

private actor Alerts {
    private(set) var posted: [String] = []
    func post(_ title: String) { posted.append(title) }
}

/// PLAN.md §4.2: crash-loop safe mode shows, notifies once, and ends when
/// the person resumes.
@MainActor
@Test
func safeModeShowsNotifiesOnceAndResumes() async {
    let agent = SafeModeAgent()
    let alerts = Alerts()
    let store = AgentStore(
        statusRequest: { await agent.status() },
        leaveSafeModeRequest: { await agent.resume() },
        alert: { title, _ in await alerts.post(title) }
    )
    await store.refresh()
    await store.refresh()
    #expect(store.safeModeStops == 3)
    let viewModel = MainViewModel(appInfo: AppInfoStore(), agent: store)
    #expect(viewModel.safeModeText?.contains("stopped unexpectedly 3 times") == true)
    // The notification is posted from its own task, after the refresh.
    for _ in 0..<1_000 where await alerts.posted.isEmpty {
        try? await Task.sleep(for: .milliseconds(1))
    }
    #expect(await alerts.posted == ["Foldover paused syncing"])

    await viewModel.leaveSafeMode()
    #expect(store.safeModeStops == nil)
    #expect(viewModel.safeModeText == nil)
}

/// A login item whose agent launchd keeps but cannot start until it is
/// registered anew, as after an ad-hoc-signed build replaced the app.
@MainActor
private final class StaleLoginItem: AgentRegistering {
    var registration: AgentRegistration
    var agentBuild: Data? = Data("new build".utf8)
    /// What registering anew leaves the login item as.
    var afterReregistering: AgentRegistration = .enabled
    /// How many times the agent must be registered anew before launchd
    /// starts it; nil when it never does.
    private let startsAfter: Int?
    private(set) var reregistrations = 0

    init(_ registration: AgentRegistration = .enabled, startsAfter: Int?) {
        self.registration = registration
        self.startsAfter = startsAfter
    }

    func status() throws -> AgentReport {
        guard let startsAfter, reregistrations >= startsAfter, registration == .enabled else {
            throw AgentNotRunning(message: "Connection refused")
        }
        return AgentReport(version: "1.0")
    }

    func register() throws -> AgentRegistration {
        if registration == .disabled { registration = .enabled }
        return registration
    }

    /// While set, each registration anew waits for `release()`.
    var holds = false
    private var held: CheckedContinuation<Void, Never>?
    var isHeld: Bool { held != nil }

    func reregister() async throws -> AgentRegistration {
        reregistrations += 1
        if holds { await withCheckedContinuation { held = $0 } }
        registration = afterReregistering
        return registration
    }

    func release() {
        held?.resume()
        held = nil
    }

    func openLoginItems() {}
}

private func scratchDefaults() -> UserDefaults {
    UserDefaults(suiteName: "foldover-tests-\(UUID().uuidString)")!
}

@MainActor
private func agentStore(for item: StaleLoginItem, defaults: UserDefaults = scratchDefaults()) -> AgentStore {
    AgentStore(statusRequest: { try await item.status() }, system: item, defaults: defaults,
               startupTimeout: .milliseconds(50), startupPoll: .milliseconds(5))
}

/// The login item reads as enabled, but launchd cannot start the agent:
/// Turn On registers it anew instead of waiting on it.
@MainActor
@Test
func turnOnRegistersAnewAnAgentLaunchdCannotStart() async {
    let item = StaleLoginItem(startsAfter: 1)
    let store = agentStore(for: item)
    await store.enable()
    #expect(store.connection == .connected(version: "1.0"))
    #expect(item.reregistrations == 1)
}

/// The first registration anew can leave launchd's old job in place.
@MainActor
@Test
func turnOnRegistersAnewASecondTime() async {
    let item = StaleLoginItem(startsAfter: 2)
    let store = agentStore(for: item)
    await store.enable()
    #expect(store.connection == .connected(version: "1.0"))
    #expect(item.reregistrations == 2)
}

@MainActor
@Test
func turnOnGivesUpAfterTwoTriesAndSaysWhatToDo() async {
    let item = StaleLoginItem(startsAfter: nil)
    let store = agentStore(for: item)
    await store.enable()
    #expect(item.reregistrations == 2)
    guard case .failed(let message) = store.connection else {
        Issue.record("Expected a failure, got \(store.connection)")
        return
    }
    #expect(message.contains("Login Items"))
}

/// An ad-hoc-signed agent registered anew may need approval again.
@MainActor
@Test
func registeringAnewCanNeedApproval() async {
    let item = StaleLoginItem(startsAfter: 1)
    item.afterReregistering = .requiresApproval
    let store = agentStore(for: item)
    await store.enable()
    #expect(store.connection == .approvalRequired)
}

/// Turning on an agent that was off registers it once, with no repair.
@MainActor
@Test
func turnOnRegistersAnAgentThatWasOff() async {
    let item = StaleLoginItem(.disabled, startsAfter: 0)
    let store = agentStore(for: item)
    await store.enable()
    #expect(store.connection == .connected(version: "1.0"))
    #expect(item.reregistrations == 0)
}

/// A new build of the agent is registered anew once, at the first launch
/// that finds it.
@MainActor
@Test
func anUpdatedAgentIsRegisteredAnewOnceAtLaunch() async {
    let defaults = scratchDefaults()
    let item = StaleLoginItem(startsAfter: 1)
    let first = agentStore(for: item, defaults: defaults)
    await first.reregisterIfUpdated()
    #expect(first.connection == .connected(version: "1.0"))
    #expect(item.reregistrations == 1)

    await agentStore(for: item, defaults: defaults).reregisterIfUpdated()
    #expect(item.reregistrations == 1)

    item.agentBuild = Data("newer build".utf8)
    await agentStore(for: item, defaults: defaults).reregisterIfUpdated()
    #expect(item.reregistrations == 2)
}

/// A login item that is off, or a build that cannot be read, is left as
/// it is at launch.
@MainActor
@Test
func launchLeavesAnAgentThatIsOffOrUnreadableAlone() async {
    let off = StaleLoginItem(.disabled, startsAfter: 0)
    await agentStore(for: off).reregisterIfUpdated()
    #expect(off.reregistrations == 0)

    let unreadable = StaleLoginItem(startsAfter: 0)
    unreadable.agentBuild = nil
    await agentStore(for: unreadable).reregisterIfUpdated()
    #expect(unreadable.reregistrations == 0)
}

/// A health check while the agent is turned on waits it out, instead of
/// reporting halfway that the agent refuses connections.
@MainActor
@Test
func healthChecksWaitOutTurningOn() async {
    let item = StaleLoginItem(startsAfter: 1)
    item.holds = true
    let store = agentStore(for: item)
    let turningOn = Task { await store.enable() }
    for _ in 0..<10_000 where !item.isHeld { await Task.yield() }
    #expect(item.isHeld)
    await store.refresh()
    #expect(store.connection == .connecting)
    item.release()
    await turningOn.value
    #expect(store.connection == .connected(version: "1.0"))
}
