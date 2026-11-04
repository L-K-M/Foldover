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
    private var holdsOpen = false
    private var heldOpen: CheckedContinuation<Void, Never>?
    private var holdsConfirm = false
    private var heldConfirm: CheckedContinuation<Void, Never>?
    private(set) var starts = 0
    private(set) var closeCalls = 0
    private(set) var cancelCalls = 0
    private(set) var stateReads = 0
    private(set) var shareReads = 0

    func state() async throws -> MacToMacState {
        stateReads += 1
        return snapshot()
    }

    private func snapshot() -> MacToMacState {
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
        return snapshot()
    }

    func openPairing() async throws -> MacToMacState {
        on = true
        open = true
        let answer = snapshot()
        if holdsOpen {
            holdsOpen = false
            await withCheckedContinuation { heldOpen = $0 }
        }
        return answer
    }

    func closePairing() async throws -> MacToMacState {
        closeCalls += 1
        open = false
        return snapshot()
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
        let answer = snapshot()
        if holdsConfirm {
            holdsConfirm = false
            await withCheckedContinuation { heldConfirm = $0 }
        }
        return answer
    }

    func cancel(device: String) async throws -> MacToMacState {
        cancelCalls += 1
        pending.removeAll { $0.device == device }
        paired.removeAll { $0.device == device }
        return snapshot()
    }

    func forget(device: String) async throws -> MacToMacState {
        paired.removeAll { $0.device == device }
        return snapshot()
    }

    func shares() async throws -> [FolderShare] {
        shareReads += 1
        return shared
    }

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

    func holdOpenPairing() { holdsOpen = true }

    func waitForHeldOpen() async -> Bool {
        let deadline = ContinuousClock.now + .seconds(3)
        while heldOpen == nil {
            if ContinuousClock.now >= deadline { return false }
            await Task.yield()
        }
        return true
    }

    func releaseOpenPairing() {
        heldOpen?.resume()
        heldOpen = nil
    }

    func holdConfirm() { holdsConfirm = true }

    func waitForHeldConfirm() async -> Bool {
        let deadline = ContinuousClock.now + .seconds(3)
        while heldConfirm == nil {
            if ContinuousClock.now >= deadline { return false }
            await Task.yield()
        }
        return true
    }

    func releaseConfirm() {
        heldConfirm?.resume()
        heldConfirm = nil
    }
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
func pairingWindowChoicesRunInInvocationOrder() async {
    let client = MacToMacFixtureClient()
    let store = MacToMacStore(client: client)
    await client.holdOpenPairing()
    let open = Task { await store.openPairing() }
    #expect(await client.waitForHeldOpen())

    let close = Task { await store.closePairing() }
    for _ in 0..<10_000 where store.pendingMutations < 2 { await Task.yield() }
    #expect(store.pendingMutations == 2)
    #expect(store.mutationPending)
    #expect(await client.closeCalls == 0)

    let stateReads = await client.stateReads
    let shareReads = await client.shareReads
    await store.refresh()
    #expect(await client.stateReads == stateReads)
    #expect(await client.shareReads == shareReads)

    await client.releaseOpenPairing()
    await open.value
    await close.value
    #expect(await client.closeCalls == 1)
    #expect(store.state?.pairingOpen == false)
    #expect(!store.mutationPending)
}

@MainActor
@Test
func pairingDecisionsRunInInvocationOrder() async {
    let client = MacToMacFixtureClient()
    let store = MacToMacStore(client: client)
    await store.pair("studio")
    await client.holdConfirm()
    let confirm = Task { await store.confirm("studio") }
    #expect(await client.waitForHeldConfirm())

    let cancel = Task { await store.cancel("studio") }
    for _ in 0..<10_000 where store.pendingMutations < 2 { await Task.yield() }
    #expect(store.pendingMutations == 2)
    #expect(store.mutationPending)
    #expect(await client.cancelCalls == 0)

    await client.releaseConfirm()
    await confirm.value
    await cancel.value
    #expect(await client.cancelCalls == 1)
    #expect(store.state?.pending.isEmpty == true)
    #expect(store.state?.paired.isEmpty == true)
    #expect(!store.mutationPending)
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
