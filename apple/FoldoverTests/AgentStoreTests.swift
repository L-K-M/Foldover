import Testing
@testable import Foldover

@MainActor
@Test
func agentRefreshReportsTheConnectedVersion() async {
    let store = AgentStore(statusRequest: { "test-version" })
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
    let store = AgentStore(statusRequest: { try await client.status() })
    let older = Task { await store.refresh() }
    await client.waitUntilStarted()
    await store.refresh()
    await client.finishFirst()
    await older.value
    #expect(store.connection == .connected(version: "new"))
}
