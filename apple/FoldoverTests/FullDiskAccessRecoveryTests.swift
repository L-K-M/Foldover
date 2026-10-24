import Foundation
import Testing
@testable import Foldover

@MainActor
private final class LoginItemFake: LoginItemControl {
    var status: LoginStatus = .disabled
    var enableCalls = 0
    var disableCalls = 0
    var enableError: (any Error)?

    func enable() throws {
        enableCalls += 1
        if let e = enableError { throw e }
        status = .enabled
    }

    func disable() throws {
        disableCalls += 1
        status = .disabled
    }
}

private enum ResetError: Error, CustomStringConvertible {
    case failed
    var description: String { "reset failed" }
}

@MainActor
private final class FDATestContext {
    private let suiteName = "foldover-fda-test-\(UUID().uuidString)"
    let defaults: UserDefaults
    let loginItem = LoginItemFake()
    let build = Data([0x01, 0x02, 0x03])
    var resetCalls = 0
    var openCalls = 0
    var resetAction: @MainActor () async throws -> Void = { }
    var events: [String] = []

    init() {
        defaults = UserDefaults(suiteName: suiteName)!
    }

    func cleanUp() { defaults.removePersistentDomain(forName: suiteName) }

    func makeStore(
        build: Data? = Data([0x01, 0x02, 0x03]),
        canReset: @escaping @MainActor () -> Bool = { true }
    ) -> SettingsStore {
        SettingsStore(
            loginItem: loginItem,
            showFullDiskAccess: { self.openCalls += 1; self.events.append("open") },
            resetFullDiskAccess: {
                self.resetCalls += 1
                self.events.append("reset")
                try await self.resetAction()
            },
            appBuild: build,
            defaults: defaults,
            canResetFullDiskAccess: canReset
        )
    }
}

@MainActor
@Suite
final class FullDiskAccessRecoveryTests {

    // First launch shows reminder; dismissal hides it. Same build on
    // relaunch stays dismissed; a different build re-triggers the reminder.
    // None of these paths invoke reset or open Settings implicitly.
    @Test
    func startupReminder_lifecycleAndBuildDetection() {
        let ctx = FDATestContext()
        defer { ctx.cleanUp() }

        let store = ctx.makeStore()
        store.reviewFullDiskAccessAtLaunch()
        #expect(store.fullDiskAccessReminderNeeded == true)
        #expect(ctx.resetCalls == 0)
        #expect(ctx.openCalls == 0)

        store.dismissFullDiskAccessReminder()
        #expect(store.fullDiskAccessReminderNeeded == false)

        let storeSameBuild = ctx.makeStore()
        storeSameBuild.reviewFullDiskAccessAtLaunch()
        #expect(storeSameBuild.fullDiskAccessReminderNeeded == false)

        let storeNewBuild = ctx.makeStore(build: Data([0xFF]))
        storeNewBuild.reviewFullDiskAccessAtLaunch()
        #expect(storeNewBuild.fullDiskAccessReminderNeeded == true)
    }

    // A nil build (e.g. running from Xcode without a codesign) preserves
    // any existing acknowledgement and never calls reset or open.
    @Test
    func nilBuild_preservesAcknowledgementAndNeverActs() {
        let ctx = FDATestContext()
        defer { ctx.cleanUp() }
        ctx.resetAction = { throw ResetError.failed }

        let store = ctx.makeStore(build: nil)
        store.reviewFullDiskAccessAtLaunch()
        #expect(store.fullDiskAccessReminderNeeded == false)
        #expect(ctx.resetCalls == 0)
        #expect(ctx.openCalls == 0)

        // Acknowledge with a real build, then verify nil build doesn't clear it.
        let storeReal = ctx.makeStore()
        storeReal.reviewFullDiskAccessAtLaunch()
        storeReal.dismissFullDiskAccessReminder()

        let storeNil = ctx.makeStore(build: nil)
        storeNil.reviewFullDiskAccessAtLaunch()
        #expect(storeNil.fullDiskAccessReminderNeeded == false)
        #expect(ctx.resetCalls == 0)

        let storeRealAgain = ctx.makeStore()
        storeRealAgain.reviewFullDiskAccessAtLaunch()
        #expect(storeRealAgain.fullDiskAccessReminderNeeded == false)
    }

    // Explicit reset succeeds: the reset closure runs, Settings opens,
    // and the reminder stays visible until the user acknowledges the reminder.
    @Test
    func explicitReset_successOpensSettingsAndKeepsReminder() async {
        let ctx = FDATestContext()
        defer { ctx.cleanUp() }
        let store = ctx.makeStore()

        store.dismissFullDiskAccessReminder()
        await store.resetFullDiskAccessAndOpenSettings()
        #expect(ctx.resetCalls == 1)
        #expect(ctx.openCalls == 1)
        #expect(ctx.events == ["reset", "open"])
        #expect(store.fullDiskAccessReminderNeeded == true)
        #expect(store.fullDiskAccessError == nil)
        let relaunched = ctx.makeStore()
        relaunched.reviewFullDiskAccessAtLaunch()
        #expect(relaunched.fullDiskAccessReminderNeeded)
    }

    // Reset failure exposes an error without opening Settings. A subsequent
    // retry that succeeds clears the error and opens Settings.
    @Test
    func explicitReset_failureThenRetry() async {
        let ctx = FDATestContext()
        defer { ctx.cleanUp() }
        var attempt = 0
        ctx.resetAction = {
            attempt += 1
            if attempt == 1 { throw ResetError.failed }
        }
        let store = ctx.makeStore()

        await store.resetFullDiskAccessAndOpenSettings()
        #expect(ctx.resetCalls == 1)
        #expect(ctx.openCalls == 0)
        #expect(store.fullDiskAccessError != nil)

        await store.resetFullDiskAccessAndOpenSettings()
        #expect(ctx.resetCalls == 2)
        #expect(ctx.openCalls == 1)
        #expect(store.fullDiskAccessError == nil)
    }

    // When the guard closure returns false, reset and open are both
    // prevented, and the saved acknowledgement remains untouched.
    @Test
    func busyGuard_preventsResetAndPreservesAcknowledgement() async {
        let ctx = FDATestContext()
        defer { ctx.cleanUp() }
        ctx.resetAction = { throw ResetError.failed }

        // Acknowledge first with a store that can reset.
        let storeAck = ctx.makeStore()
        storeAck.reviewFullDiskAccessAtLaunch()
        storeAck.dismissFullDiskAccessReminder()
        #expect(storeAck.fullDiskAccessReminderNeeded == false)

        // Now use a store whose guard blocks reset.
        let storeGuarded = ctx.makeStore(canReset: { false })
        #expect(storeGuarded.mayResetFullDiskAccess == false)

        await storeGuarded.resetFullDiskAccessAndOpenSettings()
        #expect(ctx.resetCalls == 0)
        #expect(ctx.openCalls == 0)
        #expect(storeGuarded.fullDiskAccessError != nil)

        // Acknowledgement from the first store should still hold.
        let storeCheck = ctx.makeStore()
        storeCheck.reviewFullDiskAccessAtLaunch()
        #expect(storeCheck.fullDiskAccessReminderNeeded == false)
    }
    @Test
    func duplicateResetIsIgnoredWhileTheFirstIsPending() async {
        let ctx = FDATestContext()
        defer { ctx.cleanUp() }
        let (started, signal) = AsyncStream.makeStream(of: Void.self)
        var resumeReset: CheckedContinuation<Void, Never>?
        ctx.resetAction = {
            signal.yield(())
            await withCheckedContinuation { resumeReset = $0 }
        }
        let store = ctx.makeStore()
        let first = Task { await store.resetFullDiskAccessAndOpenSettings() }
        for await _ in started.prefix(1) { }
        #expect(store.resettingFullDiskAccess)
        #expect(!store.mayResetFullDiskAccess)
        await store.resetFullDiskAccessAndOpenSettings()
        #expect(ctx.resetCalls == 1)
        #expect(ctx.openCalls == 0)
        resumeReset?.resume()
        await first.value
        #expect(!store.resettingFullDiskAccess)
        #expect(ctx.events == ["reset", "open"])
    }

}
