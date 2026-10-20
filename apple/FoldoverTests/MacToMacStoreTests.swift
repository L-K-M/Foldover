import Foundation
import Testing
@testable import Foldover

private enum MacToMacFixtureError: Error { case unreachable }

/// Plays both Macs' agents: Studio is nearby, and pairing with it shows a
/// code until this Mac confirms it.
private actor MacToMacFixtureClient: MacToMacClient {
    private var on = false
    private var open = false
    private var studioOpen = true
    private var pending: [PendingCode] = []
    private var paired: [PairedDevice] = []
    private var failure: Error?
    private var shared: [FolderShare] = []
    private(set) var starts = 0

    func state() async throws -> MacToMacState {
        MacToMacState(
            isOn: on, name: "Laptop", pairingOpen: open, pending: pending, paired: paired,
            nearby: on ? [NearbyDevice(device: "studio", name: "Studio", openForPairing: studioOpen,
                                       paired: paired.contains { $0.device == "studio" })] : [],
            discoveryProblem: nil
        )
    }

    func start() async throws -> MacToMacState {
        if let failure { throw failure }
        starts += 1
        on = true
        return try await state()
    }

    func openPairing() async throws -> MacToMacState {
        on = true
        open = true
        return try await state()
    }

    func closePairing() async throws -> MacToMacState {
        open = false
        return try await state()
    }

    func pair(device: String) async throws -> PendingCode {
        if let failure { throw failure }
        let code = PendingCode(device: device, name: "Studio", code: 12_345)
        pending.append(code)
        return code
    }

    func confirm(device: String) async throws -> MacToMacState {
        pending.removeAll { $0.device == device }
        paired.append(PairedDevice(device: device, name: "Studio", paired: Date(timeIntervalSince1970: 0)))
        studioOpen = false
        return try await state()
    }

    func cancel(device: String) async throws -> MacToMacState {
        pending.removeAll { $0.device == device }
        return try await state()
    }

    func forget(device: String) async throws -> MacToMacState {
        paired.removeAll { $0.device == device }
        return try await state()
    }

    func shares() async throws -> [FolderShare] { shared }

    func share(path: Data, name: String, device: String) async throws -> FolderShare {
        if let failure { throw failure }
        let share = FolderShare(id: "share-\(shared.count)", device: device, path: path, name: name)
        shared.append(share)
        return share
    }

    func stopSharing(id: String) async throws -> [FolderShare] {
        shared.removeAll { $0.id == id }
        return shared
    }

    func offered(device: String) async throws -> [OfferedShare] {
        if let failure { throw failure }
        return device == "studio" ? [OfferedShare(id: "photos-id", name: "Photos")] : []
    }

    func rejectRequests() { failure = MacToMacFixtureError.unreachable }
}

@MainActor
@Test
func openingMacToMacExplainsTheNetworkPromptBeforeTurningOn() async {
    let client = MacToMacFixtureClient()
    let store = MacToMacStore(client: client)
    let viewModel = MacToMacViewModel(store: store)

    // Reading the state never turns Mac-to-Mac on.
    await store.refresh()
    #expect(!viewModel.isOn)
    #expect(viewModel.intro.contains("local network"))
    #expect(viewModel.nearby.isEmpty && viewModel.nearbyNote == nil)
    #expect(await client.starts == 0)

    await viewModel.turnOn()
    #expect(viewModel.isOn)
    #expect(await client.starts == 1)
    #expect(viewModel.thisMac == "Other Macs see this one as “Laptop”.")
}

@MainActor
@Test
func pairingShowsTheCodeToCompareAndConfirmingListsThePairedMac() async {
    let store = MacToMacStore(client: MacToMacFixtureClient())
    let viewModel = MacToMacViewModel(store: store)
    await viewModel.turnOn()
    #expect(viewModel.nearby == [
        .init(id: "studio", name: "Studio", detail: "Ready to pair", canPair: true),
    ])

    await viewModel.pair(viewModel.nearby[0])
    #expect(viewModel.codes.map(\.code) == ["012 345"])
    #expect(viewModel.codes[0].title == "Pairing with “Studio”")

    await viewModel.confirm(viewModel.codes[0])
    #expect(viewModel.codes.isEmpty)
    #expect(viewModel.paired.map(\.name) == ["Studio"])
    #expect(viewModel.nearby[0].detail == "Paired")
    #expect(!viewModel.nearby[0].canPair)

    await viewModel.forget(viewModel.paired[0])
    #expect(viewModel.paired.isEmpty)
}

@MainActor
@Test
func pairingWindowsOpenAndCloseFromTheToggle() async {
    let store = MacToMacStore(client: MacToMacFixtureClient())
    let viewModel = MacToMacViewModel(store: store)
    await viewModel.setPairing(.open)
    #expect(viewModel.pairingOpen)
    #expect(viewModel.pairingNote.contains("next few minutes"))
    await viewModel.setPairing(.closed)
    #expect(!viewModel.pairingOpen)
}

@MainActor
@Test
func failedMacToMacRequestsStayVisiblePastRefreshes() async {
    let client = MacToMacFixtureClient()
    let store = MacToMacStore(client: client)
    await client.rejectRequests()
    await store.start()
    #expect(store.error != nil)
    await store.refresh()
    #expect(store.error != nil)
}

@Test
func codesShowSixDigitsInTwoGroups() {
    #expect(MacToMacViewModel.codeText(7) == "000 007")
    #expect(MacToMacViewModel.codeText(987_654) == "987 654")
    #expect(MacToMacViewModel.codeSpeech(12_345) == "0 1 2 3 4 5")
}

@MainActor
@Test
func foldersSharedWithAPairedMacAreListedUnderItAndCanStop() async {
    let store = MacToMacStore(client: MacToMacFixtureClient())
    let viewModel = MacToMacViewModel(store: store)
    await viewModel.turnOn()
    await viewModel.pair(viewModel.nearby[0])
    await viewModel.confirm(viewModel.codes[0])

    await viewModel.share(URL(fileURLWithPath: "/Users/a/Photos"), with: viewModel.paired[0])
    #expect(viewModel.paired[0].shares == [
        .init(id: "share-0", name: "Photos", path: "/Users/a/Photos"),
    ])
    await viewModel.stopSharing(viewModel.paired[0].shares[0])
    #expect(viewModel.paired[0].shares.isEmpty)
}

@MainActor
@Test
func aPairSideCanBeAFolderAPairedMacShares() async {
    let macs = MacToMacStore(client: MacToMacFixtureClient())
    let macToMac = MacToMacViewModel(store: macs)
    await macToMac.turnOn()
    await macToMac.pair(macToMac.nearby[0])
    await macToMac.confirm(macToMac.codes[0])
    let editor = PairEditorViewModel(store: PairStore(), macs: macs)
    #expect(editor.offersOtherMacs)

    let studio = editor.pairedMacs[0]
    let folders = await editor.folders(on: studio)
    #expect(folders.map(\.name) == ["Photos"])
    editor.chooseShared(folders[0], on: studio, side: .right)
    #expect(editor.folderText(.right) == "Studio › Photos")
    #expect(editor.usesOtherMac)
    #expect(editor.metadataNote.contains("between Macs"))
    // Without the folder names fetched, the Mac's name still shows.
    let fresh = MacToMacStore(client: MacToMacFixtureClient())
    #expect(fresh.folderName(for: Data("peer://studio/photos-id".utf8)) == "A folder on another Mac")
    #expect(macs.folderName(for: Data("/Users/a".utf8)) == nil)
}
