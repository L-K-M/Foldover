> Raw AI research report (opencode, GLM-5.3), 2026-10-16. Unedited. Corrections: [fact-checks](../README.md#corrections).

# Foldover Technical Research Report: Portable Rust Core + Native Swift macOS UI (Linux later)

**Date:** 2026-10-16 · **Scope:** Rust↔Swift interop, process/IPC architecture, core crates, network transports (SMB/SFTP/P2P), Linux UI + packaging, sync-engine testing strategy, and a concrete recommended stack with risks.
**Method:** Every version number, date, license and behavior claim below was checked against crates.io, docs.rs, GitHub, official manuals or vendor documentation on 2026-10-16; bracketed markers `[n]` refer to the Sources list at the end. Anything that could not be confirmed from a primary source is explicitly marked **UNVERIFIED**.

---

## 0. Executive summary

- **Interop:** Use **UniFFI** (0.32.2, Sept 2026, MPL-2.0, Mozilla) to expose the Rust core to Swift, packaged as a universal **XCFramework consumed through a SwiftPM binary target**. This is the pattern proven at scale by Element X, Mozilla application-services and Bitwarden. swift-bridge is interesting but effectively single-maintainer and still 0.x; cbindgen + raw C ABI is the fallback for hot paths. ([1][2][3][8][9][10][11][12][17])
- **Process model:** One signed app containing both the SwiftUI UI and the Rust core-as-library for the MVP, **plus an `SMAppService`-registered LaunchAgent** so sync keeps running with the UI closed. Design the UI↔core boundary as an async, event-driven API from day one so the same core can later be hosted in a standalone daemon; on Linux that daemon is a systemd user service. Syncthing, Resilio and Dropbox all keep the sync engine alive independently of any window. ([15][19][20][23][25][26][27])
- **Core crates:** notify 8.2 (v9 in RC), ignore, BLAKE3, fastcdc, zstd, rusqlite (WAL) for metadata, tokio 1.53, reflink-copy, globset, keyring 4, xattr/exacl. **Avoid sled** (no stable release since 2021, self-described beta); redb or fjall only if you outgrow SQLite. ([28][29][35][36][38][40][43][44][45][46][47][48][50][51][52][53][57][58])
- **Network:** SFTP via **russh + russh-sftp** (very active, async, post-quantum kex already). SMB: no pure-Rust client is battle-tested yet (two promising 2025–2026 projects are 0.x); for MVP rely on OS mounts and optionally `pavao` (libsmbclient bindings) for in-app connections. LAN peer-to-peer on **quinn/rustls or iroh 1.x**, discovery via **mdns-sd**. There is **no maintained Rust implementation of Syncthing's BEP** — implement your own protocol (the spec is small) rather than adopting BEP. ([59][60][61][62][63][64][67][68][69][70][72][73][74])
- **Linux later:** gtk4-rs 0.11 + libadwaita 0.9 + Relm4 0.11 (or plain gtk4-rs); fanotify's unprivileged mode (kernel 5.13+) addresses inotify watch limits; Flatpak's sandbox heavily shapes the design (portals, `/media` access, background autostart). Ship .deb via cargo-deb + a Flatpak. ([33][34][52][74][75][76][79][80])
- **Testing:** Copy Dropbox Nucleus's proven recipe — deterministic, seeded randomized testing against a three-tree model (a CanopyCheck-style planner checker + a Trinity-style whole-engine simulator with mocked FS/network/time), property-based tests with proptest, fuzzing with cargo-fuzz, benchmarks with criterion. Rust-native simulation harnesses: **turmoil** (tokio-rs, now with experimental simulated filesystems) and **madsim** (used by RisingWave). ([27][81][82][83][84][85][86][87])

---

## 1. Swift ↔ Rust interop

### 1.1 UniFFI — recommended

**Current state.** `uniffi` is at **0.32.2 (released 2026-09-23)**, with a steady cadence (0.30.0 Oct 2025, 0.31.0 Jan 2026, 0.32.0 Jun 2026) [1]. License: **MPL-2.0** [3]. It is developed by Mozilla, "used extensively by Mozilla in Firefox mobile and desktop browsers" and considers itself production-ready while pre-1.0 [1]. First released 2020; Swift, Kotlin, Python and Ruby are first-class targets, with third-party C#, Go, Dart and JS bindings [1].

**Async support.** Async/futures support landed in **0.24.0 (2023-06-21)** ("Added support for async/futures") and has been extended ever since: async cancellation fixes in 0.25.0; **async constructors** in 0.27.0 (alternate constructors in all languages, *primary* async constructors only in Swift); per-method `async_runtime` overrides in 0.28.0; `#[uniffi::export(async_runtime = "tokio")]` extended to trait exports in 0.32.0 [2]. In Swift, async functions surface as `async` methods bridged onto a Rust future; the 0.29 series renamed parts of the async FFI surface (`UniffiForeignFutureDroppedCallback` etc.) [2].

**Callback interfaces (Swift → Rust calls).** Proc-macro callback interfaces have existed since 0.24.0; the callback ABI was reworked onto **vtables** in 0.27.0; `[WithForeign]` (Rust and foreign implementations of the same trait) arrived in 0.26.0 and is being replaced in 0.32.0 by the clearer `#[uniffi::export(foreign)]` / `#[uniffi::export(rust, foreign)]` [2]. This is exactly the mechanism Foldover needs for Swift-side callbacks (auth prompts, progress UI hooks).

**Swift 6 strict-concurrency compatibility.** UniFFI has been actively tracking Swift 6: 0.29.0 marked generated Swift interfaces `Sendable`; **0.29.1 (2025-03-18)**: "Protocols generated for Swift now conform to the `Sendable` protocol … foreign implemented traits also must when Swift 6 conformance is enabled"; **0.31.2 (June 2026)** fixed "Swift: Prevent strict concurrency warning with callback interface tables" [2]. So current UniFFI generates code usable under Swift 6 strict concurrency, with the constraint that foreign-implemented callback traits must themselves be `Sendable` when conformance checking is on [2]. Budget time for annotating callback implementations accordingly.

**Other capabilities worth knowing:** zero-copy `&[u8]`/`Data` argument passing (0.32.0, sync functions only), `bytes` → `Data` mapping since 0.24.0, error enums surfaced with `localizedDescription` (0.28.0), per-item renaming via TOML, and a dedicated **`uniffi-bindgen-swift`** binary "with additional Swift-specific features" added in 0.28.2 [2].

**Packaging: XCFramework + SwiftPM binaryTarget.** Apple's documented pattern: build a multiplatform **XCFramework** bundle, then declare either a remote `.binaryTarget(name:url:checksum:)` (zip + `swift package compute-checksum`) or a local path-based binary target inside the package [8]. UniFFI's flow fits directly: `cargo build` for `aarch64-apple-darwin` and `x86_64-apple-darwin`, `lipo`-create a fat dylib, wrap with `xcodebuild -create-xcframework` (Apple's "Creating a multiplatform binary framework bundle" article) [88], then add the generated `modulemap` + Swift bindings as package sources. Since 0.29.1 you can even declare extra frameworks (e.g. `CoreFoundation`) in the generated modulemap for the xcframework [2].

**Build tooling.**
- **cargo-swift 0.11.1 (2026-05-20, Apache-2.0/MIT)** by Antonius Naumann — `cargo swift init` / `cargo swift package` builds a Swift Package from a UniFFI crate for iOS/macOS; it pins to specific UniFFI versions (0.11.1 ↔ UniFFI 0.31.1) so versions must be matched manually; repo active (pushed Aug 2026, 290 stars, not archived) [17][18]. The UniFFI README itself lists cargo-swift as an external resource, alongside a community "uniffi-starter" template with XCFramework generation scripts (**repo URL not re-verified — UNVERIFIED**) [1].
- For production CI, most teams roll their own GitHub Actions matrix that runs `uniffi-bindgen-swift`, `lipo` and `xcodebuild -create-xcframework`, then publishes a tagged Swift package release — Bitwarden's `build-swift.yml` → `bitwarden/sdk-swift` flow is documented publicly [12].

**Universal binaries, signing, notarization.** Build per-target with cargo + `lipo` (or emit two XCFramework slices) [8][88]. Embedded libraries must be code-signed: Apple requires all distributed executables signed, **Developer ID** certificates, the **Hardened Runtime** enabled, a **secure timestamp** in signatures, and no `get-task-allow` entitlement; the notary service scans the artifact and you staple the ticket (or Gatekeeper finds it online). Since **Nov 1, 2023** only `notarytool` (Xcode 14+) is accepted for uploads [16]. Practically: sign innermost binaries first (`codesign`), and let Xcode's "Distribute App → Developer ID" flow or `notarytool submit … --staple` handle the app bundle; the embedded Rust dylib inside the XCFramework is covered by the app bundle's signature [16].

**Real-world users (verified):**
- **Element X iOS / matrix-rust-sdk** — Element X is "a total rewrite using the Matrix Rust SDK underneath", consuming the SDK "through an FFI layer exposed as a swift package" (`matrix-rust-components-swift`); the SDK README states it is production-ready, sponsored and maintained by Element, and used by Element X iOS/Android, Fractal and iamb [9][10].
- **Mozilla application-services** — Firefox's Rust components (logins, places, fxa-client, nimbus, …) generate their Kotlin/Swift FFI with UniFFI; primary consumers are Fenix (Firefox Android) and Firefox iOS [11].
- **Bitwarden** — the internal SDK (`bitwarden/sdk-internal`, public mirror) contains `bitwarden-uniffi`: "Mobile bindings for swift and kotlin using UniFFI", with CI building XCFrameworks and auto-PRs into the iOS client; note the SDK is dual-licensed GPL + commercial and is explicitly "not intended for public use" [12].
- **libsignal (contrast case)** — Signal keeps its Rust core but uses its **own generated C/JNI/Node "bridge" layers** rather than UniFFI; the Swift library is a CocoaPod built "on top of the C FFI produced by rust/bridge/ffi/", with prebuilt Rust artifacts fetched by checksum [13][14]. This validates the "Rust core + thin generated FFI" model at extreme scale, while showing a team choosing bespoke tooling (AGPL-3.0) [13].
- Widely cited in this space: **1Password** (Rust core across platforms) and **Proton** — **UNVERIFIED** (1Password's original engineering posts appear retired; no primary source could be re-verified on 2026-10-16).

### 1.2 swift-bridge

`swift-bridge` **0.1.59 (2026-01-06)**, MIT/Apache-2.0, by Chinedu fn (single primary maintainer); 1,143 stars, 100 open issues, repo not archived (last push Sept 2026) [4][5]. Release history shows a **~16-month gap** (0.1.57, Aug 2024 → 0.1.58, Dec 2025) [4]. Strengths: bridge-module macro (`#[swift_bridge::bridge]`) with `extern "Rust"` / `extern "Swift"` sections, built-in mappings for `String`, `Result<T,E>` (→ throwing functions), `Option`, `Vec<T>`, opaque types, **async functions in both directions** (including `extern "Swift" async`), and a stated Minimum Supported Swift Version of Swift 6.0 [4]. Gaps are documented in its own type table: `&[T]`, `Arc<T>`, arrays, `Box<dyn Fn>` Rust→Swift etc. are "Not yet implemented" [4]. No flagship production deployer could be verified (**UNVERIFIED**). Verdict: elegant API, but a 0.x, single-maintainer project with an uneven release cadence is a hard sell for a long-lived product core.

### 1.3 cbindgen + hand-written C ABI

`cbindgen` **0.29.4 (2026-06-09)**, **MPL-2.0**, repo under the **mozilla** org; steady releases (0.28 Jan 2025, 0.29 May 2025) [6][7]. It generates C/C++11 headers from a Rust library exposing `extern "C"` APIs; used in production by webrender, stylo, maturin and tquic [6]. Swift imports C headers natively via a modulemap, so this is the "no magic" option: full ABI control, zero codegen lock-in, but you hand-write all glue (strings as `char*`+len, error codes, ownership conventions). Reasonable for a handful of performance-critical or callback-heavy surfaces; tedious for a whole app API. Newer alternatives named by UniFFI's README: **Diplomat** (C/C++-focused; Swift support status **UNVERIFIED**) and **Interoptopus** [1].

### 1.4 Recommendation

**UniFFI**, with rationale:
1. Proven at Foldover's exact problem shape (Rust core, native Swift app, later other frontends) by Element X, Mozilla and Bitwarden — and independently validated at scale by libsignal's bespoke-but-similar C-FFI approach [9][10][11][12][13].
2. Async, callback interfaces and error mapping are built in and actively hardened for Swift 6 strict concurrency [2].
3. Mozilla-backed, MPL-2.0, regular releases 2020→2026 [1][2][3].
4. The packaging path (XCFramework + SwiftPM binary target + `uniffi-bindgen-swift`) is documented by both UniFFI and Apple [2][8].

Keep the UniFFI surface deliberately small (a `FoldoverCore` façade + event/callback interfaces + DTO records); if a specific call ever shows FFI overhead, punch a cbindgen-generated C function through for that hot path only [6].

---

## 2. Process architecture: in-process library vs background agent/daemon

### 2.1 What the incumbents do

- **Syncthing** is a single daemon binary with an embedded **web GUI + REST API on 127.0.0.1:8384**; the same daemon speaks its BEP protocol on TCP+QUIC port 22000 with relay fallback [23][24]. Every "flavor" (Android app, wrappers) runs "the same Syncthing core underneath" [23]. On macOS, the official **syncthing-macos** app (Objective-C/Swift, MIT, active — pushed Oct 2026, default branch `v2`, 3,970 stars) wraps the daemon in a native bundle rather than rewriting the core [25].
- **Resilio Sync** ships a long-running agent plus web UI: its help center documents running Sync **as a Windows service** (`rslsyncsvc`), Linux packages installed as a service under a dedicated `rslsync` user, and a headless "configuration mode" [26]. (macOS-specific service details not verified — treat as UNVERIFIED.)
- **Dropbox** runs its Nucleus sync engine as part of the always-on desktop client ("the engine that powers Dropbox on hundreds of millions of users' machines"); the engine's architecture (single control thread, offloaded I/O) is documented publicly [27]. Exact macOS process/extension structure today (File Provider, helpers) — **UNVERIFIED** on this pass.

The common thread: **the sync engine outlives any window.** Foldover should assume from day one that the core runs without a UI.

### 2.2 macOS options

- **`SMAppService` (macOS 13+)** — Apple's modern API: "use SMAppService to register and control LoginItems, LaunchAgents, and LaunchDaemons as helper executables for your app"; `SMAppService.agent(plistName:)` registers a LaunchAgent that lives inside the app bundle (replacing hand-installed plists in `~/Library/LaunchAgents` and the deprecated `SMLoginItemSetEnabled`); `register()` requires user approval surfaced in System Settings → Login Items, and `openSystemSettingsLoginItems()` deep-links there [15].
- **launchd plist mechanics** (still the substrate under SMAppService): keys like `RunAtLoad`, `KeepAlive` (bool or conditional dict incl. `NetworkState`, `SuccessfulExit`, `PathState`), `ProcessType` (`Interactive`/`Adaptive`/`Background` — affects resource limits), `ThrottleInterval` (default 10 s respawn throttle), and `Sockets`/`MachServices` for launch-on-demand [19].
- Notarization applies to the whole app, including the embedded agent and the Rust dylib [16].

### 2.3 Linux options

- A **systemd user service** (`~/.config/systemd/user/foldover.service`) keeps sync running per-user; **`loginctl enable-linger`** keeps the user manager (and thus the service) running after logout and starts it at boot — see loginctl(1) [20].
- Packaging: **cargo-deb 3.8.0** has first-class `[package.metadata.deb.systemd-units]` support so the .deb installs and enables the unit correctly [80]. Under Flatpak, autostart/background goes through the Background portal (see §5.3) [79].

### 2.4 IPC choices if the UI is a separate client

| Option | Notes | Source |
|---|---|---|
| **In-process (UniFFI library)** | Simplest; events via callback interfaces; "UI is just another host" keeps you daemon-ready. | [1][2][15] |
| **XPC** | Apple-idiomatic for macOS IPC; launchd `MachServices` supports launch-on-demand services. No Linux equivalent — don't make it the only seam. | [19] |
| **Unix domain socket + JSON-RPC** | Cross-platform, trivially debuggable; mirrors Syncthing's REST/GUI control surface (community crates wrap that REST API). | [23][73] |
| **gRPC (tonic 0.14.6, MIT, active — grpc/grpc-rust org)** | Strong schemas, streaming, codegen; can run over UDS; heavier dependency tree; handy if the daemon later serves non-Swift clients. | [22] |
| **D-Bus (zbus 5.19.0, active)** | Right answer for Linux desktop integration (notifications, portals, session services); not a macOS seam. | [21] |

### 2.5 Recommendation for Foldover

- **MVP:** SwiftUI app embeds the Rust core via UniFFI. Additionally register a tiny **LaunchAgent** (`SMAppService.agent`, `RunAtLoad` + `KeepAlive`, `ProcessType: Adaptive`) that hosts the *same* core binary in headless mode, so sync continues when the app is quit — matching Syncthing/Resilio user expectations [15][19][23][26].
- **Design constraint:** the core must be a **library with an async, event-driven API** (no reliance on living inside a GUI process): single event log, cancellable operations, state queries. This is what makes the Linux systemd-user-service port a recompile plus a thin `main()`, not a redesign [20][80].
- **UI↔agent protocol:** versioned JSON-RPC over a Unix socket in `$XDG_RUNTIME_DIR`/user-scoped directory (typed on the Rust side; mirrored in Swift via Codable). JSON first for debuggability; tonic remains available if you later need a multi-client daemon API [22].

---

## 3. Core crates (all versions checked 2026-10-16)

### 3.1 File watching — notify

- **notify 8.2.0** (2025-08-03, stable; **CC0-1.0** for the core crate, MIT/Apache-2.0 for the satellite crates). **notify 9.0.0-rc.5** (2026-08-30) is in release-candidate phase. Backends: Linux/Android **inotify**; macOS **FSEvents or kqueue**; Windows ReadDirectoryChangesW; BSDs/iOS kqueue; polling fallback. Used by cargo-watch, rust-analyzer, watchexec, deno, mdBook, etc. [28]
- **Debouncers:** `notify-debouncer-full` 0.7.0 (2026-01-23; 0.8.0-rc.2 tracks notify 9) and `notify-debouncer-mini` 0.7.0 (2025-08-03). The "full" debouncer merges rename pairs, stitches renames via file IDs, collapses directory deletions, and suppresses duplicate creates/modify-after-create — most of the event hygiene a sync engine needs [30][31].
- **Known issues / sharp edges (from notify release notes and platform man pages):**
  - FSEvents coalesces events and delivers them after a **latency** window — notify 9 finally exposes `Config::with_fsevent_latency` (PR #930, in 9.0.0-rc.5); v9 also replaced `fsevent-sys` with modern `objc2-core-services` bindings and fixed panics on non-UTF8 paths [29].
  - inotify is **not recursive** (notify adds per-directory watches), has per-user watch/instance/queue limits (`/proc/sys/fs/inotify/max_user_watches` …), **does not see events on network filesystems**, may coalesce identical events, and queue overflows (`IN_Q_OVERFLOW`) lose events — robust apps must rescan, as inotify(7) itself advises; notify 8.2.0 added an explicit error when `max_user_watches` is exhausted [28][29][32].
  - macOS repeats "create" events; the debouncer now emits `remove` even when create+remove collapse (0.8.0-rc.2) [29].
- **Verdict:** notify + debouncer-full is the standard choice; budget engineering for reconciliation scans, latency tuning on macOS, and watch-limit UX on Linux (see fanotify in §5.2) [28][29][32].

### 3.2 Scanning

- **walkdir 2.5.0** (2024-03-01, MIT/Unlicense, BurntSushi) — the sequential standard; performance "comparable with `find` and glibc's nftw" [35].
- **ignore 0.4.33** (2026-08-04, MIT/Unlicense) — ripgrep's walker: parallel traversal with `.gitignore`/`.ignore`/glob/type filtering built in; very active (many 2025–26 releases) [36]. This is Foldover's default scanner: gitignore-style filtering is what users expect.
- **jwalk 0.9.0** (2026-08-05) — **explicitly unmaintained**: its own crate description now says "Use `dua-core` instead" [37]. Do not adopt.

### 3.3 Hashing

- **blake3 1.8.7** (2026-08-20; CC0-1.0 / Apache-2.0) — the official implementation; SIMD (SSE→AVX-512/NEON), Merkle-tree based → parallelism, streaming and *verified* streaming, `rayon` multithreading, XOF/KDF/MAC modes; "much faster than MD5, SHA-1, SHA-2, SHA-3, and BLAKE2"; adopted by Iroh, OpenZFS, Bazel, ClickHouse and others [38]. Also the content-addressing choice of iroh-blobs [70].
- **xxhash-rust 0.8.19** (2026-09-28) — xxh3 with SIMD, feature-gated per algorithm; ideal for non-adversarial change fingerprints (mtime/inode → "probably changed" checks) [39].
- Pattern: xxh3 to detect candidates, BLAKE3 for authoritative content IDs and dedup keys [38][39].

### 3.4 Content-defined chunking & delta transfer

- **fastcdc 5.0.0** (2026-08-22, active repo nlfiedler/fastcdc-rs) — pure-Rust FastCDC with three variants (`v2016`, `v2020` "rolling two bytes", `ronomon` compat), deterministic cut points, streaming and async-streaming (`tokio`/`futures` features), criterion benchmark suite included [40].
- **fast_rsync 0.2.0** (2023-03-31, Apache-2.0, **dropbox/fast_rsync**) — pure-Rust rsync algorithm with SIMD (x86-64/aarch64); its benchmarks show ~3.6 GiB/s signature computation vs ~0.68 GiB/s for librsync; **MD4-based legacy format only** — the README explicitly warns to verify output with a cryptographic hash [41]. Dormant since 2023 but small and complete.
- **librsync 0.2.6** (2026-08-26) — bindings that **vendor the C librsync (LGPL-2.0)**; streaming API; LGPL obligations apply to the vendored C [42].
- **zstd 0.14.2** (2026-10-10; BSD-3-Clause binding; zstd itself BSD/GPLv2 dual) — bindings with `zstdmt` multithreaded compression, dictionaries, frame-size hints; mature and active [43].
- Design note: Syncthing's BEP uses **fixed power-of-two blocks (128 KiB–16 MiB, sized to keep files under ~2000 blocks)** rather than CDC [24]; CDC (fastcdc) + BLAKE3 chunk IDs gives Foldover dedup + resumable transfer + chunk-reuse deltas, and fast_rsync is a good add-on for whole-file deltas against dumb targets (e.g., SFTP servers) [38][40][41].

### 3.5 State database

- **rusqlite 0.40.2** (2026-08-08, MIT) — ergonomic SQLite wrapper; `bundled` feature compiles SQLite 3.53.2 in-tree; WAL mode gives "readers do not block writers and a writer does not block readers", auto-checkpoint at ~1000 pages, but **WAL does not work over network filesystems** (shared-memory requirement) — keep the DB on local disk; note SQLite's recent **WAL-reset bug** (rare checkpoint race present 3.7.0→3.51.2, fixed in 3.51.3, 2026-03-13) as a reason to pin a current bundled version [44][45].
- **redb 4.4.0** (2026-10-13, MIT/Apache-2.0, cberner) — pure-Rust ACID embedded KV, copy-on-write B+trees, MVCC, "**Stable and maintained… The file format is stable**"; very active (a 2.6.x maintenance branch also exists) [46].
- **fjall 3.1.12** (2026-10-03, MIT/Apache-2.0) — LSM engine (RocksDB-like), LZ4 default compression, optional serializable transactions, key-value separation for blobs; **single-process only** ("A single database may not be loaded in parallel from separate processes") [47].
- **sled 0.34.7** (2021; 1.0.0-alpha.124, 2024-10-11) — repo still exists (pushed Apr 2026) but **no stable release in five years**, and the README still says "if reliability is your primary constraint, use SQLite. sled is beta", with on-disk format migrations promised before 1.0 [48][49]. **Do not build on sled.**
- Verdict: **rusqlite (WAL) for MVP** — one file, SQL for tree/journal queries, battle-tested crash behavior, and the Dropbox precedent (Nucleus persists its state in SQLite) [27][44][45]. Revisit redb only if KV latency ever becomes the bottleneck [46].

### 3.6 Runtime & platform plumbing

- **tokio 1.53.3** (2026-10-13, MIT) — LTS lines 1.51 (→Mar 2027) and 1.53 (→Sep 2027); rolling ~6-month MSRV [50].
- **xattr 1.6.1** (2025-09-21, MIT/Apache-2.0, Stebalien; ~285M downloads) — get/set/list extended attributes on Linux/macOS/BSDs [51].
- **ACLs:** **exacl 0.13.0** (updated 2026-05; ~2.8M downloads) manipulates ACLs on **macOS, Linux and FreeBSD**; **posix-acl 1.2.0** (2023, Linux-only POSIX.1e) [52]. (macOS ACLs are not POSIX ACLs — exacl's cross-platform abstraction is the point.)
- **reflink-copy 0.1.30** (2026-06-18, cargo-bins org; actively maintained) — CoW copies via `clonefile`/`FICLONE`/`copy_file_range` with fallback; the older `reflink` 0.1.3 (2019) is stale [53][54].
- **filetime 0.2.29** (2026-05-12, MIT/Apache) — set/read atime/mtime; its README now points to std's `File::set_times`, but filetime keeps a killer feature for Foldover: `RUSTFLAGS=--cfg emulate_second_only_system` **rounds all times to 1 s to emulate HFS-era filesystems in tests** [55].
- **unicode-normalization 0.1.25** (2025-10-30, unicode-rs) — NFC/NFD/NFKC/NFKD per UAX #15; core to macOS filename handling (BEP, for comparison, mandates **UTF-8 NFC** names on the wire) [24][56].
- **globset 0.4.20** (2026-08-04, MIT/Unlicense, BurntSushi) — fast multi-glob matching (regex-compiled sets), case-insensitive and literal-separator options; powers `ignore`'s filters [57].
- **keyring 4.2.0** (2026-08-29, MIT/Apache-2.0) — v4 split into `keyring-core` + per-platform stores: `apple-native-keyring-store` (macOS Keychain), `linux-keyutils-keyring-store`, `dbus-secret-service-keyring-store` / `zbus-secret-service-keyring-store` (Secret Service); very active in 2026 [58].

---

## 4. Network transport

### 4.1 SMB (and NAS shares generally)

Options found on crates.io (search `smb`, 2026-10-16) [59]:

| Crate | Version / updated | What it is | Maturity signals |
|---|---|---|---|
| **pavao** | 0.3.1 (2026-09-02), MIT | Safe wrapper over **libsmbclient** (SMB 1/2/3; SMB1 explicit opt-in), dialect bounds SMB2.0–3.1.1, optional vendored build | Active (veeso); **serializes all libsmbclient calls process-wide** (no parallel SMB ops in one process); C dependency; Samba licensing applies to libsmbclient (**verify LGPL/GPL terms before static linking — UNVERIFIED here**) [60] |
| **smb** (afiffon/smb-rs) | 0.12.1 (crate updated 2026-09-20) | Pure-Rust SMB client, split crates (smb-msg/smb-transport/smb-fscc) | Repo created 2024-12, 86 stars, ~52K downloads; **GitHub license field: NOASSERTION** — must be clarified before use [59][62] |
| **smb2** (vdavid/smb2) | 0.31.0 (2026-10-16) | Pure-Rust SMB2/3 client with pipelined I/O; repo tagline claims "faster than native macOS SMB on all operations" (the project's own claim) | Repo created **2026-04**, 31 stars, 50 releases in ~6 months — extremely young but very active; Apache-2.0 [59][61] |
| smolder-smb-core, gfeh-smb, msrpce-smb | 0.1–0.4 (2026) | SMB2/3 + RPC / QUIC-era experiments | Early research grade [59] |
| remotefs-smb 1.0.0 | 2026-09-15 | remotefs trait impl over pavao | Niche [59] |

**Assessment:** pure-Rust SMB2/3 finally exists (two active 2025–26 projects), but both are 0.x with small communities and unproven interoperability across NAS firmware in the wild — SMB is a compatibility minefield. Pragmatic MVP: **use the OS's native SMB client via mounted volumes** for sync roots (Linux/GVfs even exposes network mounts at `/run/user/$UID/gvfs`, reachable inside Flatpak sandboxes [78]), and treat `pavao` as the optional "connect without mounting" escape hatch, understanding its global-serialization limit [60]. Re-evaluate `smb`/`smb2` quarterly — if either reaches 1.0 with a NAS compatibility matrix, an embedded client becomes attractive (own credentials, own timeouts, no mount plumbing) [61][62].
**NFS:** no established Rust NFS client surfaced in our searches (**absence UNVERIFIED**) — plan on OS mounts only. **AFP:** legacy; out of MVP scope (current macOS client status **UNVERIFIED** on this pass).

### 4.2 SFTP / SSH

- **russh 0.64.1** (2026-10-05, Eugeny/russh; Apache/MIT) — async SSH 2.0 client/server on tokio; modern algorithm set including **post-quantum hybrid key exchange (`mlkem768x25519-sha256`)** and OpenSSH **strict kex (Terrapin mitigation)**; `deny(unwrap/expect/indexing)` safety posture; adopters include Warpgate, Yazi (terminal file manager, via russh-sftp), lapdev [63].
- **russh-sftp 3.0.1** (2026-09-28) — SFTP v3 client **and** server for russh; extensions (limits/hardlink/fync/statvfs/expand-path); self-reported throughput ~257/203 MiB/s up/down against OpenSSH in Docker, on par with Go's `pkg/sftp` and Node's ssh2 [64].
- **ssh2 0.9.6** (2026-06-30, rust-lang org) — mature **libssh2** bindings; synchronous only; slower release cadence; system OpenSSL/libssh2 deps [65].
- **openssh 0.11.6** (2025-12-03) — scriptable wrapper around the OpenSSH **binary** (honors `~/.ssh/config`); great escape hatch, external-process model [66].
- Verdict: **russh + russh-sftp** — pure Rust, async, fast-moving security posture, proven adopters [63][64]. Use ssh2 only if you must match libssh2 behavior exactly.

### 4.3 LAN peer-to-peer

- **quinn 0.11.12** (2026-09-14, MIT/Apache) — the standard pure-Rust QUIC stack (tokio-based; `quinn-proto` is a deterministic, I/O-free state machine — handy for simulation); the README explicitly covers self-signed certs via **rcgen** and **trust-on-first-use** validation through a custom verifier, i.e., pairing is DIY but well supported [67].
- **rustls 0.23.46** (2026-10-14; Apache-2.0/MIT/ISC) — TLS 1.2/1.3 with pluggable crypto providers (aws-lc-rs default, ring optional); extremely active; 0.24 in dev [68].
- **iroh 1.3.0** (2026-09-28, n0-computer; Apache-2.0/MIT) — "p2p quic connections dialed by public key": each endpoint's **public key is its TLS identity (mutual authentication for free)**, QUIC hole-punching with encrypted-relay fallback, DNS/Pkarr address lookup; built on n0's newer **noq** QUIC implementation (introduced March 2026); reached **1.0 on 2026-06-15** ("Dial Keys, not IPs") after a public roadmap program; releases continue monthly; **first-party Swift, Kotlin, Python and JS bindings announced 2026-06-18** (iroh-ffi); iroh-blobs provides BLAKE3-tagged blob transfer [69][70]. Note: n0's company structure changed during 2025 (details **UNVERIFIED** on this pass), but engineering velocity through Oct 2026 is demonstrably high [70].
- **libp2p 0.57.1** (2026-10-15) — the maximal modular stack (noise/quic/yamux/kad-dht/gossipsub/relay…); powerful if you need DHT-based global discovery and many transports; heavier and conceptually denser than iroh for a product that mainly needs "my Macs find each other" [71].
- **mdns-sd 0.21.5** (2026-10-05, keepsimple1; MIT/Apache) — pure-Rust mDNS/DNS-SD responder+querier (RFC 6762/6763) with a detailed self-assessed RFC-compliance table, **no async-runtime dependency** (thread + flume channels usable from sync and async), IPv4+IPv6, tested against Avahi/dns-sd/Bonjour; self-described beta; very active [72].
- **Pairing/trust models in the wild:** Syncthing's BEP authenticates peers by **pre-shared SHA-256 certificate fingerprints ("Device IDs")** exchanged out-of-band, mutual configuration required [24]; iroh's identity *is* the public key (TOFU or verified out-of-band) [69]; quinn leaves it to you (rcgen + custom verifier / TOFU) [67].
- Verdict: **quinn + rustls + mdns-sd as the baseline** (direct LAN connections, Syncthing-style device-ID pairing), with **iroh** as the strong "it just works across networks/NAT" upgrade — and its new Swift bindings are a notable bonus for a Swift-UI product [67][68][69][70][72].

### 4.4 Syncthing BEP in Rust?

The **Block Exchange Protocol** is well-specified: proto3 messages over TLS 1.3; `Hello` (magic `0x2EA7D90B`) before auth; `ClusterConfig` first post-auth; `Index`/`IndexUpdate` with **version vectors** and per-file block lists; `Request`/`Response` block fetch; `DownloadProgress` partial availability; delta indexes via `{index id, max sequence}`; optional LZ4 message compression; a 500 MB message cap; block sizing 128 KiB–16 MiB to stay under ~2000 blocks/file [24]. A crates.io search for "syncthing" finds only REST-API wrappers (`syncthing`, `syncthing-async`, `syncthing-types`) plus **`rbep` 0.1.1** ("SyncThing BEP implementation in clean Rust", first published 2026-08-31, 24 total downloads) — i.e., **no maintained BEP implementation exists** [73]. Interop with Syncthing would mean owning a BEP + discovery + relay-compatible stack; instead, implement your own minimal BEP-*inspired* protocol (version vectors, block index, temp-file hydration) over quinn/iroh [24][67][69].

---

## 5. Linux future

### 5.1 UI toolkit

- **gtk4 crate 0.11.5** (2026-09-20, MIT; gtk-rs project; MSRV 1.83) — safe bindings tracking GTK4 feature versions (`v4_2`…`v4_22`, `gnome_48`–`gnome_50` profiles); strong docs and the "GUI development with Rust and GTK 4" book [74].
- **libadwaita 0.9.2** (2026-07-07) — GNOME design-system bindings, paired with gtk4 0.11 [75].
- **relm4 0.11.0** (2026-04-08, MIT/Apache) — Elm-style architecture on gtk4-rs with a `view!` DSL, component model, optional libadwaita support; active; used by real apps (LACT, fm, NixOS Configuration Editor, …) [76]. (0.11.0 currently has a docs.rs build failure — last-good docs at 0.10.0 [76].)
- **Qt route:** **cxx-qt 0.10.0** (2026-08-24, KDAB; safe Rust↔Qt/QML via CXX; active) is credible if you specifically want Qt/QML [77]. But for a GNOME-era app distributed via Flathub, gtk4/libadwaita is where the ecosystem gravity is (portal support, runtimes); choose **gtk4-rs + libadwaita (+ Relm4 if the team likes Elm)** [74][75][76].
- Architecture bonus: a Rust core with an async API slots directly into gtk-rs's async main-loop integration; keep the UniFFI boundary macOS-only and give Linux a thin GTK layer over the same core.

### 5.2 Watching limits: inotify vs fanotify

- inotify's per-user limits, non-recursive watches and blindness to network filesystems are documented in inotify(7); notify surfaces `max_user_watches` exhaustion since 8.2.0 [28][29][32].
- **fanotify** can watch an **entire mount or filesystem race-free** (no per-directory walk) and gained create/delete/move events in Linux 5.1 plus file-handle (FID) reporting in 5.1–5.9; since **5.13 unprivileged listeners are allowed with restrictions**: no permission events, no `FAN_MARK_MOUNT`/`FAN_MARK_FILESYSTEM` (inode marks only), `FAN_REPORT_FID` required, and PID reporting limited — full-power modes still require CAP_SYS_ADMIN [33][34]. Practical plan: inotify+notify by default; a fanotify backend (privileged via the systemd service, or unprivileged inode-mark mode) for "millions of files" users; periodic rescan always.

### 5.3 Flatpak portals & sandboxing impact

- Flatpak's default sandbox has **no host filesystem and no network**; you grant `--filesystem=` (host paths, XDG dirs, `/media`, `/run/media`, `/mnt` for external drives) and `--share=network` via finish-args, while portals mediate one-off access (file chooser, open URI, network status, …) [78].
- Sync-app consequences: (a) sync roots outside XDG dirs need explicit `--filesystem` grants (external drives via `/media`, `/run/media`) [78]; (b) host-mounted network shares (SMB etc.) are reachable through GVfs FUSE at `/run/user/$UID/gvfs` with the gvfs socket permissions — the sanctioned route to NAS from a sandbox [78]; (c) **background/autostart**: the portal set includes **Background** and **Documents** portals (API reference in the xdg-desktop-portal docs) — a Flatpak'd Foldover must request autostart via the Background portal rather than installing unit files, and use the document portal for user-granted files [79]; (d) secrets should go through the **Secret** portal / Secret Service rather than raw keyfiles [58][79].
- Design guidance: make the daemon "portal-aware" (ask for paths, don't assume) and keep a non-Flatpak .deb distribution for power users and full-filesystem setups.

### 5.4 Packaging

- **cargo-deb 3.8.0** (2026-09-01, active) — builds .deb from Cargo metadata with maintainer scripts, assets, and **built-in systemd unit integration** (`[package.metadata.deb.systemd-units]`); supports cross-compilation and dbgsym packages [80].
- **Flatpak**: flatpak-builder manifest on a GNOME runtime with cargo sources; distribute via Flathub (building/dependency flow per docs.flatpak.org) [78].
- Result: the same daemon binary ships as (1) a .deb with a systemd user service for full power, and (2) a Flatpak with portal-mediated permissions.

---

## 6. Testing a sync engine

### 6.1 The Dropbox Nucleus playbook (verified from their engineering post)

Dropbox's 2020 post "Testing sync at Dropbox" documents how they de-risked the Nucleus rewrite (Rust) [27]:

- **Data model designed for testability:** three trees — **Remote** (server), **Local** (disk observation), **Synced** (merge base) — with the invariant "all three trees converge"; nodes keyed by **unique IDs** (a move is one atomic op, not delete+add cascades); design principle "design away invalid system states" [27].
- **Determinism:** nearly all logic on a **single control thread** (I/O offloaded); deterministic hasher instead of Rust's randomized HashMap; every test runs from a **seeded PRNG**, failing seeds print and re-run identically; "tens of millions of randomized test runs" nightly, with CI auto-creating a task per failing seed [27].
- **CanopyCheck** (planner-level property tester, named after QuickCheck): generates random three-tree configurations, runs the planner loop, asserts termination (<200 planning iterations), no panics, and directional correctness invariants (e.g., server-only files must reach disk; local adds must not be evicted); **shrinks failing trees** to minimal cases [27].
- **Trinity** (whole-engine simulator): Nucleus is one giant `Future`; Trinity is a **custom executor** that interleaves polling with injected chaos — reorders/fails intercepted async filesystem and network calls, mutates local/remote state, fast-forwards mock **time**, and simulates crashes by snapshotting/restoring an in-memory filesystem; asserts final consistency *and* determinism by re-running the seed; in-memory FS gave ~10× more test runs [27].
- **Heirloom**: a real-network protocol suite, ~100× slower, accepting weaker determinism [27].
- Limitations they call out: native-OS coverage (fsync-on-crash durability) can't be fully simulated — motivating targeted crash tests on real disks [27].

### 6.2 Industry precedents: FoundationDB & TigerBeetle

- **FoundationDB** "uniquely integrates a deterministic simulation framework, used to test every new feature of the system under a myriad of possible faults" (SIGMOD'21 papers announcement) — the origin of the DST discipline [81].
- **TigerBeetle** runs production code in a deterministic "flight simulator": fault injectors for partitions, packet loss, crashes, skewed clocks, disk corruption and misdirected I/O, with simulated time accelerated **700×** — "a fleet of 1024 dedicated CPU cores simulate … two millennia of faults and recoveries — every day" [82].

### 6.3 Rust-native harnesses

- **turmoil 0.7.2** (2026-04-24, tokio-rs; MIT) — deterministic multi-host simulation on a single thread: simulated TCP/UDP **plus an experimental simulated filesystem** (`unstable-fs`) with per-host isolation, configurable fsync probability and `sim.crash()` crash-recovery testing — remarkably close to Trinity's core ideas, off the shelf [84].
- **madsim 0.2.34** (2025-10-11, Apache-2.0) — "Magical Deterministic Simulator": swap `tokio`→`madsim-tokio` etc. behind `--cfg madsim`, then kill processes and partition the network deterministically; used by **RisingWave**; explicitly credits FoundationDB's approach [83].
- Neither simulates a *filesystem-quirk* layer out of the box — write your own in-memory FS facade (as Trinity did) [27].

### 6.4 Property-based, fault injection, quirk emulation, fuzzing, benchmarks

- **proptest 1.12.0** (2026-10-10; proptest-rs) — Hypothesis-style strategies + shrinking; ideal for the **two-replica model**: generate random operation sequences + partition/disconnect schedules, run against a trivial reference model (path→content maps), assert convergence and no-data-loss invariants; failure-persistence files convert regressions into unit tests [85].
- **Fault-injection checklist** (each backed by the harnesses above): kill -9 mid-copy (staging → atomic rename crash-consistency), disconnects/response reordering (turmoil/madsim) [83][84], ENOSPC (inject via your I/O trait; SQLite WAL documents behavior under adverse disk conditions) [45], read-only volumes, permission errors, clock skew (mock time à la Trinity) [27][84].
- **Filesystem quirk emulation:** case-insensitivity and NFC/NFD normalization layers in your in-memory FS; `filetime`'s `--cfg emulate_second_only_system` for 1-second-mtime emulation [55]; BEP's NFC-on-the-wire rule is a good model for normalization policy [24]. (No off-the-shelf "quirky filesystem" crate was found — build it as a test facade.)
- **cargo-fuzz 0.13.2** (2026-06-09, rust-fuzz; MIT/Apache) — libFuzzer harness management for every parsing surface: network frames, index records, path strings, xattr blobs [86].
- **criterion 0.8.2** (2026-02-04; Apache-2.0/MIT) — statistics-driven micro-benchmarks with baselines and regression detection; fastcdc's bench suite is a good template (`--save-baseline` / `--baseline` workflow) [40][87].

### 6.5 Recommended testing pyramid for Foldover

1. **Model-based proptest suite** over the planner (three-tree invariants à la CanopyCheck) [27][85].
2. **turmoil/madsim deterministic simulations** of the full engine (2+ replicas, chaos scheduling, crash/restart) on every PR; nightly long-runs across thousands of seeds [83][84].
3. **In-memory quirk FS** (case-insensitive, NFD, 1-s mtimes, ENOSPC injector) as the default test backend [27][55].
4. **cargo-fuzz** targets for every wire parser [86].
5. **criterion** benchmarks guarding scan/hash/chunk/DB hot paths [87].

---

## 7. Recommended stack (concrete) and risks

### 7.1 The stack

| Layer | Choice | Version (2026-10-16) | License | Why |
|---|---|---|---|---|
| Swift↔Rust | **UniFFI** (+ `uniffi-bindgen-swift`) | 0.32.2 | MPL-2.0 | Async, callbacks, Swift 6 work; Element/Mozilla/Bitwarden-proven [1][2][3][9][10][11][12] |
| Packaging | XCFramework (arm64+x86_64) + SwiftPM binaryTarget; sign + notarize via notarytool | — | — | Apple-documented flow [8][16][88] |
| Scaffold tooling | cargo-swift (dev convenience) | 0.11.1 (matches UniFFI 0.31.x) | Apache-2.0/MIT | Fast iteration; pin versions [17][18] |
| macOS background | App-embedded core + `SMAppService` LaunchAgent (`RunAtLoad`, `KeepAlive`) | macOS 13+ | — | Sync survives closed UI; visible toggle in Login Items [15][19] |
| Linux background | systemd user service (+ `enable-linger`); cargo-deb `systemd-units` | cargo-deb 3.8.0 | MIT | Same daemon, packaged [20][80] |
| UI↔core seam | async UniFFI façade now; versioned JSON-RPC over UDS when daemonized | — | — | Keeps Linux/future-UI options open [22][23] |
| Watching | notify + debouncer-full; fanotify backend later on Linux | 8.2.0 (9.0.0-rc.5) | CC0 / MIT-Apache | Standard; v9 adds FSEvents latency config [28][29][30][31] |
| Scanning | ignore (gitignore-style filters) | 0.4.33 | MIT/Unlicense | Parallel + filters; ripgrep-grade [36] |
| Hashing | blake3 (+ xxhash-rust fingerprints) | 1.8.7 / 0.8.19 | CC0-Apache / Apache-MIT | Fast, content-addressable [38][39] |
| Chunking/delta | fastcdc (+ fast_rsync where whole-file deltas help) | 5.0.0 / 0.2.0 | MIT / Apache-2.0 | Deterministic CDC; SIMD rsync [40][41] |
| Compression | zstd | 0.14.2 | BSD-3 (binding) | Multithreaded, dictionaries [43] |
| State DB | rusqlite (bundled, WAL) | 0.40.2 | MIT | SQL for trees/journals; WAL concurrency; Dropbox precedent [27][44][45] |
| Runtime | tokio (pin an LTS line) | 1.53.3 | MIT | Default choice; LTS policy [50] |
| Metadata | xattr, exacl, reflink-copy, filetime, unicode-normalization, globset | 1.6.1 / 0.13.0 / 0.1.30 / 0.2.29 / 0.1.25 / 0.4.20 | MIT/Apache etc. | Cross-platform fidelity [51][52][53][55][56][57] |
| Secrets | keyring (apple-native / secret-service stores) | 4.2.0 | MIT/Apache | Modular per-OS stores [58] |
| SFTP | russh + russh-sftp | 0.64.1 / 3.0.1 | Apache-MIT | Async, active, PQ-ready [63][64] |
| SMB | OS mounts first; pavao for embedded client; watch smb/smb2 | pavao 0.3.1 | MIT (libsmbclient terms apply) | Pure-Rust SMB immature [59][60][61][62] |
| P2P | quinn + rustls + mdns-sd; iroh for NAT traversal | 0.11.12 / 0.23.46 / 0.21.5 / 1.3.0 | MIT-Apache etc. | QUIC + TOFU pairing (Syncthing-style device IDs) [24][67][68][69][70][72] |
| Linux UI | gtk4 + libadwaita (+ relm4 optional) | 0.11.5 / 0.9.2 / 0.11.0 | MIT | Ecosystem default; Flatpak-friendly [74][75][76] |
| Testing | proptest + turmoil (+ madsim) + cargo-fuzz + criterion; three-tree model | 1.12.0 / 0.7.2 / 0.2.34 / 0.13.2 / 0.8.2 | MIT-Apache | Nucleus-style DST in Rust [27][81][82][83][84][85][86][87] |

### 7.2 Key risks & mitigations

1. **UniFFI is pre-1.0** — breaking changes land between minors (the changelog carries regular breaking-change sections) [2]. *Mitigation:* pin exact versions in CI; treat upgrades as scheduled work; keep the exposed API narrow.
2. **Swift 6 strict concurrency** — generated code is compatible, but foreign-implemented traits must be `Sendable` under Swift 6 conformance [2]. *Mitigation:* enable strict checking early; isolate callbacks onto actors.
3. **SMB compatibility** is the biggest product risk; pure-Rust clients are <2 years old and 0.x [59][61][62]. *Mitigation:* OS mounts by default; in-app SMB behind an experimental flag; conformance-test against Samba + popular NAS firmware.
4. **Watcher event loss** (inotify overflow, FSEvents coalescing) is a platform property, not a bug [29][32]. *Mitigation:* periodic reconciliation scans + content-hash verification; surface "rescanning" UX.
5. **Flatpak sandboxing** constrains the Linux daemon's filesystem/network reach and autostart (portals) [78][79]. *Mitigation:* design permission requests into onboarding; ship the .deb alongside.
6. **Single-maintainer dependency risk** (mdns-sd, fastcdc, russh-sftp are small-team projects [40][64][72]). *Mitigation:* vendoring/fork policy; prefer wire protocols with public specs you can reimplement.
7. **Licensing watch-items:** MPL-2.0 for UniFFI/cbindgen (file-level copyleft; compatible with proprietary linking) [3][7]; libsmbclient's Samba licensing if embedded (verify — **UNVERIFIED**) [60]; smb-rs's undeclared GitHub license [62]; librsync's LGPL-2.0 vendored C if chosen [42].

---

## Sources

1. UniFFI crate — version history & README: https://docs.rs/crate/uniffi/latest
2. UniFFI CHANGELOG (async, Sendable/Swift 6, foreign exports, uniffi-bindgen-swift): https://raw.githubusercontent.com/mozilla/uniffi-rs/main/CHANGELOG.md
3. UniFFI crates.io metadata (license MPL-2.0): https://crates.io/api/v1/crates/uniffi
4. swift-bridge crate: https://docs.rs/crate/swift-bridge/latest
5. swift-bridge repo status (stars, issues, pushes): https://api.github.com/repos/chinedufn/swift-bridge
6. cbindgen crate: https://docs.rs/crate/cbindgen/latest
7. cbindgen crates.io metadata (license MPL-2.0): https://crates.io/api/v1/crates/cbindgen
8. Apple — Distributing binary frameworks as Swift packages (binaryTarget, checksums): https://developer.apple.com/tutorials/data/documentation/xcode/distributing-binary-frameworks-as-swift-packages.json
9. Element X iOS README (total rewrite on Matrix Rust SDK via Swift package): https://raw.githubusercontent.com/element-hq/element-x-ios/main/README.md
10. matrix-rust-sdk README (production-ready; Element-sponsored; bindings): https://raw.githubusercontent.com/matrix-org/matrix-rust-sdk/main/README.md
11. Mozilla application-services README (UniFFI-generated Swift/Kotlin bindings; Fenix/Firefox iOS consumers): https://raw.githubusercontent.com/mozilla/application-services/main/README.md
12. Bitwarden sdk-internal README (bitwarden-uniffi; xcframework CI; GPL+commercial licensing): https://raw.githubusercontent.com/bitwarden/sdk-internal/main/README.md
13. libsignal README (Rust core; JNI/C/Node bridge layers; AGPL-3.0): https://raw.githubusercontent.com/signalapp/libsignal/main/README.md
14. libsignal swift/README (CocoaPod over generated C FFI; prebuilt artifacts): https://raw.githubusercontent.com/signalapp/libsignal/main/swift/README.md
15. Apple — SMAppService reference (macOS 13+; LaunchAgents; replaces SMLoginItemSetEnabled): https://developer.apple.com/tutorials/data/documentation/servicemanagement/smappservice.json
16. Apple — Notarizing macOS software before distribution (Developer ID, hardened runtime, secure timestamp, notarytool-only since Nov 1 2023, stapling): https://developer.apple.com/tutorials/data/documentation/security/notarizing-macos-software-before-distribution.json
17. cargo-swift crate (UniFFI version matrix): https://docs.rs/crate/cargo-swift/latest
18. cargo-swift repo status: https://api.github.com/repos/antoniusnaumann/cargo-swift
19. launchd.plist(5) (RunAtLoad, KeepAlive, ProcessType, ThrottleInterval, Sockets/MachServices): https://www.manpagez.com/man/5/launchd.plist/
20. loginctl(1) — enable-linger / user services: https://www.freedesktop.org/software/systemd/man/latest/loginctl.html
21. zbus crate: https://docs.rs/crate/zbus/latest
22. tonic crate (grpc-rust org): https://docs.rs/crate/tonic/latest
23. Syncthing docs — Getting Started (single daemon; GUI+API on 127.0.0.1:8384; TCP/QUIC 22000): https://docs.syncthing.net/intro/getting-started.html
24. Syncthing BEP v1 spec (TLS 1.3; Device IDs = SHA-256 cert fingerprints; block-size table; 500 MB limit; NFC names; delta indexes): https://docs.syncthing.net/specs/bep-v1.html
25. syncthing-macos repo (native wrapper bundle; v2 branch; active): https://api.github.com/repos/syncthing/syncthing-macos
26. Resilio Sync help center search results (run-as-service on Windows/Linux, rslsync user, config mode): https://help.resilio.com/hc/en-us/search?query=run+as+service
27. Dropbox Tech — "Testing sync at Dropbox", Isaac Goldberg, 2020-04-20 (Nucleus, three trees, CanopyCheck, Trinity, Heirloom): https://dropbox.tech/infrastructure/-testing-our-new-sync-engine
28. notify crate (backends, licenses, users; v9 RC series): https://docs.rs/crate/notify/latest
29. notify release notes feed (9.0.0-rc.5 `with_fsevent_latency`; objc2 migration; 8.2.0 max_user_watches notice): https://github.com/notify-rs/notify/releases.atom
30. notify-debouncer-full crate: https://docs.rs/crate/notify-debouncer-full/latest
31. notify-debouncer-mini crate: https://docs.rs/crate/notify-debouncer-mini/latest
32. inotify(7) man page (limits, non-recursive, network-FS blindness, overflow): https://man7.org/linux/man-pages/man7/inotify.7.html
33. fanotify(7) man page (mount/filesystem-wide marks; event types since 5.1): https://man7.org/linux/man-pages/man7/fanotify.7.html
34. fanotify_init(2) man page (unprivileged mode since 5.13 and its restrictions): https://man7.org/linux/man-pages/man2/fanotify_init.2.html
35. walkdir crate: https://docs.rs/crate/walkdir/latest
36. ignore crate: https://docs.rs/crate/ignore/latest
37. jwalk crate (marked unmaintained; "use dua-core"): https://docs.rs/crate/jwalk/latest
38. blake3 crate: https://docs.rs/crate/blake3/latest
39. xxhash-rust crate: https://docs.rs/crate/xxhash-rust/latest
40. fastcdc crate (v2016/v2020/ronomon; async streams; bench workflow): https://docs.rs/crate/fastcdc/latest
41. fast_rsync crate (Dropbox; MD4 caveat; SIMD benchmarks): https://docs.rs/crate/fast_rsync/latest
42. librsync crate (vendored LGPL-2.0 librsync): https://docs.rs/crate/librsync/latest
43. zstd crate: https://docs.rs/crate/zstd/latest
44. rusqlite crate (bundled SQLite 3.53.2): https://docs.rs/crate/rusqlite/latest
45. SQLite WAL documentation (reader/writer concurrency; no network FS; WAL-reset bug fixed in 3.51.3, 2026-03-13): https://www.sqlite.org/wal.html
46. redb crate (ACID, MVCC, stable format): https://docs.rs/crate/redb/latest
47. fjall crate (LSM; single-process warning; LZ4 default): https://docs.rs/crate/fjall/latest
48. sled crate docs ("beta… use SQLite"; 0.34.7 from 2021): https://docs.rs/crate/sled/latest
49. sled repo (pushed 2026-04; alpha train ended 2024-10): https://api.github.com/repos/spacejam/sled
50. tokio crate (LTS 1.51/1.53 policy): https://docs.rs/crate/tokio/latest
51. xattr crate + crates.io metadata (MIT/Apache-2.0; platforms): https://crates.io/api/v1/crates/xattr
52. crates.io search "acl" (exacl 0.13.0; posix-acl 1.2.0): https://crates.io/api/v1/crates?q=acl
53. reflink-copy crate: https://docs.rs/crate/reflink-copy/latest
54. reflink crate (stale, last release 2019): https://docs.rs/crate/reflink/latest
55. filetime crate (`emulate_second_only_system` cfg): https://docs.rs/crate/filetime/latest
56. unicode-normalization crate: https://docs.rs/crate/unicode-normalization/latest
57. globset crate: https://docs.rs/crate/globset/latest
58. keyring crate (v4 core + platform stores): https://docs.rs/crate/keyring/latest
59. crates.io search "smb" (smb 0.12.1, smb2 0.31.0, smb-msg/transport/fscc, remotefs-smb, smolder/gfeh/msrpce): https://crates.io/api/v1/crates?q=smb
60. pavao crate (libsmbclient wrapper; process-wide serialization; vendored option): https://docs.rs/crate/pavao/latest
61. vdavid/smb2 repo (created 2026-04; Apache-2.0; release velocity): https://api.github.com/repos/vdavid/smb2
62. afiffon/smb-rs repo (created 2024-12; license NOASSERTION): https://api.github.com/repos/afiffon/smb-rs
63. russh crate (PQ kex; strict kex; adopters incl. Yazi, Warpgate): https://docs.rs/crate/russh/latest
64. russh-sftp crate (extensions; throughput benchmarks): https://docs.rs/crate/russh-sftp/latest
65. ssh2 crate (libssh2 bindings; sync): https://docs.rs/crate/ssh2/latest
66. openssh crate (wraps OpenSSH binary; mux client): https://docs.rs/crate/openssh/latest
67. quinn crate (rcgen/TOFU guidance; quinn-proto determinism): https://docs.rs/crate/quinn/latest
68. rustls crate (providers; release cadence): https://docs.rs/crate/rustls/latest
69. iroh crate (dial-by-public-key; relay fallback; noq): https://docs.rs/crate/iroh/latest
70. iroh blog (1.0 on 2026-06-15 "Dial Keys, not IPs"; noq 2026-03; Swift/Kotlin/Python/JS bindings 2026-06-18; release cadence through Oct 2026): https://www.iroh.computer/blog
71. libp2p crate: https://docs.rs/crate/libp2p/latest
72. mdns-sd crate (RFC compliance table; no async-runtime dependency): https://docs.rs/crate/mdns-sd/latest
73. crates.io search "syncthing" (REST wrappers; rbep 0.1.1 of 2026-08-31): https://crates.io/api/v1/crates?q=syncthing
74. gtk4 crate (MSRV 1.83; feature versions; MIT): https://docs.rs/crate/gtk4/latest
75. libadwaita crate: https://docs.rs/crate/libadwaita/latest
76. relm4 crate (Elm-style; libadwaita feature; apps list): https://docs.rs/crate/relm4/latest
77. cxx-qt crate (KDAB; safe Rust↔Qt): https://docs.rs/crate/cxx-qt/latest
78. Flatpak docs — Sandbox Permissions (defaults; filesystem grants; gvfs FUSE; /media & /run/media; portals): https://docs.flatpak.org/en/latest/sandbox-permissions.html
79. xdg-desktop-portal docs (Background, Documents, Secret portals; backends): https://flatpak.github.io/xdg-desktop-portal/docs/
80. cargo-deb crate (assets, maintainer scripts, systemd-units integration): https://docs.rs/crate/cargo-deb/latest
81. FoundationDB blog (SIGMOD'21 papers post: "deterministic simulation framework" quote): https://www.foundationdb.org/blog/
82. TigerBeetle homepage (DST; 700× time acceleration; "2000 years every 24 hours"): https://tigerbeetle.com/
83. madsim crate (FoundationDB-inspired; RisingWave user): https://docs.rs/crate/madsim/latest
84. turmoil crate (tokio-rs; simulated net + unstable-fs crash testing): https://docs.rs/crate/turmoil/latest
85. proptest crate (strategies, shrinking, persistence): https://docs.rs/crate/proptest/latest
86. cargo-fuzz crate (libFuzzer): https://docs.rs/crate/cargo-fuzz/latest
87. criterion crate (statistics, baselines): https://docs.rs/crate/criterion/latest
88. Apple — Creating a multiplatform binary framework bundle (XCFramework): https://developer.apple.com/documentation/xcode/creating-a-multi-platform-binary-framework-bundle
