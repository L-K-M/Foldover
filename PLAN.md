# Foldover — Plan

Status: P1 implementation, 2026-10-17. Evidence: [docs/research](docs/research/README.md)
(competitor review: [competitors.md](docs/research/competitors.md)).

## 1. Summary

**Foldover syncs folders between your Mac's disks, NAS shares, SFTP servers
and other Macs, and shows exactly what it will do before it does it. No cloud,
no account, no subscription.**

A portable Rust engine does the work. Each platform gets a native UI: SwiftUI
and AppKit now, GTK4/libadwaita on Linux later.

| Decision | Choice | Why |
|---|---|---|
| Engine | Rust Cargo workspace | Portable, fast, memory-safe; Dropbox rewrote its sync engine in Rust |
| Mac UI | SwiftUI shell; AppKit `NSOutlineView` for the diff tree | SwiftUI `List` stalls at 100k rows |
| Process model | Engine runs only in a background agent (LaunchAgent; systemd user service on Linux). All UIs are clients | Sync keeps running with the window closed; exactly one process writes the state |
| UI ↔ engine | Rust client library over a local socket, exposed to Swift via UniFFI | One typed protocol, reused by the CLI and the Linux UI |
| Sync model | Three-way reconciliation against a stored last-synced base | The only reliable way to tell "deleted here" from "created there" |
| NAS shares | SMB and NFS through OS mounts that Foldover manages | Pure-Rust SMB clients are 0.x; revisit after 1.0 |
| Mac-to-Mac | Built-in peer mode: Bonjour discovery, code pairing, encrypted QUIC | Top unmet need; competitors charge extra or need an account |
| Distribution | Ad-hoc-signed downloads, explicit `xattr` activation, update checks against GitHub releases; no Mac App Store | Owner decision: no Developer ID or notarization |
| Minimum OS | macOS 27, Apple silicon only | Owner decision; no Intel build |
| Price | Free and open-source, no sponsorships or paid features | Owner decision; public-domain code |

## 2. Why Foldover can win

Details in [competitors.md](docs/research/competitors.md).

- **GoodSync** has the deepest two-way semantics, trigger set and protocol
  coverage. But its UI is non-native and jargon-heavy, it requires a
  subscription and an account, and it keeps its sync database inside every
  synced folder (`_gsdata_`).
- **ChronoSync** has the best Mac metadata fidelity, safe first-run rules,
  archiving and readiness checks. But its document-per-task UI is dated, its
  operation names confuse, and the good Mac-to-Mac path costs extra per Mac.
- **SyncMate** has the friendliest discovery: other Macs and drives just
  appear. But its engine has no persistent state, rescans everything, and has
  a poor reliability record.
- **Open gaps** across 17 surveyed tools: continuous sync *with* a reviewable
  plan; native Mac-to-Mac sync without an account or web console; a conflict
  queue instead of `*.sync-conflict` litter; a permissively licensed engine.

## 3. Product

### 3.1 Who it is for

| Persona | Job to be done |
|---|---|
| Photographer with a NAS (iMac, MacBook, Synology) | "Get new shoots onto the NAS and edits onto both Macs, with zero risk to the library; show me deletions before they happen." |
| Two-Mac household (laptop + desktop) | "Keep Documents and Projects identical over the LAN, continuously, with no cloud and no setup." |
| Developer / admin (SFTP box, external SSD, Linux PC) | "Back up to my SFTP server with preview, filters and logs; mirror the SSD when I plug it in; script it." |

### 3.2 Principles

1. **The preview is the product.** Every pair carries a live plain-language
   plan; ordinary scheduled syncs execute the fresh preview without stopping.
   Pairs may opt in to `--review required` to review every run.
2. **Never lose anything quietly.** Replaced and deleted files are archived;
   conflicts wait in a queue; mass deletions stop for confirmation.
3. **Speak plainly.** Three modes, named for their effect. No "Blind Backup"
   or "Mirror vs Backup" riddles.
4. **Quiet, glanceable trust.** Silent success, loud failure; one menu-bar
   line answers "are we good?" (the Time Machine contract).
5. **A real Mac app.** Native controls, keyboard-complete, VoiceOver-clean,
   Quick Look. English-only.
6. **Fast you can see.** Results stream in as the scan runs; live counters;
   honest throughput and ETA.
7. **Honest about limits.** Every target declares what it can store (tags,
   ACLs, resource forks) and how often its changes are checked; Foldover
   warns *before* the first sync.

### 3.3 Sync modes

| Mode | Effect (shown to the user) |
|---|---|
| **Sync** (two-way) | "Changes flow both ways. A file edited on both sides is flagged for you, never silently picked." |
| **Mirror** (one-way, exact) | "The target is made identical to the source. Anything removed or replaced on the target, including files Foldover never synced, is archived for 30 days." |
| **Backup** (one-way, additive) | "New and changed files are copied to the target. Nothing there is ever deleted." |

### 3.4 Endpoints in 1.0

| Endpoint | How | Notes |
|---|---|---|
| Local or external volume | Direct file I/O | Identified by volume UUID, not name; APFS clones for same-volume copies |
| SMB share | OS mount, managed by Foldover | Credentials in Keychain; auto-mount, reconnect, resume; other computers' changes found by polling (§4.8) |
| NFS share | OS mount | Data and POSIX permissions; xattrs mostly unavailable, which the UI warns about |
| SFTP server | Built-in client (russh) | Honors `~/.ssh/config` and the SSH agent; host-key pinning |
| Another Mac running Foldover | Peer protocol (§4.11) | Encrypted, paired, real-time changes both ways |

Several Macs can share one NAS folder: each Mac pairs with the same share,
which acts as the hub. AFP is not supported: macOS 27 removed the AFP client.

### 3.5 Triggers

Manual; file change (FSEvents, debounced 10–20 s, forced after 5 min of
churn); target appears (drive mounted, share reachable, peer visible);
interval; calendar schedule. Paused on expensive or constrained networks, and
in Low Power Mode unless the user opts in. The agent holds a power assertion
during runs.

### 3.6 Safety defaults

- **Require approval before syncing** is a per-pair setting, off by default
  (`--review automatic`). Ordinary manual and scheduled syncs, including the
  initial backup, execute the fresh observed plan. Turning it on
  (`--review required`) requires approval on every run. Explicit approvals
  remain bound to the exact reviewed plan; changes require a new review.
- **Stop unusually large removals** is a separate per-pair setting, on by
  default (`--removal-protection enabled`). It stops numeric mass removals and
  propagation from a suddenly empty folder for review. Turning it off disables
  these two review stops; root identity, canary, space, lease, revision and
  per-file freshness checks remain enforced.
- **First run:** Sync and Backup have no prior base from which to infer source
  deletions. Mirror may archive target extras under the configured policies.
- **Archive:** replaced and deleted files go to the endpoint's
  `.foldover/archive/` (30-day retention, browsable, "Put back").
- **Checks before destructive steps:**
  - the root is present and its volume identity is unchanged;
  - the optional canary file is unchanged;
  - there is enough free space;
  - with removal protection enabled, a suddenly empty folder cannot empty
    its partner without review, and removing more than 50 paths **and** 1% of
    existing target paths needs confirmation. Unattended runs that would remove
    more than 50% stop, including small pairs.
- **Readiness warnings at pair creation:**
  - a root inside a cloud-managed folder (iCloud Drive, Desktop & Documents,
    a File Provider domain) gets a warning that another sync engine manages
    it;
  - roots that overlap another pair's root are blocked;
  - a root inside the agent's state directory is blocked, but a root that
    holds it — such as the home folder — is allowed: the state subtree is
    left out of scans, refused to reads, writes and peers, and its own
    changes never start a run;
  - a target where `.foldover/` can't be created can only be a Backup
    target, and only after the user accepts that files replaced there are
    not archived.
- **During runs:** a file changed since analysis is re-queued, never
  overwritten.
- **Conflicts** never auto-resolve by default. "Keep both" names the copy
  `name (from MacBook, Oct 16).ext`.

### 3.7 Filters

Per-pair rules, gitignore-style: name and path patterns, plus size and
modification-date terms. Exclusions win. A `.foldoverignore` file in a root
is honored. Built-in exclusions, which aren't user filters and can't be
switched off: `.foldover/` and Foldover's temp files. `.DS_Store` is skipped
by default. New home-folder Backups offer editable cache exclusions, with
additional exclusions chosen explicitly. Existing pairs are unchanged.
The same rules apply to both folders; excluded entries are forgotten
instead of deleted, and existing target copies stay untouched
([ADR 0024](docs/adr/0024-filters.md)).

### 3.8 UI surfaces

One main window (sidebar of pairs, devices and history → pair view with its
live plan sentence → inspector), a menu-bar extra, and the streamed Preview
below. Full wireframes: [mac-ux.md §7](docs/research/sources/mac-ux.md).

```
┌ Preview: Photos ⇄ MyNAS ─ scanning… 48,211 items ────────────────────────┐
│ [New 1,204] [Changed 312] [Deleted 48] [Conflicts 3]   🔍  ☐ unchanged   │
│ →  2026/Trip/day2/IMG_2041.RAW     41 MB   copy to MyNAS        [▾]      │
│ ←  2026/Trip/notes.md              12 KB   copy to this Mac     [▾]      │
│ ⚠  2026/Trip/header.psd            96 MB   edited on both sides [Resolve]│
│ ⌫  2024/drafts/old-banner.psd      66 MB   archive on MyNAS     [▾]      │
│ 1,516 to copy (38.4 GB) · 48 to archive · 3 conflicts · ~6 min  [Sync]   │
└──────────────────────────────────────────────────────────────────────────┘
   [▾] = per-item override: copy the other way, skip once, always skip
```

Signature moments:
- Drag two folders in to create a pair.
- NAS shares and Macs appear by themselves (Bonjour).
- The plan sentence rewrites itself as you edit settings.
- A completion card offers "Review 3 conflicts · Undo". Undo reverses the
  journaled run: created files go to the archive; deleted and overwritten
  files come back from it. Anything it can't reverse is listed.

System integration in 1.0: `MenuBarExtra`, notifications for failures and
conflicts only, App Intents (Run, Pause, Preview; they run in the app, with
no extension), Open at Login. Optional behind a flag: Finder Sync badges and
menu, which are unreliable on recent macOS, so status never depends on them.

### 3.9 Not in 1.0

- Cloud services (Dropbox, Proton Drive, Filen).
- The Linux UI, and Windows. The layering leaves room for Windows drivers.
- More than two parties per folder without a hub; WAN peers.
- An in-app SMB client.
- Delta transfer of modified files.
- Selective (partial) sync.
- Email and webhook alerts; pre/post scripts.
- Bootable clones; iOS; Mac App Store; logged-out sync on macOS.

## 4. Architecture

Terms:
- **Base:** the last-synced state of a pair.
- **Re-baseline:** rebuild snapshots and base without copying anything.
- **Hub:** a shared endpoint that several pairs sync with.
- **SFM mapping:** macOS's encoding of NTFS-illegal characters as U+F0xx on
  SMB.

### 4.1 Layers

Each layer calls only the layer directly below it. Raw I/O, OS APIs, sockets
and SQLite live only in the driver layer; everything above it works with
domain objects (pair, endpoint, plan, conflict). Calls back up, such as scan
sinks, event streams and peer requests, use traits defined by the lower layer
and implemented by the upper one.

```
 Foldover.app (Swift)             foldover CLI       Linux app (GTK4, later)
 ┌───────────────────────────┐  ┌──────────────┐   ┌────────────────────────┐
 │ Views (SwiftUI, AppKit)   │  │ commands     │   │ widgets (gtk4-rs)      │
 │ View models (@Observable) │  │              │   │ view models            │
 │ Stores (Swift domain)     │  │              │   │                        │
 │ FoldoverCore (UniFFI)     │  │              │   │                        │
 └─────────────┬─────────────┘  └──────┬───────┘   └───────────┬────────────┘
               └──── foldover-client → foldover-ipc → net (Unix socket) ┘
                                       │ JSON-RPC
 ┌─────────────────────────────────────▼────────────────────────────────────┐
 │ foldoverd — background agent, the only engine process                    │
 │   ipc (via net) ⇄ service (API façade, sessions, event bus)              │
 │                     ▼                                                    │
 │                   engine (scan, reconcile, plan, gates, execute,         │
 │                           journal, archive, conflicts, scheduler)        │
 │                     ▼                                                    │
 │        endpoint (trait + backends)    state (store)    host (env)        │
 │                     ▼                     ▼               ▼              │
 │ drivers:   fs · watch · net              db              os              │
 └──────────────────────────────────────────────────────────────────────────┘
               macOS: APFS, FSEvents, DiskArbitration, NetFS, Keychain
               Linux: ext4/btrfs, inotify, udev, GVfs/CIFS, Secret Service
```

### 4.2 Process model

The agent/client path supports health and local pair configuration. Lifecycle
gates are recorded in [ADR 0008](docs/adr/0008-agent-health-path.md); configuration
scope is in [ADR 0011](docs/adr/0011-pair-configuration-api.md).

- `foldoverd` ships inside the app bundle and is registered with
  `SMAppService.agent` (plist in `Contents/Library/LaunchAgents`,
  `BundleProgram` key; `RunAtLoad`; restart on crash). On Linux it is a
  systemd user service.
- It holds an exclusive lock on its state directory. A second instance exits.
  One writer by construction: no hand-over between app and agent.
- The agent detects volume mounts itself (DiskArbitration), so drive and
  share triggers work with every window closed.
- **Lifecycle:**
  - App quit → sync continues.
  - Agent crash → launchd restarts it; journal recovery runs before any new
    work.
  - App update → the version handshake detects the mismatch and the agent
    restarts onto the new binary.
  - Downgrade → the state DB records a minimum reader version. An older agent
    refuses to open it and says why. The DB is backed up before every
    migration.
  - Crash loop (N crashes in a few minutes) → safe mode: no syncing, but
    `daemon.status` reports the error and a notification appears.
  - Login item disabled → the app shows "background sync is off" with a
    one-click fix.
- Not in 1.0: sync while logged out (needs a root daemon; Linux gets it via
  `loginctl enable-linger`).

### 4.3 IPC

- JSON-RPC 2.0, newline-delimited, over a Unix domain socket owned by the
  `net` driver.
  - **macOS:** S7 selects a private per-user directory and a short socket
    name within the 104-byte limit. Ad-hoc distribution cannot assume an
    entitled App Group container. The agent fails loudly if no safe path is
    available; S7 evaluates a launchd Mach service for later extensions.
  - **Linux:** `$XDG_RUNTIME_DIR/foldover/`.
- Socket mode `0600`. The agent checks the peer's user ID and, on macOS,
  the client's code signature via its audit token.
  - Every privileged method and event subscription must authenticate the
    installed app or bundled CLI, because plans, history and events expose
    paths. S7 must validate exact ad-hoc code identities against the
    installed bundle, bound to the connecting process's audit token. A
    bundle identifier or executable path alone is insufficient; missing
    identity evidence fails closed. Developer ID team trust is unavailable.
    The measured primitive and remaining gates are in
    [ADR 0006](docs/adr/0006-ad-hoc-client-identity.md).
  - Only `session.hello` and `daemon.status` stay user-gated.
  - Clients also verify the installed agent before sending private parameters.
    The transport pins verified bundle/code identities for its process lifetime
    and rejects installation changes until restart; see ADR 0006.
  - On Linux there is no code signing, so only the user ID is checked.
- Handshake `session.hello` negotiates the protocol version.
  - **Method groups:** `pair.*`, `run.*`, `preview.*` (paged plan rows),
    `conflict.*`, `endpoint.*` (browse, probe), `archive.*`, `history.*`,
    `device.*`, `daemon.*`.
  - **Server events:** `scanProgress`, `planReady`, `opDone`, `conflict`,
    `runFinished`, `endpointUp/Down`.
- `service` exposes `handle(request) → response` plus an event stream, so all
  dispatch logic is unit-testable there. `foldoverd` only connects it to
  `ipc::serve`.
- File data never crosses this socket; it moves endpoint to endpoint inside
  the agent.

### 4.4 Rust workspace

**Vocabulary crates** hold data only and may be used anywhere:
- `foldover-types`: domain values, enums, errors.
- `foldover-proto`: local API messages and peer wire messages, as separate
  modules.

**Rules:**
- Every other crate depends only on the layer directly below it. `cargo-deny`
  and a dependency-edge lint in CI enforce this.
- Transport is its own thin stack on the drivers: `client → ipc → net`. In
  the agent, `foldoverd` hands `ipc::serve` a closure over `service.handle`,
  so neither `ipc` nor `service` depends on the other.
- Items are private by default; only `-proto`, `-client` and `-ffi` expose a
  wide public API.
- Parameters use enums, never booleans (`Direction`, `SyncMode`,
  `DeletePolicy`, `ConflictPolicy`, `ScanDepth`).

| Crate | Layer | Responsibility | Depends on |
|---|---|---|---|
| `foldover-fs` | driver | Local file I/O relative to an open root handle: enumerate with attributes, atomic write (temp → `F_FULLFSYNC` → rename → dir fsync), clone, hard link, xattr/ACL/flags/FinderInfo, AppleDouble, sparse, dataless detection | OS |
| `foldover-watch` | driver | FSEvents streams with stored event IDs; inotify; polling; volume mount events (DiskArbitration, udev) | OS |
| `foldover-net` | driver | Unix sockets with peer-uid check; SFTP (russh, russh-sftp); QUIC (quinn, rustls); mDNS (mdns-sd); share mounting (NetFS, GVfs) | OS |
| `foldover-db` | driver | SQLite (rusqlite, bundled, WAL), migrations, backups | — |
| `foldover-os` | driver | Power assertions, network path (expensive/constrained), Keychain / Secret Service (keyring) | OS |
| `foldover-endpoint` | domain services | `Endpoint` trait, capabilities, credentials, `.foldover/` management, backends `local`, `share`, `sftp`, `peer` (dialer and listener) | fs, watch, net, os |
| `foldover-state` | domain services | Typed store: pairs, base, snapshots, journal, runs, conflicts, devices | db |
| `foldover-host` | domain services | Environment policy: battery, Low Power Mode, metered network, power assertions | os |
| `foldover-engine` | engine | Scan, reconcile, plan, gates, execute, recover, archive, conflicts, scheduler; implements the peer listener's request handler | endpoint, state, host |
| `foldover-service` | service | Public API: sessions, `handle`, validation, event bus | engine |
| `foldover-ipc` | transport (on drivers) | Framing, JSON-RPC, `serve` (takes a handler closure) / `connect` | net |
| `foldoverd` | binary | Composition root: connects `service` to `ipc::serve` | service, ipc |
| `foldover-client` | client | Typed async client: requests, subscriptions, reconnect | ipc |
| `foldover-ffi` | client | UniFFI export of the client for Swift (macOS only) | client |
| `foldover` (CLI) | client | `pairs`, `preview`, `sync`, `conflicts`, `status`; exit codes separate conflicts from errors | client |
| `foldover-sim` | test only | In-memory quirk endpoint, deterministic executor, fault injection | endpoint |

### 4.5 Swift app

Native pair creation and configuration editing use this stack; streamed preview,
execution and the remaining surfaces are pending.

`Views → View models → Stores → FoldoverCore`. Stores (`PairStore`,
`ActivityStore`, `ConflictStore`, …) own the client session and turn API
messages into view state. A sibling `System` module beside FoldoverCore wraps
`SMAppService`, notifications, update checks and permission probes. Views never call
FoldoverCore or `System` directly. UniFFI's Swift 6 support is partial (async
code is not fully `Sendable`), so all callbacks enter through one actor, and
the API favors event streams over many small calls.

### 4.6 Endpoints

The read-only local metadata inventory is implemented in
`LocalEndpoint::scan_metadata`. Its protected namespace, streaming/error
contract and remaining snapshot gates are recorded in
[ADR 0010](docs/adr/0010-local-metadata-inventory.md).
`LocalPair::open` rejects equal or nested selected roots through held-directory
identities. Configuration also checks the pair catalog and keeps the agent
state directory out of any root that holds it.
Execution-time readiness remains pending.

Observed regular-file data-fork hashing is available below the agent API;
[ADR 0012](docs/adr/0012-local-content-hashing.md) records its bounded-read
contract and required residency/metadata gates before exposure.
Darwin stat observations include dataless flags. Known placeholders stop
inventory and hashing; macOS reads enforce a non-materializing thread policy.
Provider verification and cloud-root readiness remain required before agent exposure.
Held-root macOS volume UUID observations are available below the agent API;
[ADR 0015](docs/adr/0015-held-volume-uuid.md) records the probe and remaining
mount-continuity/readiness gates.
Native stat fidelity fields, xattr/resource-fork digests and ACL observations
are available below the agent API; [ADR 0016](docs/adr/0016-local-extended-metadata.md)
records bounds, platform limits and remaining canonical-metadata/capability gates.

```rust
trait Endpoint {
    fn capabilities(&self) -> &Capabilities;
    async fn identity(&self) -> Result<EndpointIdentity>;   // volume UUID + .foldover ID, host key, device ID
    async fn scan(&self, scope: ScanScope, sink: &mut dyn ScanSink) -> Result<ScanStats>;
    async fn stat(&self, path: &SyncPath) -> Result<Option<Entry>>;
    async fn read(&self, path: &SyncPath, range: ByteRange) -> Result<Chunk>;
    async fn create_dir(&self, path: &SyncPath) -> Result<Entry>;
    async fn write_atomic(&self, write: AtomicWrite) -> Result<Entry>;  // returns what was actually stored
    async fn rename(&self, from: &SyncPath, to: &SyncPath) -> Result<()>;
    async fn archive(&self, path: &SyncPath, reason: ArchiveReason) -> Result<()>;  // the only removal op
    async fn set_metadata(&self, path: &SyncPath, patch: &MetadataPatch) -> Result<Entry>;
    fn changes(&self) -> ChangeFeed;                       // Events(stream) | Poll(policy)
    fn availability(&self) -> AvailabilityFeed;            // mounted, reachable, peer online
}
```

**Capabilities** record, per endpoint:
- metadata support: xattrs (native, AppleDouble, streams, none), ACLs,
  symlinks, hard links, sparse files;
- file-ID stability and change events;
- operations: atomic replace (SFTP needs the `posix-rename@openssh.com`
  extension) and clones;
- naming: case sensitivity, Unicode normalization, SFM mapping;
- mtime granularity.

They are probed on every mount or connect, re-probed after a capability error
(`ENOTSUP`, failed rename-over), and drive both the engine and the UI's
fidelity warnings.
Filesystem support and driver operation fidelity are separate probes:
[ADR 0005](docs/adr/0005-metadata-fidelity.md) records the local baseline's
symlink, sparse-copy and exFAT Unicode failures.
Held-volume filesystem declarations are available below the agent API;
[ADR 0017](docs/adr/0017-filesystem-feature-declarations.md) records validity
handling and the remaining operation/readiness gates.

Endpoint-layer rules, not filters:
- `.foldover/` and temp files are never reported by `scan`.
- On volumes without native xattrs, `._X` AppleDouble files are read as
  metadata of `X` and never appear as entries.

| Backend | Key traits |
|---|---|
| `local` | APFS: full metadata, clones, stable file IDs, ns mtime. exFAT: case-insensitive, coarse mtime (exact granularity verified in S4), AppleDouble |
| `share` | `local` + mount lifecycle and quirks: no change events, unstable file IDs, SFM names |
| `sftp` | POSIX permissions + 1 s mtime; rename detection by hash; pipelined requests; atomic replace only with `posix-rename` |
| `peer` | Full Mac metadata, remote change events, block resume, stable IDs |

### 4.7 State and reconciliation

**State DB:**
- Pair configuration and initial writer/format guards:
  [ADR 0009](docs/adr/0009-pair-state-storage.md). The agent owns this store
  before serving requests. Authenticated create/read/update/remove APIs and the CLI
  are implemented, as is the per-path base; the remaining state tables are pending.
- Lives in `~/Library/Application Support/Foldover/state.db`: local disk,
  because WAL does not work on network file systems. The directory is
  excluded from Time Machine; the search index uses a `.noindex` suffix.
- Tables:
  - `pair`, `endpoint`;
  - `base`: last-synced entry per path (kind, size, mtime, file ID, hash,
    block list, stored metadata);
  - `snapshot`: last observed state per side;
  - `journal`, `run`, `run_item`, `conflict`, `tombstone`;
  - `watch_cursor`: FSEvents ID per volume UUID;
  - `device`.
- Secrets stay in Keychain; the DB stores references.

**Each endpoint root** gets one hidden `.foldover/` folder. Unlike GoodSync's
`_gsdata_`, it holds no sync database:
- **`archive/`:** retention-pruned; with several Macs on one share, each
  prunes only what it archived.
- **`id`:** lets Foldover notice a different disk mounted at the same path.
- **`lock`:** a lease with holder device, run ID and a heartbeat counter
  ([ADR 0032](docs/adr/0032-root-leases.md)).
  - **Reclaim:** a lease is dead when its counter hasn't moved across two
    reads one TTL apart, timed on the reclaimer's own clock. Taking it over
    is an exclusive-create claim file, then the lock swap, so two Macs can't
    both win. The claim file carries its own lease, so a claimant that dies
    mid-takeover is reclaimed by the same rule.
  - **Fencing:** the holder re-reads `lock` every batch, with the identity
    check, and aborts if another run owns it.
  - **Ordering:** locks are taken in endpoint-ID order; the UI shows
    "waiting for <device>".

**Classification per path** (v = some content; v′, v″ = other contents;
— = absent):

| Base | A | B | Result |
|---|---|---|---|
| v | v | v | in sync |
| v | v′ | v | copy A → B (and mirrored) |
| v | v′ | v″ | same content → update base; else **conflict** |
| — | v | — | create on B (and mirrored) |
| — | v | v′ | same content → update base; else **conflict** |
| v | — | v | archive on B (and mirrored) |
| v | — | v′ | **conflict**: deleted vs edited (and mirrored) |
| v | — | — | drop from base |

Entry classification and per-side baselines: [ADR 0007](docs/adr/0007-entry-reconciliation.md).
Local entry versions (content and tracked metadata): [ADR 0018](docs/adr/0018-local-entry-versions.md).
Pure deletion-review thresholds: [ADR 0013](docs/adr/0013-deletion-review-fuse.md).
Run execution, approvals and journal-free recovery: [ADR 0020](docs/adr/0020-run-execution.md).
Run journal and Undo: [ADR 0029](docs/adr/0029-undo.md).
Name clashes between spellings a folder treats as one: [ADR 0030](docs/adr/0030-name-clashes.md).
Hierarchy-aware candidate generation is implemented below the agent API;
[ADR 0014](docs/adr/0014-tree-plan-candidates.md) records tree validation,
conflicted-branch suppression and remaining executable-plan gates.

**Mode rules:**
- Mirror makes the target match the chosen source. Backup copies new,
  missing and source-changed entries; independent target edits wait until
  the source changes. Backup never removes target-only entries. Neither
  mode copies target changes back to its source.
- Content leaves a target only if it equals the base or has been archived.
  Everything else that must go is archived: Mirror's never-synced extras,
  overwritten versions, conflict losers. The one exception is an archive-less
  Backup target the user explicitly accepted (§3.6): files replaced there are
  overwritten unarchived.

**Plan enrichment** before gating:
- moves: same file ID or hash → rename;
- case-only renames: two-step on case-insensitive volumes;
- whole-tree time shifts (FAT/DST): metadata fix, not recopy;
- base repairs.

**Conflict kinds:** edited on both sides, delete vs edit, file vs folder, case
clash, metadata only. Metadata-only conflicts may auto-resolve by policy; the
rest go to the queue.

**Execution:**
1. Plan, then the gates (§3.6), then journal every op.
2. Order: creates parents-first, removals children-first; siblings run in
   parallel.
3. Work through an open handle on each root. Re-check volume identity per
   batch; abort on `endpointDown`. An unmounted volume therefore fails fast
   and never gets writes through its stale mount point.
4. Per op: re-check the target against the base, execute idempotently, read
   back the stored metadata.
5. Commit the base.

Scoped scan errors are reported in previews and run results. Their paths
and descendants are unknown, never absent: both folders and the base leave
them unchanged while independent changes continue. Ancestors that might
displace unread children are protected too. Root, identity, lease, canary
and cancellation failures still stop the run. Completed partial runs keep
the normal archive-retention policy, so persistent errors cannot fill the disk.

**Preserving an overwritten version**, best option first:
1. clone (APFS);
2. hard link;
3. journaled two-step: old → archive, new → path. Used where neither exists,
   for example plain SFTP. The UI shows this weaker guarantee.

**On start:** unfinished journal entries are verified against disk and
completed or rolled back; stale temp files are removed.

### 4.8 Change detection

FSEvents is a hint, never the truth.

| Source | Mechanism |
|---|---|
| Local volumes | One path-scoped FSEvents stream per pair root; event ID stored with volume and FSEvents-store UUIDs; replay after restart or sleep only when both match. Missing/changed UUIDs, remounts, dropped events or root changes → full root metadata rescan ([ADR 0002](docs/adr/0002-change-detection.md)) |
| Shares, SFTP | **Shallow** check on an adaptive interval (default 5 min): root and recently active directories, plus SMB change notifications if S3 shows they reach the agent. **Deep** attribute listing on run triggers and on the verification schedule, spaced by measured scan cost. The UI says how often other computers' changes are checked |
| Peers | Remote change stream from the other agent |
| Everything | Full metadata verification about hourly where cheap (scaled by scan cost on slow targets); content-hash pass weekly (catches same-size, same-mtime edits) |

- The watcher starts before the scan; directories that change during a scan
  are rescanned.
- Each side is compared with its own previous snapshot exactly: same size,
  same mtime, same file ID means unchanged; anything else is hashed (BLAKE3).
- Granularity tolerance only applies when comparing *across* endpoints, for
  example the first sync of two already-populated folders.

### 4.9 Transfer

- Same APFS volume: `clonefile` (instant, metadata-complete). Other local
  copies: `copyfile` with metadata flags.
- Remote: streamed whole files. Files over 16 MiB get a block list (fixed
  128 KiB–16 MiB blocks) for verification, resume and, with peers, block
  reuse across files, so moves and copies transfer nothing. Delta transfer of
  modified files comes in 1.1.
- Parallelism per device, not per pair: SSD 4–8 workers, HDD 1–2,
  network 4–8 connections with pipelined requests.
- A file modified while being copied is detected (stat before and after) and
  retried next cycle. Bandwidth cap per pair; global pause.

### 4.10 Metadata and names

- **Names:**
  - Stored names keep their raw bytes; comparisons use an NFC key,
    case-folded when either side is case-insensitive.
  - New files on foreign file systems are written in NFC.
  - SFM-mapped names on SMB are decoded for comparison and flagged.
- **Recorded reality:** after each write Foldover records what the target
  *actually* stored, so lossy targets don't trigger endless recopies.
- **Mac metadata:**
  - Finder tags, FinderInfo and resource forks are kept where the target
    allows (native xattrs, SMB streams, or `._` sidecars), otherwise dropped
    with a warning.
  - `com.apple.quarantine` is never written to network targets.
- **Special items:**
  - Packages sync as trees, keeping FinderInfo and the bundle bit; "treat
    bundles as single files" may come later.
  - Symlinks are synced as links and never followed outside the root.
  - Hard links are preserved only within capable endpoints.
  - iCloud-evicted (dataless) files are skipped by default rather than
    downloaded.

### 4.11 Peer protocol (Mac to Mac)

| Part | Choice |
|---|---|
| Discovery | mDNS `_foldover._udp` (mdns-sd; QUIC is UDP), with the Local Network permission explained before the prompt |
| Transport | QUIC (quinn + rustls), one control stream plus data streams; LAN only in 1.0; iroh evaluated for WAN later |
| Identity | Ed25519 device key in Keychain; device ID = certificate fingerprint |
| Pairing | Both Macs show a 6-digit code derived from the TLS session, both fingerprints and a commit-then-reveal nonce exchange ([ADR 0004](docs/adr/0004-peer-pairing.md)); the user confirms; keys pinned. Inbound policy: ask, allow paired, allow list. One-click revoke, which also closes open connections |
| Scope | A paired Mac sees only the roots its owner shared with that device. Every request carries a pair ID and is refused outside those roots |
| Ownership | One reconciler per pair: the Mac that created it. The other Mac serves its root and streams its changes, and shows the pair read-only. If the owner is offline, the pair waits. A second pair over the same roots is rejected |
| Wire | Versioned protobuf frames (`foldover-proto::peer`): hello, index offer (delta by index ID + sequence, as in Syncthing's BEP), block request/response, change stream, staged-write commit. Names in NFC. A reserved version-vector field leaves room for mesh sync |
| Layering | The `peer` backend dials and listens. Incoming requests go to a handler trait that the engine implements: it checks scope in `state` and answers through the local endpoint |

The transport driver provides pinned TLS connections, code exchange and
live-channel revocation ([ADR 0004](docs/adr/0004-peer-pairing.md#driver-implementation)).
The endpoint layer can restore its exact identity from the macOS Keychain;
installed-agent authorization across updates remains an integration gate.
Agents find each other on the local network, pair and keep paired devices
([ADR 0031](docs/adr/0031-mac-to-mac.md)); shared folders, wire messages
and the peer endpoint remain P5 work.

### 4.12 Linux readiness

- **CI from the first commit:** everything below the socket compiles and
  passes tests on Linux.
- **Platform code** lives only inside driver crates (`#[cfg(target_os)]`).
- **The Linux app** comes later. It links `foldover-client` directly (no FFI)
  and ships as:
  - `.deb`: cargo-deb with a systemd user unit;
  - Flatpak: GNOME runtime; Background, Documents and Secret portals; shares
    through GVfs.

## 5. Quality

### 5.1 Invariants (tested on every PR)

1. Content leaves a target only if it equals the base or has been archived,
   except on an archive-less Backup target the user explicitly accepted
   (§3.6).
2. Every path holds either its old or its complete new content. The one
   exception is the journaled two-step replace on endpoints without atomic
   replace (§4.7).
3. Every op is idempotent; after a crash the journal drains and the replicas
   converge.
4. The base updates only after both sides reach the recorded state.
5. Local and peer access stays inside the held root, including across symlinks
   and remounts. Standard SFTP uses path checks on a trusted server; its protocol
   cannot atomically prevent server-side directory/symlink races from escaping
   a selected folder. The app and CLI warn before remote writes. `.foldover/`
   and temp files never propagate.
6. All gates run before any destructive op is journaled.
7. Both versions of a conflict stay recoverable (archive or conflict
   copy).
8. Same inputs and schedule → same plan.

### 5.2 Tests

| Layer | What |
|---|---|
| Property tests (proptest) | Random three-tree states and op sequences against a reference model; failures shrink to minimal cases |
| Deterministic simulation ([ADR 0038](docs/adr/0038-simulation.md)) | Whole engine on one seeded executor with in-memory endpoints, mock clock, network chaos (turmoil), crashes at every journal transition, unmount mid-run; nightly long runs; any failing seed replays exactly |
| Quirk file system | In-memory endpoint emulating case-insensitivity, decomposed names, coarse mtimes, missing xattrs, volatile file IDs, lost events, no atomic replace, ENOSPC |
| Device matrix (nightly) | APFS SSD/HDD, HFS+, exFAT, FAT32; Samba with and without `vfs_fruit`; macOS file sharing; NFS; OpenSSH SFTP; two-agent peer |
| Fuzzing | Every parser: IPC frames, peer frames, AppleDouble, paths |
| Benchmarks (criterion) | Scan, hash, plan and DB hot paths against stored baselines |

**1.0 gate:** 14 consecutive nights of at least 100k seeded runs each, with
zero invariant violations.

**Test lab, needed by P4:**
- In CI containers: Samba, NFS and OpenSSH.
- Hardware to procure: one real NAS, a spinning-disk external drive, and a
  second Mac (for peers and macOS file sharing).

### 5.3 Performance budgets (initial; calibrated by the spikes)

| Measure | Budget |
|---|---|
| Cold scan, local APFS SSD | ≥ 10k entries/s (1M files ≈ 100 s) |
| No-change check of a 1M-file tree via FSEvents replay | < 5 s |
| First preview rows on screen | < 1 s; plan complete at 500k paths < 5 s after scan |
| Diff tree | 60 fps at 200k rows, flat memory |
| Local copy | ≥ 90 % of `cp`; same-volume = clone |
| SMB enumeration (`vfs_fruit` NAS) | ≥ 500 entries/s |
| SFTP large files on 1 GbE | ≥ 100 MB/s |
| Idle (agent + menu bar), including share polling | < 1 % of one core |
| Memory at 1M tracked files | ≤ 500 MB agent |
| App cold start to cached view | < 300 ms |

## 6. Repository, build, release

```
Foldover/
├── README.md AGENTS.md CLAUDE.md CHANGELOG.md PLAN.md LICENSE
├── Cargo.toml  Cargo.lock  rust-toolchain.toml  deny.toml
├── crates/            # §4.4
├── apple/             # Xcode project: app, agent embedding, extensions, FoldoverCore package
├── linux/             # later: GTK app, Flatpak manifest
├── packaging/         # entitlements, LaunchAgent plist, deb/systemd
├── scripts/           # build.sh (multi-target orchestrator), release.sh (lkm-release stub)
├── docs/              # research, ADRs
└── .github/           # ci.yml, release.yml, nightly.yml, dependabot.yml, zai-code-review.yml
```

- **Identifiers:** `ch.lkmc.foldover` on every platform; sub-identifiers
  scope under it (agent `ch.lkmc.foldover.agent`, tests
  `ch.lkmc.foldover.tests`).
- **Build:** `scripts/build.sh` follows the family's multi-target
  orchestrator style. Steps: arm64 Rust library → XCFramework + UniFFI
  Swift bindings → Xcode build → `dist/`. A missing toolchain skips its
  target by default and fails it when named.
- **Versioning:** one version, bumped in lockstep (workspace `Cargo.toml`,
  Xcode `MARKETING_VERSION`) by `scripts/release.sh`, a stub over the shared
  `lkm-release` engine. The engine may need a new "rust + xcode" kind.
- **CI:** least-privilege permissions, concurrency that cancels only
  superseded PR runs, timeouts on every job. Jobs:
  - Rust on macOS and Linux;
  - Swift build with strict concurrency;
  - lint: fmt, clippy, cargo-deny, layering;
  - nightly simulation and device matrix.
- **Release:** `release.yml` runs on `v*` tags. Order: test → build → ad-hoc
  sign innermost-out (embedded code, agent, extensions, app) → verify
  signatures → archive → GitHub release. Publish
  explicit quarantine-removal instructions. No Developer ID, notarization
  or stapling. Pre-release tags never become "latest".
- **Pinned:** `rust-toolchain.toml`, Xcode version, UniFFI version, action
  SHAs.

## 7. Roadmap

Rough sizes assume one developer with AI agents; the spikes recalibrate them.

| Phase | Size | Delivers | Exit criteria |
|---|---|---|---|
| **P0 Foundations** | ~1 wk | Repo conventions (README with LLM disclosure, AGENTS.md, CLAUDE.md, CHANGELOG, CI, dependabot, review workflow), workspace and Xcode skeletons, layering lint | Fresh clone → `scripts/build.sh` → green CI on macOS and Linux |
| **P1 Spikes** | ~4–6 wk | S1–S9 (§8), each closed by an ADR in `docs/adr/` | Every spike has a recorded pass/fail and decision; an ad-hoc-signed hello-world app with embedded agent installs on a clean macOS 27 Mac using the documented activation command |
| **P2 Engine** → v0.1 | ~6 wk | Types, state, journal, reconciler, gates, archive, `local` endpoint, basic filters, CLI, simulation and property suites | Suites green; kill -9 at any journal state recovers; 3 local pairs dogfooded for 2 weeks with zero manual repair |
| **P3 Agent + Mac app** → v0.2 | ~5 wk | `foldoverd`, service, IPC, client, UniFFI. Main window, pair creation, streamed preview with per-item overrides, menu bar, history, onboarding | App quit mid-sync → agent finishes; 200k-row preview at 60 fps; a new user creates a first pair in < 3 min |
| **P4 Shares, SFTP, automation** → v0.3 | ~6 wk | `share` and `sftp` endpoints. Mount management, capability and readiness warnings, full filters. Watchers, triggers, scheduler. Conflict center, archive browser, Undo, notifications, App Intents | SMB (Samba ± fruit, macOS server), NFS and SFTP pairs run unattended for 7 days; unplug or sleep mid-run loses nothing |
| **P5 Mac-to-Mac** → v0.4 | ~4 wk | Discovery, pairing, scope and ownership, QUIC transport, `peer` endpoint, remote changes, block resume | Two Macs pair in < 60 s; packet capture shows no plaintext; unshared roots are unreachable; sleep, wake and network switches resume; 24 h fuzz clean |
| **P6 Beta → 1.0** | ~6–8 wk | Public beta via GitHub releases, docs (capability matrix, NAS guide, limitations), accessibility, perf budgets enforced | §5 gates green; ≥ 6 weeks of beta with no open data-loss bug; crash-free sessions > 99.5 %; release timing is the owner's call (§10 item 10) |

After 1.0, in order of expected value:
- **1.1** speed: delta transfer, and an in-app SMB client if S2 favors it.
- **1.2** Linux app.
- **1.3** cloud endpoints (Dropbox, Proton Drive, Filen) behind the same
  trait.
- **1.4** mesh sync and WAN peers (version vectors, iroh relay).

If P5 threatens the schedule, 1.0 ships without Mac-to-Mac and P5 becomes 1.1.
Engine correctness gates are never traded for dates.

## 8. Risks and spikes

| # | Risk | Spike (P1) | Pass | Fallback |
|---|---|---|---|---|
| R1 | Two-way sync loses data | Continuous from P2 (§5.2) | 1.0 gate in §5.2 | Ship Mirror/Backup first; two-way behind a beta flag |
| S1 | UniFFI client under Swift 6 strict concurrency; event throughput over IPC | 3 d: XCFramework, event stream, callback, strict mode on | ≥ 50k events/s; no leaks in 1 h; concurrency escapes confined to one actor | Batch events per 100 ms; Swift 5 mode for one module; C ABI for the event channel |
| S2 | macOS-mounted SMB is slow or flaky; in-app SMB is immature | 1 wk: benchmark mounted SMB vs `smb2`/`smb` crates on three NAS setups, small files, unplug, sleep | Mounted path ≥ 500 entries/s and ≥ 200 small-file ops/s; errors within 10 s, no hang | Mounted SMB plus defensive mount management and a "recommended NAS settings" page; in-app client stays experimental |
| S3 | Change detection: path-scoped FSEvents replay; mount events inside the agent; whether SMB change notifications reach it | 3 d | No missed change after forced event loss; a change in an active share directory is seen ≤ 30 s at ≤ 2 % CPU; deep changes are bounded by the verification interval | Shallow polling only, with the honest "checked every N min" line |
| S4 | Metadata and operation fidelity across APFS, exFAT, Samba ± fruit, SFTP | 4 d: round-trip corpus (tags, forks, ACLs, flags, NFC/NFD and `:` names, case pairs, sparse, bundles), exFAT mtime granularity, SFTP `posix-rename` and hard links | No silent loss: every change is predicted by `Capabilities` | Per-target warnings; AppleDouble export option |
| S5 | Diff tree performance | 3 d: `NSOutlineView` in SwiftUI, 200k streamed rows, Quick Look, VoiceOver | 60 fps, flat memory, first rows < 1 s | Flat `Table` mode for huge plans |
| S6 | Ad-hoc signing of app + agent + Rust code; `SMAppService`; Sparkle | 3 d | Clean-VM install opens after documented `xattr` activation, agent registers, Sparkle update and downgrade both behave | None acceptable; fix before P2 ends |
| S7 | Agent permissions: Full Disk Access, removable/network volumes, Local Network. Private socket path and audit-token-bound ad-hoc client identity | 3 d | Prompts name Foldover; guided access flow; socket reachable at realistic path lengths; unrelated, unsigned and modified clients refused for privileged methods; permission behavior checked after an update | Onboarding card that grants the agent separately; folder-scoped access by default; Mach service for extensions |
| S8 | Finder Sync extension unreliable on macOS 27 | 2 d | Loads and badges across 20 mount cycles | Stays off by default; status never depends on it |
| S9 | Peer pairing security | 4 d (may move to P5) | No plaintext on the wire; unpaired peers get nothing; one-click revoke | TOFU with fingerprint confirmation |
| R10 | One developer, public-domain code, a crowded market | Process | — | Narrow 1.0 scope (§3.9), test pyramid, public capability matrix, responsive issue triage |

## 9. Distribution and pricing

Direct downloads for macOS 27 on Apple silicon, signed ad hoc. No Developer
ID, notarization or stapling. The installation instructions explain how to
remove `com.apple.quarantine` from the installed app before opening it.
The app checks GitHub's latest release and, when it finds a newer version,
shows a message with a link to the download page; replacing the app is
manual, like the first install (ADR 0045).

Foldover is free and open-source under the Unlicense. No sponsorships,
subscriptions, paid builds, gated features or license checks. No account or
telemetry; an opt-in diagnostics bundle helps support.

## 10. Decisions for the owner

Resolved by the owner, 2026-10-17:

1. **Pricing:** free and open-source, with no sponsorships or paid features.
2. **Mac-to-Mac in 1.0:** include if P5 passes its gates.
3. **OS floor:** macOS 27, Apple silicon only.
4. **Archive location:** `.foldover/archive/` at each endpoint root, with
   30-day retention.
5. **Distribution:** ad-hoc-signed builds activated with `xattr`; no
    Developer ID or notarization.
6. **Unsafe OS bindings:** small audited macOS driver modules are permitted,
   behind safe interfaces and native tests. Other layers retain the prohibition.
7. **SFTP root safety** (2026-10-19): support ordinary SFTP writes on trusted
   servers, with checks and a visible warning about server-side symlink races.
   This qualifies the strict root-isolation guarantee for SFTP only; a jailed
   account or Foldover helper is not required.
8. **Updates** (2026-10-22): no updater framework; the app checks the
   GitHub release feed and links a newer version to its download. A
   prerelease install hears about newer betas as well as the stable
   release that ends the line; a stable install is only offered stable
   releases.
9. **Localization** (2026-10-22): the UI is English-only; localization
   is out of scope.
10. **Release timing** (2026-10-22): the owner tags and ships releases.
    The §5.2 record of nightly runs and the "≥ 6 weeks of beta" rule in
    §7 inform that call but do not gate it; the other P6 criteria (no
    open data-loss bug, crash-free sessions > 99.5 %) still block.
11. **Hardware validation** (2026-10-22): the owner runs
    `docs/hardware-validation.md` against a real NAS and a second Mac
    per release candidate, and runs the VoiceOver pass personally.
