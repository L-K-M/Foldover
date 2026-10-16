> Raw AI research report (devin, SWE-2 max), 2026-10-16. Unedited. Corrections: [fact-checks](../README.md#corrections).

# Foldover — File-Sync Landscape Survey

**Date:** 2026-10-16. **Scope:** tools relevant to Foldover's MVP (local/external-disk, NAS over SMB/NFS, SFTP, Mac-to-Mac LAN sync). Every factual claim carries an inline citation `[n]` → Sources at the end. Anything not confirmed against a source is marked **UNVERIFIED**.

---

## Tool survey

### 1. Syncthing (+ syncthing-macos)

- **What:** Open-source continuous file synchronizer; current release **v2.1.6** (Oct 2026), with v2.1.7-rc.2 tagged Oct 13, 2026 [1][2].
- **License:** `MPL-2.0` [1].
- **Platforms:** Windows, macOS, Linux, BSDs, Android, plus community ports; the official Mac story is the separate `syncthing-macos` bundle [2][5].
- **Sync model:** Peer-to-peer mesh; folders are Send-Only, Receive-Only, Send-Receive, or Receive-Encrypted; any number of devices per folder [3][4].
- **Change detection:** OS filesystem watcher (default on) plus periodic full rescans (default hourly); watcher batches changes for ~10 s before hashing [3].
- **Conflict handling:** Loser of an edit-edit conflict is renamed to `<name>.sync-conflict-<date>-<time>-<modifiedBy>.<ext>`; conflict files propagate to all peers as normal files [3].
- **Transfer/delta:** Files split into 128 KiB–16 MiB blocks hashed with SHA-256; only differing blocks fetched, possibly from any peer or locally [3][4].
- **Mac UX:** Core is a CLI + web console; `syncthing-macos` (MIT [5], latest v2.0.3+1, Aug 25, 2026, bundles Syncthing 2.0.3, requires macOS 11+) is a deliberately "frugal" native tray wrapper — menus and notifications only, config still in the browser [5][6].
- **Price:** Free, donation-supported [2].
- **Lesson for Foldover:** A documented protocol (BEP over TLS 1.3, protobuf messages) is what made Syncthing an ecosystem — Foldover's protocol should be a spec, not an implementation detail [4].

### 2. Unison

- **What:** The original academically-specified two-way synchronizer (Pierce et al., since 1998); latest GitHub release **v2.54.0** (May 1, 2026) [7][8].
- **License:** `GPL-3.0-or-later` [7][8].
- **Platforms:** Linux, macOS, Windows, other Unix; written in OCaml [8].
- **Sync model:** Strict pairwise two-way sync between two "roots" — local paths or remote over SSH/socket; keeps a persistent archive of last-synced state to detect which side changed [8].
- **Change detection:** Compares current filesystem state against the stored archive (timestamps first, deeper analysis on change); no real-time watching — runs are user-triggered or scripted [8].
- **Conflict handling:** Same-file-changed-on-both-sides is a real conflict: displayed in a reconciliation list and resolved manually (or via `prefer`/`batch` preferences); conflicts are skipped, never silently merged [8].
- **Transfer:** Rsync-algorithm delta transfer of changed blocks [8].
- **Mac UX:** Text UI + a bundled Cocoa GUI whose maintainers openly warn "no one is contributing to verify that the Mac GUI continues to work"; a third-party Swift/AppKit front end (`bcourbage/unison-ui-mac`, GPLv3, macOS 15+) re-wraps the OCaml engine with a proper conflict-review step [7][9]. Notably, v2.54.0 also warns the lablgtk GUI may be dropped even in a micro release [7].
- **Price:** Free [7].
- **Lesson for Foldover:** Unison proves users will accept per-run reconciliation with explicit conflict review — but its historical same-version/same-OCaml requirement and rotting GUI show that protocol versioning and UI ownership matter as much as the sync math [7][8].

### 3. FreeFileSync (+ RealTimeSync)

- **What:** Open-source folder comparison and synchronization app; current version **14.12** (Sep 9, 2026) [10].
- **License:** Source `GPL-3.0` [11]; **but binaries carry use restrictions**: free for personal use only — commercial use requires the paid Business Edition; a Donation Edition adds auto-updater, parallel sync, portable build [14].
- **Platforms:** Windows, macOS (10.10+ per older docs; current macOS 14-era supported), Linux [11].
- **Sync model:** Pairwise folder pairs with three built-in variants — **Two way** (bidirectional, tracked via `sync.ffs_db` database files), **Mirror**, **Update** — plus Custom rules [12].
- **Change detection:** On-demand compare (by date/size/content) plus **RealTimeSync**, a companion watcher that gets OS change notifications and fires a batch job after a configurable idle time; manual admits network shares can silently deliver no notifications at all [13].
- **Conflict handling:** In two-way mode, both-sides-changed files surface as conflict rows for manual resolution; versioning options recycle replaced files [12].
- **Standout:** The compare-then-preview grid before anything is written — the industry reference UX for safe sync; detects moved files when file IDs exist (not on SFTP or FAT) [12].
- **Mac UX:** Functional but non-native (wxWidgets); feels ported [11].
- **Price:** Free (personal); Donation Edition via donation; Business Edition per-device paid [14].
- **Lesson for Foldover:** Preview-first builds user trust like nothing else; and RealTimeSync's idle-debounce + "watchers lie on network shares" caveat is exactly the edge case Foldover's NAS story must handle explicitly [12][13].

### 4. rsync / openrsync

- **What:** The 30-year-old one-way incremental copy engine; upstream **rsync 3.5.1** (Sep 21, 2026; 3.5.0 fixed 33 security issues) [15]. Apple's bundled tool is now **openrsync** (ISC) since macOS Sequoia, replacing the fossilized rsync 2.6.9 [18].
- **License:** rsync `GPL-3.0-or-later` (with OpenSSL exception in packaging) [15][16]; openrsync `ISC` [17].
- **Platforms:** Every Unix/macOS/Linux; openrsync is officially OpenBSD but portable (incl. macOS, Linux, *BSD) and only accepts a subset of rsync's flags [17].
- **Sync model:** Strictly one-way source→destination; over SSH, rsync daemon protocol, or local [15].
- **Change detection:** Quick-check (mtime+size) or checksums; rolling-checksum delta transfer sends only changed blocks [15].
- **Conflict handling:** None — destination is made to match source.
- **Mac UX:** CLI only; the shipped binary on modern macOS is openrsync, not "real" rsync [18].
- **Price:** Free.
- **Lesson for Foldover:** Apple replaced rsync over GPLv3 rather than ship it — proof that GPL components are a non-starter in a shipped macOS product; openrsync also proves the protocol subset that matters is small [18].

### 5. rclone bisync

- **What:** "rsync for cloud storage" CLI with ~70 backends; current **v1.75.2** (Oct 9, 2026) [19].
- **License:** `MIT` [20].
- **Platforms:** Windows, macOS, Linux, BSDs [19].
- **Sync model:** `rclone bisync` does bidirectional sync between any two paths (local, SMB, SFTP, cloud) by comparing both listings against stored Path1/Path2 snapshots from the prior run [21].
- **Change detection:** Listing diffs (modtime/size/hash depending on backend); run on-demand or via cron — no real-time watcher [21].
- **Conflict handling:** Detects "changed on both" via its state files; conflicts must be resolved by flags (`--resync`, `--resync-mode path1|path2|newer|...`); `--backup-dir` saves replaced files [21].
- **Standout:** Backend abstraction — one engine speaks SMB/SFTP/S3/local uniformly; docs openly warn it's an "advanced command" where misuse loses data [21].
- **Mac UX:** CLI only.
- **Price:** Free (donations) [19].
- **Lesson for Foldover:** Listing-snapshot reconciliation is a simple, proven two-way design that needs no agent on the far side — ideal for NAS/SFTP endpoints where Foldover can't run its own peer [21].

### 6. Resilio Sync

- **What:** Proprietary P2P sync (formerly BitTorrent Sync); latest **3.1.2.1076** (Oct 31, 2025) [22].
- **License:** Proprietary freeware; since Sync 3.0 (Aug 2024), all Pro features free for **non-commercial use with registration**; Business line deprecated in favor of Active Everywhere [22][23].
- **Platforms:** Windows, macOS, Linux, Android, iOS, FreeBSD, many NAS packages (though 3.0 deprecated ARMv5/v7 NAS builds) [22][changelog].
- **Sync model:** True peer-to-peer mesh over a modified BitTorrent protocol; LAN and WAN (relays/trackers); read-write/read-only/encrypted-folder key model [22].
- **Change detection:** Always-on sync with fast propagation; archived replaced/deleted versions kept in `.sync/Archive` (30 days on desktop) [26].
- **Conflict handling:** Filename/case conflicts become `name.Conflict.ext` copies; content conflicts resolved last-writer-wins with the loser archived — manual cleanup required [25].
- **Mac UX:** Native-ish desktop app plus WebUI; metadata (xattrs/resource forks) handled via a `StreamsList` whitelist inside `.sync` [27].
- **Price:** Free for non-commercial (account required); Sync Home Pro $59.90 and Family Pro $99.90 one-time; Business from $35/mo [24].
- **Lesson for Foldover:** The Archive-of-overwritten-versions pattern is a cheap, loved safety net; conversely, mandatory account registration for a LAN tool is a friction Foldover can win on [23][26].

### 7. Carbon Copy Cloner 7

- **What:** Native Mac backup/clone utility from Bombich; current **7.2.2** (Oct 15, 2026); CCC 7 shipped May 9, 2024 [28][31].
- **License:** Proprietary.
- **Platforms:** macOS only (13.1+ through macOS 27 "Golden Gate") [32].
- **Sync model:** One-way backup/copy tasks — local disks, NAS mounts (it mounts shares itself), bootable clones, APFS snapshot versioning, SafetyNet retention of replaced files [28][31].
- **Change detection:** "Quick Update" uses **FSEvents** — but the 7.2.2 notes state explicitly this is "limited to destination volumes that support FSEvents, so NAS volumes are not included"; NAS destinations need full audits [29].
- **Conflict handling:** N/A (one-way); SafetyNet + snapshots protect against deletion mistakes [31].
- **Standout:** Backup Volume Setup Assistant, per-snapshot retention/search, verification reports; the reference for "Mac-native backup UX" [31].
- **Price:** $49.99 household license, one-time; upgrade discounts from CCC 5/6 [30].
- **Lesson for Foldover:** Even the best Mac tool admits FSEvents blind spots on NAS — Foldover needs its own remote-side truth (agent, index, or scheduled rescan) rather than pretending watchers cover shares [29].

### 8. SuperDuper! 4

- **What:** Shirt Pocket's bootable-clone backup app, ground-up rewrite; current **4.0.10** (Oct 8, 2026), macOS 14+ [33].
- **License:** Proprietary.
- **Platforms:** macOS only [33].
- **Sync model:** One-way full or Smart Update (copies only changed files) to local/external drives; scheduling, pre/post scripts [33][35].
- **Change detection:** Smart Update differential copying; no remote/NAS awareness focus — it copies to local drives (an external drive is the documented target) [33].
- **Conflict handling:** N/A (one-way).
- **Standout:** Dead-simple UX that survived 22 years; **free unregistered tier does complete erase-and-copy clones forever** — paid unlocks Smart Update, scheduling, unattended runs [35].
- **Price:** $43.95 per-user one-time (all your personal Macs); first paid upgrade ever, up to 30% off for recent v3 buyers [34][35].
- **Lesson for Foldover:** "Basic copy free, smart sync paid" is a proven indie Mac pricing ladder, and per-user household-style licensing is the Mac norm [34][35].

### 9. Beyond Compare 5

- **What:** Scooter Software's legendary diff/merge tool; current **5.2.6** build 32774 (Sep 24, 2026) [36].
- **License:** Proprietary.
- **Platforms:** Windows, macOS, Linux — one key covers all [36][37].
- **Sync model:** Interactive folder-compare sessions where you run update/mirror/copy actions in either direction — human-in-the-loop two-way; supports FTP/SFTP/WebDAV/S3 profiles alongside mounted paths [36].
- **Change detection:** On-demand comparison by rules (content, CRC, date, size); no watcher.
- **Conflict handling:** Fully manual — you see both sides and pick per item.
- **Standout:** The gold standard for *see-every-difference-then-act* UX; per-file overrides beat automation when stakes are high.
- **Mac UX:** Solid but Qt-flavored rather than AppKit-native.
- **Price:** Standard $35 / Pro $70 per user one-time; minor updates free forever, major upgrades ~50% [37].
- **Lesson for Foldover:** Its session model (compare → selective apply) is what a "manual mode" of Foldover should feel like; nobody regrets seeing the plan before execution [36][37].

### 10. Mutagen

- **What:** Continuous bidirectional sync for remote development (code to SSH hosts/containers); latest tag **v0.18.1** (release notes bump to Go 1.21.6 ⇒ early-2024 era — exact date UNVERIFIED); 0.x pre-1.0 [38].
- **License:** `MIT` core **plus** `SSPL-1.0` code under `sspl/` (fanotify sidecar, API libs, Pro features); official release builds include SSPL code since v0.17 — build without `--sspl` for MIT-only [39][40].
- **Ownership:** Acquired by Docker, Inc. for Docker Desktop; Mutagen Pro subscriptions discontinued [40][41].
- **Platforms:** macOS/Linux/Windows clients; endpoints = local dirs, SSH hosts, Docker containers [38][freshports].
- **Sync model:** Daemon + agent session model; near-real-time two-way (alpha↔beta) sync with configurable conflict modes — aimed at code, not general files [38].
- **Change detection:** Filesystem watching + reconciliation loop in the session daemon [38].
- **Mac UX:** CLI only, developer audience.
- **Price:** Free OSS core.
- **Lesson for Foldover:** The daemon+agent split and per-session config are worth studying architecturally — and its MIT/SSPL split shows how to keep an embeddable open core while holding back paid surface [39][40].

### 11. Transmit 5 (File Sync)

- **What:** Panic's flagship Mac file-transfer client (FTP/SFTP/S3/Drive/etc.) with a built-in Synchronize feature; current **5.11.7** (Oct 1, 2026), requires macOS 13+ [44][45].
- **License:** Proprietary.
- **Platforms:** macOS only.
- **Sync model:** Directional sync (you pick left→right or right→left) between **any** two folders — local↔remote, local↔local, remote↔remote; schedulable via Automator [42].
- **Change detection:** On-demand comparison when you run sync; preview list before transfer [42].
- **Conflict handling:** Direction + options decide; it's an explicit per-run operation, not continuous.
- **Standout:** Sync folded into a beloved dual-pane file browser — the "file manager that also syncs" pattern [42][44].
- **Mac UX:** Best-in-class native.
- **Price:** $45 one-time direct, or $24.99/yr via Mac App Store subscription [43].
- **Lesson for Foldover:** Sync as a verb inside a file browser is proven Mac UX; but it's manual and one-directional — continuous two-way is the empty seat [42][43].

### 12. ForkLift 4 (sync)

- **What:** BinaryNights' dual-pane file manager + transfer client; current **4.5.1** (Mar 10, 2026) [49].
- **License:** Proprietary.
- **Platforms:** macOS only, fully native rewrite [50].
- **Sync model:** Compares any two locations (local folders, drives, remote FTP/SFTP/etc.), then syncs **one- or two-way**, savable as a favorite; claims up to 20× faster analysis than v3 [46][47].
- **Change detection:** On-demand compare (matching/modified/new/deleted view); no background watcher — sync is a button, not a daemon [46][47].
- **Conflict handling:** Preview grid + user-chosen direction/items; manual.
- **Mac UX:** Genuinely native, modern.
- **Price:** Single user $19.95 (1 yr updates) / $34.95 (2 yr); family $29.95/$49.95; small business $69.95/$119.95 — perpetual license, paid update window [48][50].
- **Lesson for Foldover:** Users already understand "compare panes → sync"; Foldover can steal that mental model and add the always-on layer ForkLift lacks [46][47].

### 13. Hazel

- **What:** Noodlesoft's folder-watching automation engine (rules → move/rename/tag/trash/OCR…); current **6.1.2** (Feb 18, 2026); Hazel 6 needs macOS 13+ per FAQ (release notes say 12+) [51][52].
- **License:** Proprietary.
- **Platforms:** macOS only; native (System Settings pane + app) [51].
- **Sync model:** Not a sync tool — watches folders and applies rules; included here because it's *the* reference for "when files change, act on them" on macOS [53].
- **Change detection:** Continuous folder monitoring drives rule evaluation [53].
- **Conflict handling:** N/A.
- **Standout:** A rule engine non-programmers actually use; Hazel 6 adds on-the-fly OCR, revert-file, pause actions [53].
- **Price:** $42 single / $65 family / $20 upgrade, one-time [51].
- **Lesson for Foldover:** Pair "sync" with "rules" — e.g. auto-organize after arrival, or conflict policy expressed as Hazel-style rules — and you get Hazel's power users as evangelists [53].

### 14. Arq 7

- **What:** Encrypted, deduplicated, versioned **backup** (not sync) to your own storage; current Mac **7.50** (Oct 6, 2026); macOS 12+ [54][55].
- **License:** Proprietary.
- **Platforms:** macOS and Windows [54].
- **Sync model:** One-way backup plans to external drives, **NAS shares (SMB), SFTP**, S3-class clouds, etc.; retention/budget policies [54].
- **Change detection:** FSEvents-driven scans with a periodic forced full scan (v7.49 fixed the weekly full scan never running) [55].
- **Conflict handling:** N/A — versioned backup records.
- **Standout:** Proves users pay $60/computer for set-and-forget backup onto plain NAS shares; opaque-data logging and budget enforcement are nicely engineered [55].
- **Price:** $59.99 per computer one-time incl. 12 months updates; $25/yr/computer for continued updates (price rose from $49.99 on Sep 1, 2026) [54][56][price-rise].
- **Lesson for Foldover:** Destination-agnostic storage (folder/NAS/SFTP all look alike to the engine) plus FSEvents+periodic-audit is the architecture users already trust [54][55].

### 15. Time Machine → SMB NAS

- **What:** Built-in macOS backup; backs up to local disks, another Mac's shared folder, or **NAS devices that advertise Time Machine over SMB** (Bonjour broadcast or manual `smb://` mount) [57][58].
- **License:** Proprietary, bundled with macOS.
- **Platforms:** macOS.
- **Sync model:** One-way versioned backup (hourly increments); restore via Finder integration.
- **Change detection:** Incremental scans driven by filesystem change tracking (FSEvents/local snapshots) — standard documented behavior.
- **Conflict handling:** N/A.
- **Standout:** Zero-cost, zero-config baseline every Mac user already has; note **AFP is unsupported in macOS 27 — SMB is now the only network path** [57].
- **Price:** Free with macOS.
- **Lesson for Foldover:** Don't fight Time Machine for "backup to NAS" — it owns that. Foldover's wedge is *live folder sync* (TM is restore-oriented, shares nothing back, needs a TM-aware server) [57][58].

### 16. SyncTime

- **What:** Desairem's (Nicolas Kick) indie Mac sync app; changelog shows 5.5.x series (5.5 shipped May 9, 2026; 5.5.1 current) [61].
- **License:** Proprietary.
- **Platforms:** macOS 10.13+, Apple Silicon + Intel [59][60].
- **Sync model:** Per-item pairs — any folder reachable in Finder (local disk, external drive, remote server mount); **one-way, two-way (merge), and one-way move**; multiple sources can virtually merge into multiple destinations; Shortcuts/AppleScript/schedules/dependencies [59].
- **Change detection:** "Files on your Mac are monitored for changes in real time" (FSEvents-class watching), plus scheduled/auto-on-availability syncs [59].
- **Conflict handling:** Overwrite policies (newer/different/always/never) + optional move-to-Trash backup of overwritten files; two-way exists but the documented conflict model is thin — essentially policy-driven, no per-file review UI documented [59].
- **Standout:** Closest existing match to Foldover's MVP surface (local+mounted-remote pairs, real-time, cheap); moved/renamed file tracking [59].
- **Price:** $22.99 one-time on the Mac App Store; SyncTime Lite free (settings not persisted) [60].
- **Lesson for Foldover:** There's proven demand for a simple "any two folders" Mac sync tool at ~$23 — Foldover's differentiation must be LAN peers, real two-way conflict handling, and NAS-awareness that SyncTime's mount-based model can't reach [59].

### 17. DuoBolt

- **What:** Qoncept Apps' duplicate-finder + folder-sync utility; current **2.3.5** (Oct 13, 2026); macOS 11+ Universal and Windows 10/11 [62].
- **License:** Proprietary freemium.
- **Platforms:** macOS, Windows [62].
- **Sync model:** "Sync Folders" compares two folders **by content** (BLAKE3 after size + head/tail prehash), shows every difference side-by-side, then syncs one way or **both ways** [62][64].
- **Change detection:** On-demand content comparison plus a stored record of the last sync to tell which side changed [62].
- **Conflict handling:** A file changed on both sides "waits for a decision rather than being overwritten by the newer date" — explicit, conservative conflict UX [62].
- **Standout:** Content-hash truth (names/dates don't matter) + reviewable queue; also does duplicate/photo-similarity via the same engine [62].
- **Price:** Free tier (25 files / 500 MiB per operation); Standard $34.99, Pro $64.99 — one-time, 2 seats, no account [63].
- **Lesson for Foldover:** "Conflict waits for a human" + content-compare is the credible small-tool bar; DuoBolt shows there's appetite for sync wrapped in a polished, priced-once utility [62][63].

---

## Comparison table

| Tool | License | Two-way? | Real-time watching? | Preview before sync? | LAN peer? | SMB/NAS-aware? | Native Mac UI? | Price model |
|---|---|---|---|---|---|---|---|---|
| Syncthing (+ macOS wrap) | MPL-2.0 / MIT | ✅ mesh | ✅ watcher+hourly scan | ❌ auto-apply | ✅ BEP+LAN discovery | ⚠️ local paths only (mounts OK) | ⚠️ web UI; frugal tray wrapper | Free OSS |
| Unison | GPL-3.0+ | ✅ pairwise | ❌ manual/cron | ✅ reconciliation list | ⚠️ SSH host pair | ⚠️ any path/SSH | ⚠️ aging Cocoa GUI (untested); 3rd-party Swift GUI | Free OSS |
| FreeFileSync + RTS | GPL-3.0 (binaries: personal-use) | ✅ via sync.ffs_db | ✅ RealTimeSync (unreliable on shares) | ✅ compare grid | ❌ | ✅ network paths/SFTP/FTP/MTP | ❌ wxWidgets | Free personal; paid Business |
| rsync / openrsync | GPL-3.0+ / ISC | ❌ | ❌ | ⚠️ --dry-run | ⚠️ SSH/daemon | ⚠️ mounts/paths | ❌ CLI | Free OSS |
| rclone bisync | MIT | ✅ snapshot-diff | ❌ cron/manual | ⚠️ --dry-run | ❌ (remote backends) | ✅ smb+sftp+70 backends | ❌ CLI | Free OSS |
| Resilio Sync | Proprietary | ✅ P2P mesh | ✅ always-on | ❌ auto-apply | ✅ LAN+WAN P2P | ⚠️ folders on installed peers; NAS pkgs | ⚠️ desktop app/WebUI | Free non-commercial (account); $59.9–$99.9 one-time |
| Carbon Copy Cloner 7 | Proprietary | ❌ one-way backup | ⚠️ FSEvents (local vols only) | ✅ task preview/audit | ❌ | ✅ mounts NAS, AFP→SMB warnings | ✅ | $49.99 one-time |
| SuperDuper! 4 | Proprietary | ❌ | ❌ scheduled | ⚠️ script/log preview | ❌ | ❌ local/external disks | ✅ | $43.95 one-time (free tier: full copies) |
| Beyond Compare 5 | Proprietary | ✅ manual | ❌ | ✅ | ❌ | ⚠️ mounts + SFTP/FTP/cloud profiles | ⚠️ Qt | $35/$70 one-time |
| Mutagen | MIT + SSPL parts | ✅ sessions | ✅ near-real-time | ❌ auto | ⚠️ SSH/agent endpoints | ⚠️ SSH endpoints | ❌ CLI | Free OSS |
| Transmit 5 | Proprietary | ⚠️ one direction per run | ❌ | ✅ sync sheet | ❌ | ✅ FTP/SFTP/S3 + local | ✅ | $45 one-time / $24.99 yr MAS |
| ForkLift 4 | Proprietary | ✅ | ❌ | ✅ compare panes | ❌ | ✅ remote conns + local | ✅ | $19.95+ one-time w/ update window |
| Hazel 6 | Proprietary | ❌ (automation) | ✅ folder watch | ⚠️ rule preview | ❌ | ⚠️ any watched folder | ✅ | $42/$65 one-time |
| Arq 7 | Proprietary | ❌ backup | ⚠️ FSEvents + periodic full scan | ⚠️ plan/backup stats | ❌ | ✅ NAS shares/SFTP/disks | ✅ | $59.99/computer + $25/yr updates |
| Time Machine → SMB | Proprietary (bundled) | ❌ backup | ⚠️ incremental hourly | ❌ | ⚠️ Mac-as-destination only | ✅ TM-aware SMB NAS | ✅ | Free (macOS) |
| SyncTime | Proprietary | ✅ merge mode | ✅ real-time (local) | ⚠️ UNVERIFIED | ❌ | ⚠️ any Finder-reachable mount | ✅ | $22.99 one-time / Lite free |
| DuoBolt | Proprietary | ✅ both-ways | ❌ on-demand | ✅ every diff first | ❌ | ⚠️ any folder/drive | ✅ | Free tier / $34.99 / $64.99 one-time |

---

## Gaps in the market

What **nobody** currently does well for Mac users syncing local ↔ NAS ↔ LAN:

1. **Remote-side change detection is universally unsolved.** FSEvents only covers local volumes — CCC's own release notes admit Quick Update self-correction is "limited to destination volumes that support FSEvents, so NAS volumes are not included" [29]. FreeFileSync's manual warns network paths often deliver *no* change notifications at all [13]. Syncthing's watcher likewise only sees its local filesystem, so edits made on the NAS or peer wait for the next periodic rescan [3]. Every tool falls back to polling; none ships a NAS-side agent story usable by normal Mac users (Resilio/Syncthing can run *on* some NASes, but that's installing an OS package — 3.0 even deprecated most ARM NAS builds [changelog]).
2. **"Continuous sync with a preview/review surface" doesn't exist.** The always-on tools (Syncthing, Resilio, Mutagen) apply changes silently and expose conflicts only as renamed files after the fact [3][25]. The preview-first tools (FreeFileSync grid, Beyond Compare, ForkLift, DuoBolt) are all manual/scheduled and have no peer model. Foldover can own the intersection: live sync + a reviewable change plan + conflict queue.
3. **Mac-to-Mac LAN sync without an account or a web console is empty.** The only real LAN-peer options are Syncthing (works, but config lives in a web UI; the official wrapper is deliberately "frugal" — menus and notifications only [5]) and Resilio (requires registration even for free non-commercial use [23]). Nothing offers Bonjour-discover → pair → sync inside a fully native UI.
4. **True two-way semantics are scarce on the Mac.** Unison does real reconciliation but is CLI/GTK-dated, and its own release notes warn the Mac GUI is unverified and the GTK GUI may be dropped any release [7]. FreeFileSync's two-way mode is real but wx-flavored and its binaries are personal-use-only [14]. SyncTime advertises "two-way merge" but documents only overwrite policies — no conflict model [59]. Transmit/ForkLift sync is pick-a-direction [42][46].
5. **Conflict UX is bimodal: silent-auto or fully-manual.** Syncthing renames to `sync-conflict-*` and propagates the rename everywhere [3]; Resilio archives the loser + `.Conflict` suffixes for name clashes [25][26]; Unison/FFS/BC make you click per file. Almost nothing offers "sensible default + reviewable conflict queue" — DuoBolt's "waits for a decision" is the small-tool hint [62].
6. **macOS metadata fidelity is uneven.** Resource forks/xattrs need a `StreamsList` whitelist in Resilio [27]; Syncthing warns about case-sensitivity clashes on case-insensitive Mac volumes [3]; FFS's move detection fails on SFTP and FAT [12]; Unison's rename detection is experimental (`-moves-experimental`) [7]. APFS-native handling (clones, tags, ACLs, quarantine) across SMB/NFS is open territory.
7. **A permissively-licensed sync engine for macOS doesn't exist.** Apple itself swapped rsync for ISC-licensed openrsync rather than ship GPLv3 [18]. The embeddable options are GPL (Unison/FFS), MPL-in-Go (Syncthing), or dev-tool-shaped (Mutagen). An Unlicense Rust core with a commercial-friendly license story is itself differentiating.

---

## Reusable components for Foldover

License-compatibility assessment for an **Unlicense (public-domain-equivalent) repo that may ship a commercial binary**. (Not legal advice; LGPL/static-linking subtleties flagged.)

| Component | License | Verdict for Foldover |
|---|---|---|
| **Syncthing BEP spec** | Protocol spec (docs.syncthing.net) | ✅ **Interoperate freely** — implementing a documented wire spec carries no license obligation. BEP = TLS 1.3 + protobuf ClusterConfig/Index/Request/Response over block lists (128 KiB–16 MiB, SHA-256) [4]. A Rust reimplementation could speak to Syncthing peers. Syncthing *code* is MPL-2.0 — weak file-level copyleft, technically linkable [1], but it's Go; spec-driven reimplementation is the clean path. |
| **librsync** | `LGPL-2.1-or-later` [65] | ⚠️ **Linkable with care** — dynamic linking into a commercial binary is permitted, but you must ship librsync's source + your modifications and allow relinking. Static linking raises obligations. Simpler Rust alternative: `fastcdc` (MIT) for content-defined chunking [70], or write a small rolling-checksum module — the algorithm (Rabin fingerprints) is public. |
| **openrsync** | `ISC` [17] | ✅ **Reusable/vendored** — permissive C implementation of the rsync protocol subset; Apple ships it in macOS [18]. Useful as reference or even as a fallback transport target (`--rsync-path` interop). |
| **rsync** (upstream) | `GPL-3.0-or-later` + OpenSSL exception [15][16] | ❌ **Cannot link or ship** in a commercial binary. Interop at arm's length only: invoking a user-installed `rsync` binary as a subprocess is the conventional boundary (widely practiced; the FSF's derivative-work line is debated — treat as a gray area, prefer not bundling). |
| **Unison** | `GPL-3.0-or-later` [7][8] | ❌ **Code unusable; ideas reusable (clean-room).** Its contribution is conceptual: persistent last-sync archive, explicit reconciliation list, manual conflict resolution [8]. Reimplement semantics, not code. Optional `unison` subprocess integration = same GPL boundary caveat as rsync. |
| **FreeFileSync** | `GPL-3.0` source; binaries restricted (personal use) [11][14] | ❌ **Inspire only.** Reuse the *design patterns* — `sync.ffs_db` state file for two-way direction detection [12], RealTimeSync's idle-time debounce [13], compare-grid preview — none of which are license-bound as ideas. Don't touch the code; note its binaries carry extra use restrictions beyond GPL [14]. |
| **rclone** | `MIT` [20] | ✅ **Fully reusable.** MIT permits vendoring/linking into commercial binaries. Two practical paths: `librclone` C-ABI bindings (it's Go) for instant SMB/SFTP/S3 backends, or study its bisync reconciliation model and reimplement the small local+SFTP+SMB subset natively in Rust [21]. |
| **Mutagen** | `MIT` core + `SSPL-1.0` subtrees [39][40] | ⚠️ **Partial.** MIT-licensed core is reusable; everything under `sspl/` (fanotify sidecar, API client libs, ex-Pro features) must be excluded — SSPL is non-OSI and hostile to embedding. Official binaries include SSPL since v0.17; MIT-only requires building without `--sspl` [39]. As with other Go tools, its daemon+agent session architecture is the valuable (borrowable) idea. |
| **Rust crates (verified)** | — | ✅ All permissive: `notify` — **CC0-1.0**, FSEvents/kqueue/inotify watching, the standard watcher crate [66]; `russh` + `russh-sftp` — **Apache-2.0**, pure-Rust SSH incl. SFTP client *and* server [67]; `blake3` — **CC0-1.0 OR Apache-2.0 OR Apache-2.0+LLVM-exception**, fast content hashing [68]; `iroh` — **MIT OR Apache-2.0**, QUIC + NAT hole-punching + public-key dialing + encrypted relays — a nearly turnkey LAN/WAN peer layer [69]; `fastcdc` — **MIT**, content-defined chunking for delta/dedup [70]. |
| **SMB/NFS access** | — | Pragmatic route used by CCC/FFS/SyncTime/Arq: let **macOS mount** SMB/NFS (`mount_smbfs`/NetFS/share-connection APIs) and operate on the mount, while treating remote-side state via your own index — native Rust SMB clients are still immature (UNVERIFIED, engineering judgment). For NFS there's no equivalent mature Rust crate either; `libsmb2` (LGPL-2.1) is the fallback C library if a bundled client is ever needed. |

**Bottom line for licensing:** MIT/Apache/CC0/ISC/MPL-2.0 components are all compatible with an Unlicense repo *and* a commercial binary (keep license texts; MPL keeps its own files under MPL). LGPL-2.1 is usable only via dynamic linking. GPL-3.0 code (rsync, Unison, FreeFileSync) cannot be shipped or linked — implement ideas cleanly or shell out to user-installed binaries, documented as an optional integration. SSPL subtrees in Mutagen: avoid.

---

## Sources

[1] https://github.com/syncthing/syncthing
[2] https://github.com/syncthing/syncthing/releases and https://syncthing.net/downloads/
[3] https://docs.syncthing.net/users/syncing.html
[4] https://docs.syncthing.net/specs/bep-v1.html
[5] https://github.com/syncthing/syncthing-macos
[6] https://github.com/syncthing/syncthing-macos/releases
[7] https://github.com/bcpierce00/unison/releases
[8] https://en.wikipedia.org/wiki/Unison_(software)
[9] https://github.com/bcourbage/unison-ui-mac/ and https://bcourbage.github.io/unison-ui-mac/
[10] https://freefilesync.org/download.php and https://freefilesync.org/archive.php
[11] https://en.wikipedia.org/wiki/FreeFileSync
[12] https://freefilesync.org/manual.php?topic=synchronization-settings
[13] https://freefilesync.org/manual.php?topic=realtimesync
[14] https://freefilesync.org/faq.php
[15] https://rsync.samba.org/
[16] https://github.com/macports/macports-ports/blob/master/net/rsync/Portfile and https://formulae.brew.sh/formula/rsync
[17] https://github.com/kristapsdz/openrsync
[18] https://derflounder.wordpress.com/2025/04/06/rsync-replaced-with-openrsync-on-macos-sequoia/
[19] https://github.com/rclone/rclone/releases
[20] https://rclone.org/licence/
[21] https://rclone.org/commands/rclone_bisync/
[22] https://en.wikipedia.org/wiki/Resilio_Sync
[23] https://help.resilio.com/hc/en-us/articles/31116248751123-Licensing-in-Resilio-Sync-3-0 and https://www.resilio.com/blog/resilio-sync-3-0
[24] https://www.resilio.com/individuals/
[25] https://help.resilio.com/hc/en-us/articles/204753629-Conflict-files-in-Sync
[26] https://help.resilio.com/hc/en-us/articles/204754239-Using-Archive-for-file-versioning-and-restoring-deleted-files
[27] https://help.resilio.com/hc/en-us/articles/206217185-What-is-sync-folder-and-StreamsList-IgnoreList-and-Archive-inside
[28] https://support.bombich.com/hc/en-us/articles/20686469389719-CCC-7-Release-Notes
[29] https://bombich.com/doc-pdf/ccc7-documentation-en.pdf (CCC 7.2.2 notes)
[30] https://support.bombich.com/hc/en-us/articles/20686511201943-How-much-does-CCC-cost-and-how-can-I-purchase-it
[31] https://bombich.com/ and https://bombich.com/press
[32] https://bombich.com/download
[33] https://www.shirtpocket.com/superduper4.php
[34] https://tidbits.com/2026/08/03/superduper-4s-radical-new-interface-explains-itself-as-you-use-it/
[35] https://www.shirtpocket.com/blog/supererduperer
[36] https://www.scootersoftware.com/download
[37] https://www.scootersoftware.com/shop and https://www.scootersoftware.com/kb/licensing
[38] https://github.com/mutagen-io/mutagen and https://github.com/mutagen-io/mutagen/releases
[freshports] https://www.freshports.org/devel/mutagen/
[39] https://github.com/mutagen-io/mutagen/blob/master/LICENSE
[40] https://mutagen.io/blog/building-a-robust-future-for-mutagen/
[41] https://www.docker.com/blog/mutagen-acquisition/
[42] https://help.panic.com/transmit/transmit5/synchronize/
[43] https://help.panic.com/transmit/transmit5/mac-app-store/
[44] https://www.panic.com/transmit/
[45] https://help.panic.com/releasenotes/transmit5/
[46] https://binarynights.com/manual
[47] https://binarynights.com/
[48] https://binarynights.com/store
[49] https://binarynights.com/versionhistory
[50] https://blog.binarynights.com/2023/09/05/introducing-forklift-4/ and https://blog.binarynights.com/2025/11/19/forklift-4-winter-sale-50-off-for-a-limited-time/
[51] https://www.noodlesoft.com/kb/hazel-6-faq/
[52] https://www.noodlesoft.com/release_notes
[53] https://www.noodlesoft.com/manual/hazel/whats-new-in-hazel/
[54] https://cloud.arqbackup.com/purchase
[55] https://arqbackup.com/download/arqbackup/arq7_release_notes.html
[56] https://www.arqbackup.com/documentation/arq7/English.lproj/arq7Pricing.html
[price-rise] https://www.bestbackupreviews.com/bits-and-bytes/beat-the-arq-7-price-increases/
[57] https://support.apple.com/en-us/102423
[58] https://support.apple.com/guide/mac-help/back-up-to-a-shared-folder-mchl31533145/mac
[59] https://apps.apple.com/us/app/synctime/id590386474
[60] https://appagg.com/macos/utilities/synctime-5088012.html and https://desairem.com/wordpress/synctime/synctime-pricing/
[61] https://desairem.com/wordpress/category/synctime-changelog/
[62] https://duobolt.app/press/
[63] https://duobolt.app/pricing/
[64] https://duobolt.app/sync-folders/
[65] https://github.com/librsync/librsync and https://librsync.github.io/
[66] https://crates.io/crates/notify
[67] https://crates.io/crates/russh and https://docs.rs/crate/russh/latest
[68] https://crates.io/crates/blake3
[69] https://crates.io/crates/iroh and https://github.com/n0-computer/iroh
[70] https://crates.io/crates/fastcdc
[changelog] https://help.resilio.com/hc/en-us/articles/31386579044755-Resilio-Sync-3-0-change-log
