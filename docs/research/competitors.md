# Competitor review: GoodSync, ChronoSync, SyncMate

Researched 2026-10-16. Load-bearing facts (versions, prices, features) were
independently fact-checked; user quotes and anecdotes come from the reports.
*(unchecked)* marks the rest. Detail and per-claim sources:
[GoodSync](sources/goodsync.md) · [ChronoSync](sources/chronosync.md) ·
[SyncMate](sources/syncmate.md) · [user sentiment](sources/user-sentiment.md) ·
[wider landscape](sources/landscape.md).

## At a glance

| | GoodSync | ChronoSync | SyncMate |
|---|---|---|---|
| Vendor | Siber Systems (RoboForm) | Econ Technologies (since 2002) | Electronic Team (ex-Eltima) |
| Latest | 12.11.9, 2026-10-06 | 12.0.3, 2026-09-14 | 8.11.604, 2026-08-27 |
| Platforms | Win, Mac, Linux, NAS, iOS, Android | Mac only | Mac only (syncs to phones) |
| Core idea | Cross-platform job engine, any protocol | Mac-native task documents, full metadata fidelity | Device hub: Macs, phones, drives, accounts |
| Sync | Two-way + one-way backup | Backup, Mirror, Blind Backup, Bidirectional, Bootable, Data Volume | One-way or two-way pairs |
| Peer (machine-to-machine) | GoodSync Connect (GSTP, own P2P protocol) | ChronoAgent ($19.99 per Mac) | SyncMate on both Macs |
| Price | $29.95/yr subscription, 5 devices | $49.99 one-time, free updates for life | Free tier; Expert from $39.95 one-time |
| Distribution | Direct | Direct; Express $29.99 on the Mac App Store, also in Setapp | Direct |
| UI | Custom cross-platform (shared HTML dialogs) | Native AppKit, dense | Native AppKit, dated wizard style |

## GoodSync — strengths

1. **Endpoint breadth.** Local disks, its own SMB client (Sib-SMB),
   in-house SFTP, FTP/FTPS, WebDAV, AFP, MTP, about 15 clouds, plus its own P2P
   protocol. One tool covers the whole LAN and NAS.
2. **Real two-way semantics.** A persistent state file per synced folder
   (`_gsdata_/_file_state._gs`) makes these work: deletion propagation, chained
   sync (A↔USB↔B), revert detection, move/rename detection,
   case-only renames, and FAT 2 s / DST time-shift tolerance.
3. **Editable plan before sync.** Analyze builds a sync tree. You can change
   any item's direction before Sync runs.
4. **Automation matrix.** Triggers: On File Change (20 s debounce), On Folders
   Connect (drive or share appears), periodic, cron-style schedule, and an
   unattended runner service. Also a full CLI (`gsync`), pre/post hooks and
   email alerts.
5. **Safety nets.** `_saved_` keeps the last version of anything replaced or
   deleted (30 days by default); `_history_` (optional) keeps every version.
   Copies go to a temp file and are renamed into place. Automatic two-way runs
   skip when more than 50 % of files changed. Lock files stop two machines
   syncing one folder at once.
6. **Conflict taxonomy.** Modified on both sides, delete-vs-change, case-only
   and metadata-only conflicts are separate types. Weak conflicts resolve
   automatically; strong ones wait for the user. The losing file can be kept
   as `name (Conflicted Copy from HOST on DATE).ext`.
7. **GoodSync Connect.** Block-level delta, resumable transfers, NAT traversal
   (UPnP → manual port forward → relay, brokered by a mediator service), and
   remote real-time change monitoring.

**Weaknesses:** non-native, dated, jargon-heavy UI (the most consistent
complaint over 15 years of reviews). Subscription plus a mandatory online
account. Reliability bugs ("Undefined Error: 0", lockups). `_gsdata_` folders
in every synced folder, which fail on some NAS shares. Weak support. No NFS.
Delta transfer only over GSTP, never SMB or SFTP.

## ChronoSync — strengths

1. **Mac metadata fidelity.** ACLs, xattrs, Finder tags, creation dates, BSD
   flags and resource forks are all preserved. Packages sync as units or as
   trees ("dissect"). An admin-access helper reaches other users' files.
2. **Trial Sync / Analyze.** Full preview with per-item override, exclude and
   compare. A Validate operation (schedulable in v12) checks targets without
   copying.
3. **Safe defaults.** Backup and Synchronize never propagate deletions on
   their first run. Replaced and deleted files go to `_Archived Items`, capped
   by copy count and age. A readiness check before each run blocks, warns or
   auto-fixes.
4. **Event-driven scheduler.** Volume mount, target or agent available, after
   launch, wake-to-run, battery-aware. New in v12: monthly-by-weekday,
   scheduled validation, and runs triggered from a remote ChronoAgent Mac.
5. **ChronoAgent.** A Mac-to-Mac agent: encrypted, root access, parallel
   channels, works with no user logged in. Econ says it scans 2–3× faster than
   file sharing.
6. **SmartScan.** Uses macOS fsEvents to speed up scans of local and
   ChronoAgent volumes (not file servers or removable media). The optional
   two-pass mode in v12 gives an accurate progress bar.
7. **Trust and value.** $49.99 with free updates since 2002, deep documentation,
   VoiceOver praise. MacUpdate 4.6/5. Macworld calls it the "ultimate Mac
   backup with complete control" (second in its roundup, after Time Machine).
8. **Defensive extras.** HoneyPotChecker, a downloadable pre-sync script: a
   decoy file is hash-checked before each sync, and the sync aborts if it
   changed (ransomware tripwire).

**Weaknesses:** complex, document-per-task UI with confusing operation names
(Blind Backup, Mirror vs Backup; older docs add TrueBlind). Progress lacks an
ETA unless two-pass is on. The good Mac-to-Mac path costs $19.99 per Mac. NAS
targets lose metadata and SmartScan. The sandboxed Express edition drops admin access,
scripts, SmartScan, parallel operations and resume. Bootable backups now come
with warnings (Apple Silicon, macOS 11+).

## SyncMate — strengths

1. **One hub for everything.** Other Macs, iOS, Android, MTP devices, mounted
   volumes and cloud or PIM accounts all sit in one sidebar.
2. **Zero-config Mac discovery.** Other Macs appear via Bonjour. Inbound policy:
   allow all, ask first, or allow-list, with an optional password.
3. **Readable pair table.** `this Mac | direction icon | other side | filter`,
   and three plain conflict policies: this side, other side, newest
   *(unchecked)*.
4. **Availability-triggered AutoSync.** Syncs when the other side shows up,
   every N minutes. Also a menu-bar/background mode.
5. **Simple licensing.** One-time purchase with multi-Mac family tiers. A free
   tier for contacts and calendars.

**Weaknesses:** severe reliability reports (25 TB filled by duplicates,
mangled folders). No file watching or persistent state, so both sides are
fully rescanned every time and syncs take hours. Sleep kills unattended runs.
Deletion semantics are undocumented. No conflict-copy option. Peer-protocol
encryption is undocumented. "All sales final" refunds.
Features moved into paid sibling apps. MacUpdate 3.4/5.

## Side by side

| Capability | GoodSync | ChronoSync | SyncMate | Foldover target |
|---|---|---|---|---|
| Persistent sync state | ✅ inside each synced folder | ✅ in task document | ❌ | ✅ in app data; roots hold only a small `.foldover/` (archive, ID, lock) |
| Preview + per-item override | ✅ | ✅ | ❌ | ✅ streamed, filterable |
| Real-time local watching | ✅ | ✅ (monitors targets for changes) | ❌ | ✅ FSEvents + periodic verify |
| Trigger on drive/share/peer appearing | ✅ | ✅ | ✅ | ✅ |
| Archive of replaced/deleted files | ✅ 30 days | ✅ copies + age | ❌ | ✅ retention policy |
| Mass-change fuse | ✅ 50 % | ❌ none documented | ❌ | ✅ |
| Conflict copies (keep both) | ✅ opt-in | ⚠️ pick a winner, loser archived | ❌ | ✅ + conflict center |
| Mac metadata fidelity | ⚠️ xattrs, ACLs | ✅ best in class | ⚠️ | ✅ + per-target warnings |
| Mac-to-Mac peer | ✅ GSTP | 💲 ChronoAgent | ✅ needs app both sides | ✅ built in, encrypted |
| SMB without OS mount | ✅ own client | ❌ | ❌ | later (spike) |
| SFTP | ✅ | ✅ | ❌ | ✅ |
| NFS | ❌ | ✅ via mount | ⚠️ any mounted volume | ✅ via mount |
| Native modern Mac UI | ❌ | ⚠️ dated | ⚠️ dated | ✅ SwiftUI + AppKit |
| No subscription / no account | ❌ | ✅ | ✅ | ✅ |

## What users say

From forums, reviews and app stores ([details](sources/user-sentiment.md)):

- **Most-praised feature everywhere:** seeing the plan before anything moves
  (FreeFileSync, ChronoSync Trial Sync, ForkLift, DuoBolt).
- **Hottest pain:** macOS SMB itself: hangs, `-36`/`-50` errors, dropped
  shares, small-file slowness. Every tool that rides OS mounts inherits it.
- **Unmet needs:** Mac-to-Mac LAN sync without cloud or account, sync on drive
  plug-in, honest conflict handling, fast scans of big trees, native UI.
- **Pricing:** one-time $30–50 is the comfort zone. GoodSync's move to a
  subscription is the category's cautionary tale.
- **Reliability is the brand:** every horror story pairs a sync failure with
  an unanswered support ticket.

## Market gaps (all tools surveyed)

From [the landscape survey](sources/landscape.md) of 17 tools (Syncthing,
Unison, FreeFileSync, rclone, Resilio, CCC, Transmit, ForkLift, SyncTime,
DuoBolt and others):

1. **Continuous sync plus a reviewable plan:** always-on tools apply changes
   silently; preview-first tools are manual or scheduled.
2. **Native Mac-to-Mac LAN sync without an account or web console:** today
   the choice is Syncthing (web UI) or Resilio (registration required).
3. **Remote-side change detection:** FSEvents never sees other clients'
   changes on a NAS, so everyone polls.
4. **Conflict UX is all-or-nothing:** silent auto-resolution or per-file
   clicking. Nothing offers sensible defaults plus a review queue.
5. **No permissively licensed Mac sync engine:** the open engines are GPL
   (Unison, FreeFileSync) or Go and MPL (Syncthing).

## Implications for Foldover

- **Match:** GoodSync's two-way semantics, conflict taxonomy and trigger
  matrix; ChronoSync's metadata fidelity, first-run safety, archive and
  readiness checks; SyncMate's discovery and pair-table clarity.
- **Beat:** native modern UI, preview as the hero screen, a free built-in
  encrypted peer mode, the sync database kept out of synced folders, honest
  per-target fidelity warnings, no subscription and no account.
- **Avoid:** cross-platform UI toolkits on the Mac, jargon operation names,
  per-peer upsells, silent deletion semantics, and full-rescan designs.
