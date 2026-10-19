import Testing
@testable import Foldover

private enum LoginFixtureError: Error { case refused }

@MainActor
private final class LoginItemFixture: LoginItemControl {
    var status: LoginStatus = .disabled
    var refuses = false

    func enable() throws {
        if refuses { throw LoginFixtureError.refused }
        status = .requiresApproval
    }

    func disable() throws { status = .disabled }
}

@MainActor
@Test
func openAtLoginFollowsTheSystemAndReportsRefusals() {
    let loginItem = LoginItemFixture()
    var opened = 0
    let store = SettingsStore(loginItem: loginItem, showFullDiskAccess: { opened += 1 })
    let viewModel = SettingsViewModel(store: store)
    #expect(!viewModel.opensAtLogin)

    viewModel.setLoginLaunch(.open)
    #expect(viewModel.opensAtLogin)
    #expect(viewModel.loginNote.contains("Login Items"))

    loginItem.status = .enabled
    viewModel.refresh()
    viewModel.setLoginLaunch(.stayClosed)
    #expect(!viewModel.opensAtLogin)

    loginItem.refuses = true
    viewModel.setLoginLaunch(.open)
    #expect(!viewModel.opensAtLogin)
    #expect(viewModel.error != nil)

    viewModel.openFullDiskAccess()
    #expect(opened == 1)
}
