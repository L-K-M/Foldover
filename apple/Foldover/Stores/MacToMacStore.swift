import FoldoverCore
import Foundation
import Observation

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

struct PairedDevice: Equatable, Sendable, Identifiable {
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
        guard let current = try? await client.state(), generation == request else { return }
        state = current
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
