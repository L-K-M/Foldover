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
    let store = SettingsStore(loginItem: loginItem)
    let viewModel = SettingsViewModel(store: store)
    #expect(!viewModel.opensAtLogin)

    viewModel.setOpensAtLogin(true)
    #expect(viewModel.opensAtLogin)
    #expect(viewModel.loginNote.contains("Login Items"))

    loginItem.status = .enabled
    viewModel.refresh()
    viewModel.setOpensAtLogin(false)
    #expect(!viewModel.opensAtLogin)

    loginItem.refuses = true
    viewModel.setOpensAtLogin(true)
    #expect(!viewModel.opensAtLogin)
    #expect(viewModel.error != nil)
}
