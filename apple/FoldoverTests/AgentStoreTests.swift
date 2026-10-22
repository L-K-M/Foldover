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
