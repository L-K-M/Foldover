import Foundation
import FoldoverCore
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

/// A status call may ignore cancellation, as the UniFFI bridge does. Its
/// deadline must still restore recovery, and a late answer must stay stale.
@MainActor
@Test
func anUnansweredHealthCheckTimesOutAndIgnoresItsLateReply() async throws {
    let client = DeferredStatus()
    let store = AgentStore(statusRequest: { AgentReport(version: try await client.status()) },
                           startupTimeout: .milliseconds(100))
    let checking = Task { await store.refresh() }
    await client.waitUntilStarted()
    try await Task.sleep(for: .milliseconds(150))
    let timedOut = store.connection
    if case .failed(let message) = timedOut {
        #expect(message.contains("not responding"))
    } else {
        Issue.record("Expected a timeout, got \(timedOut)")
    }

    await client.finishFirst()
    await checking.value
    #expect(store.connection == timedOut)
}

@MainActor
@Test
func cancellingTheFirstHealthCheckRestoresTheOffState() async {
    let client = DeferredStatus()
    let store = AgentStore(statusRequest: { AgentReport(version: try await client.status()) })
    let checking = Task { await store.refresh() }
    await client.waitUntilStarted()
    checking.cancel()
    await checking.value
    #expect(store.connection == .disconnected)
    await client.finishFirst()
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
final class StaleLoginItem: AgentRegistering {
    var registration: AgentRegistration
    var appBuild: Data? = Data("new build".utf8)
    /// What registering anew leaves the login item as.
    var afterReregistering: AgentRegistration = .enabled
    /// How many times the agent must be registered anew before launchd
    /// starts it; nil when it never does.
    private let startsAfter: Int?
    private(set) var reregistrations = 0
    private(set) var registrations = 0
    /// Status requests refused before the agent answers, as while it starts.
    var startingRefusals = 0

    init(_ registration: AgentRegistration = .enabled, startsAfter: Int?) {
        self.registration = registration
        self.startsAfter = startsAfter
    }

    func status() throws -> AgentReport {
        if startingRefusals > 0 {
            startingRefusals -= 1
            throw AgentNotRunning(message: "Connection refused")
        }
        guard let startsAfter, reregistrations >= startsAfter, registration == .enabled else {
            throw AgentNotRunning(message: "Connection refused")
        }
        return AgentReport(version: "1.0")
    }

    func register() throws -> AgentRegistration {
        registrations += 1
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

/// Turning the agent on and registering it anew. Each test keeps the
/// registered build in a defaults suite of its own, removed afterwards.
@MainActor
@Suite
final class AgentRegistrationTests {
    private let suiteName = "foldover-tests-\(UUID().uuidString)"
    private let defaults: UserDefaults

    init() {
        defaults = UserDefaults(suiteName: suiteName)!
    }

    deinit {
        UserDefaults.standard.removePersistentDomain(forName: suiteName)
    }

    private func agentStore(for item: StaleLoginItem, startupTimeout: Duration = .milliseconds(50)) -> AgentStore {
        AgentStore(statusRequest: { try await item.status() }, system: item, defaults: defaults,
                   startupTimeout: startupTimeout, startupPoll: .milliseconds(5))
    }

    /// The login item reads as enabled, but launchd cannot start the agent:
    /// Turn On registers it anew instead of waiting on it.
    @Test
    func turnOnRegistersAnewAnAgentLaunchdCannotStart() async {
        let item = StaleLoginItem(startsAfter: 1)
        let store = agentStore(for: item)
        await store.enable()
        #expect(store.connection == .connected(version: "1.0"))
        #expect(item.reregistrations == 1)
    }

    /// The first registration anew can leave launchd's old job in place.
    @Test
    func turnOnRegistersAnewASecondTime() async {
        let item = StaleLoginItem(startsAfter: 2)
        let store = agentStore(for: item)
        await store.enable()
        #expect(store.connection == .connected(version: "1.0"))
        #expect(item.reregistrations == 2)
    }

    /// An old agent can accept the socket but reject the new app's handshake.
    @Test
    func turnOnRepairsAnAgentFromThePreviousBuild() async {
        let item = StaleLoginItem(startsAfter: 1)
        let store = AgentStore(statusRequest: {
            guard await item.reregistrations > 0 else {
                throw AgentError.Connection(message: "Agent version mismatch; restart the background agent")
            }
            return try await item.status()
        }, system: item, defaults: defaults,
           startupTimeout: .milliseconds(100), startupPoll: .milliseconds(5))
        await store.enable()
        #expect(item.reregistrations == 1)
        #expect(store.connection == .connected(version: "1.0"))
    }

    @Test
    func turnOnPreservesRequestErrorsWithoutRestarting() async {
        let item = StaleLoginItem(startsAfter: 0)
        let store = AgentStore(statusRequest: {
            throw AgentError.Request(message: "Cannot read agent state")
        }, system: item, defaults: defaults)
        await store.enable()
        #expect(item.reregistrations == 0)
        #expect(store.connection == .failed(message: "Cannot read agent state"))
    }

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
    @Test
    func registeringAnewCanNeedApproval() async {
        let item = StaleLoginItem(startsAfter: 1)
        item.afterReregistering = .requiresApproval
        let store = agentStore(for: item)
        await store.enable()
        #expect(store.connection == .approvalRequired)
    }

    /// Registering ties the agent to its build; approving it later does not
    /// call for registering it anew at the next launch.
    @Test
    func aBuildAwaitingApprovalIsNotRegisteredAgain() async {
        let item = StaleLoginItem(startsAfter: 1)
        item.afterReregistering = .requiresApproval
        let store = agentStore(for: item)
        await store.reregisterIfUpdated()
        #expect(store.connection == .approvalRequired)

        item.registration = .enabled
        await agentStore(for: item).reregisterIfUpdated()
        #expect(item.reregistrations == 1)
    }

    /// An agent that is still starting, as right after it was turned off and
    /// on in Login Items, is given a moment before it is registered anew.
    @Test
    func turnOnGivesAStartingAgentAMoment() async {
        let item = StaleLoginItem(startsAfter: 0)
        item.startingRefusals = 1
        // Room for the retry however busy the main actor is.
        let store = agentStore(for: item, startupTimeout: .seconds(10))
        await store.enable()
        #expect(store.connection == .connected(version: "1.0"))
        #expect(item.reregistrations == 0)
    }

    /// The probe's deadline covers the request itself, not just the pauses
    /// between refused connections. Turn On must reach its repair.
    @Test
    func turnOnRepairsWhenTheStatusRequestDoesNotAnswer() async throws {
        let client = DeferredStatus()
        let item = StaleLoginItem(startsAfter: 1)
        let store = AgentStore(statusRequest: { AgentReport(version: try await client.status()) },
                               system: item, defaults: defaults,
                               startupTimeout: .milliseconds(100), startupPoll: .milliseconds(5))
        let turningOn = Task { await store.enable() }
        await client.waitUntilStarted()
        let viewModel = MainViewModel(appInfo: AppInfoStore(), agent: store)
        #expect(viewModel.agentStatusText == "Starting the background agent…")
        #expect(viewModel.agentAction == nil)
        #expect(viewModel.setupSteps.first?.action == nil)
        try await Task.sleep(for: .milliseconds(150))
        #expect(item.reregistrations == 1)
        #expect(store.connection == .connected(version: "new"))

        await client.finishFirst()
        await turningOn.value
        #expect(store.connection == .connected(version: "new"))
    }

    /// Turn On, chosen while the launch registers the agent anew, waits for
    /// that to finish rather than change the login item alongside it.
    @Test
    func turnOnWaitsForTheRegistrationAtLaunch() async {
        let item = StaleLoginItem(startsAfter: 1)
        item.holds = true
        let store = agentStore(for: item)
        let launching = Task { await store.reregisterIfUpdated() }
        for _ in 0..<10_000 where !item.isHeld { await Task.yield() }
        guard item.isHeld else {
            Issue.record("The launch never registered the agent anew")
            launching.cancel()
            return
        }
        let turningOn = Task { await store.enable() }
        for _ in 0..<1_000 { await Task.yield() }
        #expect(item.registrations == 0)
        item.release()
        await launching.value
        await turningOn.value
        #expect(store.connection == .connected(version: "1.0"))
        #expect(item.registrations == 1)
        #expect(item.reregistrations == 1)
    }

    /// Turning on an agent that was off registers it once, with no repair.
    @Test
    func turnOnRegistersAnAgentThatWasOff() async {
        let item = StaleLoginItem(.disabled, startsAfter: 0)
        let store = agentStore(for: item)
        await store.enable()
        #expect(store.connection == .connected(version: "1.0"))
        #expect(item.reregistrations == 0)
    }

    /// A new build of the app, or of the agent it embeds, registers the agent
    /// anew once, at the first launch that finds it.
    @Test
    func anUpdatedAppRegistersItsAgentAnewOnceAtLaunch() async {
        let item = StaleLoginItem(startsAfter: 1)
        let first = agentStore(for: item)
        await first.reregisterIfUpdated()
        #expect(first.connection == .connected(version: "1.0"))
        #expect(item.reregistrations == 1)

        await agentStore(for: item).reregisterIfUpdated()
        #expect(item.reregistrations == 1)

        item.appBuild = Data("newer build".utf8)
        await agentStore(for: item).reregisterIfUpdated()
        #expect(item.reregistrations == 2)
    }

    /// A login item that is off, or a build that cannot be read, is left as
    /// it is at launch.
    @Test
    func launchLeavesAnAgentThatIsOffOrUnreadableAlone() async {
        let off = StaleLoginItem(.disabled, startsAfter: 0)
        await agentStore(for: off).reregisterIfUpdated()
        #expect(off.reregistrations == 0)

        let unreadable = StaleLoginItem(startsAfter: 0)
        unreadable.appBuild = nil
        await agentStore(for: unreadable).reregisterIfUpdated()
        #expect(unreadable.reregistrations == 0)
    }

    /// A health check while the agent is turned on waits it out, instead of
    /// reporting halfway that the agent refuses connections.
    @Test
    func healthChecksWaitOutTurningOn() async {
        let item = StaleLoginItem(startsAfter: 1)
        item.holds = true
        let store = agentStore(for: item)
        let turningOn = Task { await store.enable() }
        for _ in 0..<10_000 where !item.isHeld { await Task.yield() }
        guard item.isHeld else {
            Issue.record("Turning on never registered the agent anew")
            return
        }
        await store.refresh()
        let viewModel = MainViewModel(appInfo: AppInfoStore(), agent: store)
        #expect(viewModel.agentStatusText == "Restarting the background agent…")
        #expect(viewModel.agentAction == nil)
        #expect(viewModel.setupSteps.first?.action == nil)
        item.release()
        await turningOn.value
        #expect(store.connection == .connected(version: "1.0"))
    }

    /// A health check that cannot reach an agent its login item says is on
    /// registers it anew; it does not wait for Turn On.
    @Test
    func aRefusedCheckRegistersTheAgentAnew() async {
        let item = StaleLoginItem(startsAfter: 1)
        let store = agentStore(for: item)
        await store.refresh()
        #expect(store.connection == .connected(version: "1.0"))
        #expect(item.reregistrations == 1)
    }

    /// The repair tries the registration anew and one more, then stops:
    /// until an answer arrives, more checks do not register again.
    @Test
    func aRefusedCheckRepairsOnceUntilAnAnswer() async {
        let item = StaleLoginItem(startsAfter: nil)
        let store = agentStore(for: item)
        await store.refresh()
        #expect(item.reregistrations == 2)
        guard case .failed = store.connection else {
            Issue.record("Expected failure, got \(store.connection)")
            return
        }
        await store.refresh()
        #expect(item.reregistrations == 2)
    }

    /// An agent that is simply off is reported, not repaired.
    @Test
    func aCheckDoesNotRepairAnAgentThatIsOff() async {
        let item = StaleLoginItem(.disabled, startsAfter: nil)
        let store = agentStore(for: item)
        await store.refresh()
        #expect(item.reregistrations == 0)
        guard case .failed = store.connection else {
            Issue.record("Expected failure, got \(store.connection)")
            return
        }
    }
}
