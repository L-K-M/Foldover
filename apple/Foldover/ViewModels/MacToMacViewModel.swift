import Foundation

/// Whether other Macs may pair with this one.
enum PairingWindow {
    case open
    case closed
}

/// The Mac-to-Mac settings: strings and actions only.
@MainActor
struct MacToMacViewModel {
    let store: MacToMacStore

    struct CodeRow: Identifiable, Equatable {
        let id: String
        let title: String
        let code: String
        let note: String
    }

    struct NearbyRow: Identifiable, Equatable {
        let id: String
        let name: String
        let detail: String
        let canPair: Bool
    }

    struct PairedRow: Identifiable, Equatable {
        let id: String
        let name: String
        let detail: String
    }

    var isOn: Bool { store.state?.isOn ?? false }

    /// Shown before turning on, so the Local Network prompt that follows
    /// is expected.
    var intro: String {
        String(localized: "Pair your other Macs to sync folders with them over this network, encrypted and without a cloud service. When you turn this on, macOS asks whether Foldover may find devices on your local network; allow it so your Macs can see each other.")
    }

    var thisMac: String? {
        store.state.map { String(localized: "Other Macs see this one as “\($0.name)”.") }
    }

    var pairingOpen: Bool { store.state?.pairingOpen ?? false }

    var pairingNote: String {
        pairingOpen
            ? String(localized: "Other Macs on this network can ask to pair for the next few minutes.")
            : String(localized: "To pair, allow pairing here, then choose this Mac on the other one.")
    }

    var codes: [CodeRow] {
        (store.state?.pending ?? []).map { pending in
            CodeRow(id: pending.device,
                    title: String(localized: "Pairing with “\(pending.name)”"),
                    code: Self.codeText(pending.code),
                    note: String(localized: "Confirm only if “\(pending.name)” shows the same code."))
        }
    }

    var nearby: [NearbyRow] {
        (store.state?.nearby ?? []).map { nearby in
            let detail = if nearby.paired {
                String(localized: "Paired")
            } else if nearby.openForPairing {
                String(localized: "Ready to pair")
            } else {
                String(localized: "Allow pairing on that Mac to pair with it")
            }
            return NearbyRow(id: nearby.device, name: nearby.name, detail: detail,
                             canPair: nearby.openForPairing && !nearby.paired)
        }
    }

    /// Why no other Mac is listed, when none is.
    var nearbyNote: String? {
        guard let state = store.state, state.isOn else { return nil }
        if let problem = state.discoveryProblem { return problem }
        guard state.nearby.isEmpty else { return nil }
        return String(localized: "No other Macs found. Open Foldover’s Mac-to-Mac settings on the other Mac, on the same network.")
    }

    var paired: [PairedRow] {
        (store.state?.paired ?? []).map { paired in
            PairedRow(id: paired.device, name: paired.name,
                      detail: String(localized: "Paired \(paired.paired.formatted(date: .abbreviated, time: .omitted))"))
        }
    }

    var error: String? { store.error }

    /// Six digits in two groups, e.g. "012 345".
    nonisolated static func codeText(_ code: UInt32) -> String {
        let digits = String(format: "%06u", code)
        return "\(digits.prefix(3)) \(digits.suffix(3))"
    }

    func monitor() async { await store.monitor() }
    func turnOn() async { await store.start() }

    func setPairing(_ window: PairingWindow) async {
        switch window {
        case .open: await store.openPairing()
        case .closed: await store.closePairing()
        }
    }

    func pair(_ row: NearbyRow) async { await store.pair(row.id) }
    func confirm(_ row: CodeRow) async { await store.confirm(row.id) }
    func cancel(_ row: CodeRow) async { await store.cancel(row.id) }
    func forget(_ row: PairedRow) async { await store.forget(row.id) }
}
