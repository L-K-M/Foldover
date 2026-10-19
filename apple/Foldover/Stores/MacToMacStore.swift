import FoldoverCore
import Foundation
import Observation

/// The locator prefix `PairRoot` uses for a folder a paired Mac shares.
let peerScheme = "peer://"

/// Mac-to-Mac on this Mac (ADR 0031), as last reported by the agent.
struct MacToMacState: Equatable, Sendable {
    /// Whether the agent listens and looks for Macs; off until a person
    /// turns it on or pairs a Mac.
    let isOn: Bool
    /// This Mac's name, as other Macs see it.
    let name: String
    let pairingOpen: Bool
    /// Codes waiting for a person on this Mac to compare.
    let pending: [PendingCode]
    let paired: [PairedDevice]
    let nearby: [NearbyDevice]
    /// Why Macs on the network cannot be seen, if they cannot.
    let discoveryProblem: String?
}

struct PendingCode: Equatable, Sendable, Identifiable {
    let device: String
    let name: String
    let code: UInt32
    var id: String { device }
}

struct PairedDevice: Hashable, Sendable, Identifiable {
    let device: String
    let name: String
    let paired: Date
    var id: String { device }
}

struct NearbyDevice: Equatable, Sendable, Identifiable {
    let device: String
    let name: String
    let openForPairing: Bool
    let paired: Bool
    var id: String { device }
}

/// A folder this Mac shares with one paired Mac.
struct FolderShare: Equatable, Sendable, Identifiable {
    let id: String
    let device: String
    let path: Data
    /// What that Mac shows, e.g. "Photos".
    let name: String
}

/// A folder a paired Mac shares with this one.
struct OfferedShare: Equatable, Sendable, Identifiable {
    let id: String
    let name: String
}

protocol MacToMacClient: Sendable {
    /// Never turns Mac-to-Mac on, so it never shows the Local Network prompt.
    func state() async throws -> MacToMacState
    func start() async throws -> MacToMacState
    func openPairing() async throws -> MacToMacState
    func closePairing() async throws -> MacToMacState
    func pair(device: String) async throws -> PendingCode
    func confirm(device: String) async throws -> MacToMacState
    func cancel(device: String) async throws -> MacToMacState
    func forget(device: String) async throws -> MacToMacState
    func shares() async throws -> [FolderShare]
    func share(path: Data, name: String, device: String) async throws -> FolderShare
    func stopSharing(id: String) async throws -> [FolderShare]
    /// Asks that Mac.
    func offered(device: String) async throws -> [OfferedShare]
}

// Keep generated async values inside one actor; only native Sendable records leave it.
private actor CoreMacToMacClient: MacToMacClient {
    func state() async throws -> MacToMacState { state(try await FoldoverCore.macToMacStatus()) }
    func start() async throws -> MacToMacState { state(try await FoldoverCore.startMacToMac()) }

    func openPairing() async throws -> MacToMacState {
        state(try await FoldoverCore.openMacPairing(minutes: nil))
    }

    func closePairing() async throws -> MacToMacState { state(try await FoldoverCore.closeMacPairing()) }

    func pair(device: String) async throws -> PendingCode {
        code(try await FoldoverCore.pairWithMac(device: device))
    }

    func confirm(device: String) async throws -> MacToMacState {
        state(try await FoldoverCore.confirmMacPairing(device: device))
    }

    func cancel(device: String) async throws -> MacToMacState {
        state(try await FoldoverCore.cancelMacPairing(device: device))
    }

    func forget(device: String) async throws -> MacToMacState {
        state(try await FoldoverCore.forgetMac(device: device))
    }

    func shares() async throws -> [FolderShare] { try await FoldoverCore.sharedFolders().map(share) }

    func share(path: Data, name: String, device: String) async throws -> FolderShare {
        share(try await FoldoverCore.shareFolder(device: device, path: path, name: name))
    }

    func stopSharing(id: String) async throws -> [FolderShare] {
        try await FoldoverCore.stopSharing(id: id).map(share)
    }

    func offered(device: String) async throws -> [OfferedShare] {
        try await FoldoverCore.offeredFolders(device: device).map { OfferedShare(id: $0.id, name: $0.name) }
    }

    private func share(_ share: FoldoverCore.SharedFolder) -> FolderShare {
        FolderShare(id: share.id, device: share.device, path: share.path, name: share.name)
    }

    private func state(_ state: FoldoverCore.MacToMac) -> MacToMacState {
        MacToMacState(
            isOn: state.device != nil,
            name: state.name,
            pairingOpen: state.pairingOpen,
            pending: state.pending.map(code),
            paired: state.paired.map {
                PairedDevice(device: $0.device, name: $0.name,
                             paired: Date(timeIntervalSince1970: Double($0.pairedMs) / 1000))
            },
            nearby: state.nearby.map {
                NearbyDevice(device: $0.device, name: $0.name, openForPairing: $0.openForPairing,
                             paired: $0.paired)
            },
            discoveryProblem: state.discoveryProblem
        )
    }

    private func code(_ code: FoldoverCore.PairingCode) -> PendingCode {
        PendingCode(device: code.device, name: code.name, code: code.code)
    }
}

/// Pairing with other Macs. Codes arrive from the other Mac at any time,
/// so the Mac-to-Mac view polls while it is open.
@Observable
@MainActor
final class MacToMacStore {
    private let client: any MacToMacClient
    private let pollInterval: Duration
    private var generation = 0
    private(set) var state: MacToMacState?
    /// Folders this Mac shares; they need no network.
    private(set) var shares: [FolderShare] = []
    /// Names of folders paired Macs offered, by `device/share`, for showing
    /// `peer://` roots.
    private(set) var offeredNames: [String: String] = [:]
    private(set) var error: String?

    init(client: any MacToMacClient = CoreMacToMacClient(), pollInterval: Duration = .seconds(1)) {
        self.client = client
        self.pollInterval = pollInterval
    }

    /// A status read is informational: it keeps the last state on failure
    /// and never touches `error`, so a failed action stays reported until
    /// the next one answers. A reply only applies when no action
    /// overlapped the read.
    func refresh() async {
        generation += 1
        let request = generation
        async let current = try? client.state()
        async let shared = try? client.shares()
        let (state, shares) = await (current, shared)
        guard generation == request else { return }
        if let state { self.state = state }
        if let shares { self.shares = shares }
    }

    /// Refresh until the calling task is cancelled, e.g. when the view
    /// that started it closes.
    func monitor() async {
        while !Task.isCancelled {
            await refresh()
            try? await Task.sleep(for: pollInterval)
        }
    }

    func start() async { await perform { try await $0.start() } }
    func openPairing() async { await perform { try await $0.openPairing() } }
    func closePairing() async { await perform { try await $0.closePairing() } }
    func confirm(_ device: String) async { await perform { try await $0.confirm(device: device) } }
    func cancel(_ device: String) async { await perform { try await $0.cancel(device: device) } }
    func forget(_ device: String) async { await perform { try await $0.forget(device: device) } }

    /// Start pairing; the code then shows among the pending ones.
    func pair(_ device: String) async {
        await perform { client in
            _ = try await client.pair(device: device)
            return try await client.state()
        }
    }

    /// Share the folder at `url` with `device`, named after the folder.
    func share(_ url: URL, with device: String) async {
        guard let path = PairStore.folderBytes(url) else {
            error = String(localized: "Choose a folder on this Mac.")
            return
        }
        let name = url.lastPathComponent
        await performShares { client in
            _ = try await client.share(path: path, name: name, device: device)
            return try await client.shares()
        }
    }

    func stopSharing(_ id: String) async {
        await performShares { try await $0.stopSharing(id: id) }
    }

    /// The folders `device` shares with this Mac, or none when it cannot
    /// be asked; the reason shows as the error.
    func offered(by device: String) async -> [OfferedShare] {
        error = nil
        do {
            let offered = try await client.offered(device: device)
            for share in offered {
                offeredNames["\(device)/\(share.id)"] = share.name
            }
            return offered
        } catch {
            self.error = PairStore.message(for: error)
            return []
        }
    }

    /// How a pair root on a paired Mac reads, e.g. "Studio › Photos", or
    /// nil for any other root.
    func folderName(for root: Data) -> String? {
        guard let text = String(data: root, encoding: .utf8), text.hasPrefix(peerScheme) else {
            return nil
        }
        let key = String(text.dropFirst(peerScheme.count))
        let device = String(key.prefix { $0 != "/" })
        let mac = state?.paired.first { $0.device == device }?.name
        switch (mac, offeredNames[key]) {
        case let (mac?, folder?): return "\(mac) › \(folder)"
        case let (mac?, nil): return String(localized: "A folder on \(mac)")
        default: return String(localized: "A folder on another Mac")
        }
    }

    private func performShares(_ request: @Sendable (any MacToMacClient) async throws -> [FolderShare]) async {
        generation += 1
        defer { generation += 1 }
        error = nil
        do {
            shares = try await request(client)
        } catch {
            self.error = PairStore.message(for: error)
        }
    }

    private func perform(_ request: @Sendable (any MacToMacClient) async throws -> MacToMacState) async {
        generation += 1
        // Completing an action invalidates reads issued while it was on
        // the wire, so they cannot overwrite this reply.
        defer { generation += 1 }
        error = nil
        do {
            state = try await request(client)
        } catch {
            self.error = PairStore.message(for: error)
        }
    }
}
