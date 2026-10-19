# ADR 0031: Mac-to-Mac sync

Status: Device trust, pairing, discovery, shared-folder grants and the
peer endpoint's reads implemented; its writes pending

## Context

PLAN.md §4.11 and P5: two Macs running Foldover pair once, then sync a
folder on one with a folder on the other over the LAN, encrypted and
pinned, with no cloud. The transport driver already provides pinned QUIC
connections, session-bound pairing codes and live revocation
([ADR 0004](0004-peer-pairing.md)); the agent can restore its identity
from the Keychain. Nothing connected agents to each other.

## Decision

```text
Mac A (owner of the pair)                 Mac B (shares a folder)
app ─ local socket ─ agent A              agent B ─ local socket ─ app
                      │  QUIC + pinned TLS │
                      └──── peer link ─────┘
pair: /Users/a/Photos ⇄ peer://<B>/<share>    share: /Users/b/Photos → A
```

Slices, in order:

1. **Trust and pairing** (implemented). Each agent keeps one device
   identity, the Keychain's on macOS. Paired devices live in the state
   store (schema v16, `device`: ID = certificate fingerprint, name,
   paired time).
2. **Discovery** (implemented). DNS-SD over multicast DNS, service
   `_foldover._udp` (QUIC runs over UDP), advertises each running agent's
   device ID, name and port, plus its pairing port while a window is
   open, so nobody types addresses.
3. **Shared folders and the peer endpoint.** Mac B grants one folder to
   one device (`share`, implemented: schema v17, removed with the device);
   Mac A lists them by asking Mac B, and names one `peer://<device>/<share>`
   as a pair side. Requests carry the share and are refused outside it
   (PLAN.md §5.1 invariant 5). Each request is one QUIC stream: a JSON
   control message, then raw data frames for file contents. Mac B
   answers through its local endpoint, so staging, verification and the
   archive work as on a local root. Mac B then streams change hints.
4. **Fidelity and resume.** Extended attributes, ACLs and flags travel
   with file data; large transfers resume by block.

### Pairing

```text
Mac B: peer.pairingOpen          Mac A: peer.pair(B, address)
  pairing endpoint, any device ◀── QUIC, B's fingerprint checked
  hello{protocol, name}        ◀──▶ hello{protocol, name}
  shows code + "Laptop"             shows code + "Studio"
  peer.confirm(A) ── confirm ──▶ ◀── confirm ── peer.confirm(B)
  pins A, records it                 pins B, records it
```

- The agent's main endpoint accepts only pinned devices. Unpaired Macs
  reach a separate pairing endpoint, open 1 to 60 minutes (10 by default)
  after a person asks for it.
- Both Macs derive the six-digit code from the TLS session. Each sends
  its person's decision; a Mac pins the other only after both confirmed
  within five minutes. A cancel, a timeout or a lost connection pins
  nothing on either side.
- Announced names lose control characters and become "Mac" when empty
  or longer than 256 bytes.
- Revoking forgets the device and closes its live connections.
- Adverts are hints anyone on the network can send: they only say where
  to dial, and the TLS handshake still checks the expected device. An
  advert from another version is ignored; announced names are cleaned as
  above.
- The agent listens at launch only when it has paired devices, otherwise
  once a person turns Mac-to-Mac on (`peer.start`, or any `peer.*` request
  but `peer.status`): a Mac that never pairs never opens a network port,
  and the app can explain the Local Network prompt before it appears.

### Paired requests

Paired Macs talk over the main QUIC endpoint, one bidirectional stream
per request: a JSON `PeerRequest` (`protocol`, `call`) in one
length-prefixed frame, answered by one `PeerReply` frame. Another
version's request, or one this Mac does not know, gets `refused` with a
reason. A Mac reaches a paired one at its advertised address, else one
remembered for it, and reuses the connection; a stale one is redialed
once. Requests are answered on blocking threads by the agent, which
tells each Mac only what is shared with it.

| Call | Reply |
|---|---|
| `shares` | The folders the answering Mac shares with the asking one: `id`, `name` |
| `folder` (`share`, `op`) | One operation on a folder shared with the asking Mac, refused for any other |

A request's stream carries tagged frames: a JSON message, a data chunk,
or the end of the data. Folder operations, each answered by the sharing
Mac's local endpoint at the share root:

| `op` | Reply |
|---|---|
| `probe` | `done` once the folder opens and its first entry reads |
| `identity`, `ensureIdentity` | `identity`: the folder's `.foldover/id`, created by the second |
| `ignoreRules` | `rules` from its `.foldoverignore` |
| `nameMatching` | `names`: how its volume compares case and Unicode |
| `space` | `space`: bytes free |
| `observe` (`scan`: metadata policy, rules, size limit, cutoff) | `progress` every 200 ms while scanning; then entries as binary data frames (path, version, fingerprint); then `observed`: leftovers, oversized and old paths |

Failures come back as `failed` with a kind (missing, access denied,
cancelled, changed, invalid, other), so the asking Mac waits for a
missing folder as it waits for an unmounted disk. The sharing Mac keeps
each folder's last observation in memory and rereads only changed
files. The asking Mac cancels a scan by dropping the request; the next
progress report then fails and the scan stops. Pair roots
`peer://<device>/<share>` need schema v18. Until writes land, a run
reports each change to or from another Mac's folder as not available
yet.

### Local API (protocol 9)

| Method | Params | Result |
|---|---|---|
| `peer.status` | none | This Mac's name; its device ID and address once on (else null); the pairing window; pending codes; paired devices; Macs seen nearby, and `discovery`: why none can be seen, if so. Never turns Mac-to-Mac on |
| `peer.start` | none | Status, after turning Mac-to-Mac on |
| `peer.pairingOpen` | optional `minutes` (1 to 60) | Status |
| `peer.pairingClose` | none | Status |
| `peer.pair` | `device`, optional `address` (`ip:port`; by default the window the network advertises) | The pending pairing: device, name, `code` |
| `peer.confirm`, `peer.cancel` | `device` | Status |
| `peer.revoke` | `device` | Status; also ends what this Mac shared with it |
| `peer.shares` | `device` | The folders that paired Mac shares with this one: `id`, `name` |
| `share.list` | none | The folders this Mac shares: `id`, `device`, `root`, `name`, `created_ms` |
| `share.add` | `device` (paired), `root` (an existing folder), `name` | The new share |
| `share.remove` | `id` | The remaining shares |

When the link cannot start (no Keychain access, no network), `peer.*`
fails with `PEER_UNAVAILABLE` (-32010) and folder sync is unaffected.
`foldover peers` and `foldover shares` expose the same calls.

### App

Settings › Mac-to-Mac reads `peer.status` and, while off, explains the
Local Network prompt next to a Turn On button that calls `peer.start`.
Once on, it polls every second while open: Allow pairing opens a window,
nearby Macs ready to pair offer Pair, each pending code shows in two
groups of three digits with Confirm and Cancel, and paired Macs can be
forgotten after a confirmation.

## Consequences

- Only the Mac that owns a pair reconciles it; the other serves its
  folder. If the owner is offline, the pair waits.
- JSON control messages replace the protobuf frames PLAN.md sketches:
  serde types in `foldover-proto::peer` are versioned like the local
  protocol, debuggable, and leave bulk data in binary frames.
- Agents listen, and advertise, on IPv4 only. Where multicast is blocked,
  a person can still pair by typing the other Mac's address and pairing
  port.
- Discovery runs only while the link does: on Macs with paired devices,
  or after a person used Mac-to-Mac. The agent's mDNS responder shares
  port 5353 with the system's.
- Outside macOS there is no Keychain: the agent makes a new identity at
  each start, so pairings last until it restarts. This serves tests and
  development only.
