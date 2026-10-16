> Raw AI research report (devin, SWE-2 max), 2026-10-16. Unedited. Corrections: [fact-checks](../README.md#corrections).

# SyncMate (Electronic Team, Inc.) — Deep-Dive Report

*Research date: 2026-10-16. Target: Feed design input into "Foldover", a new macOS-native file-sync app (Swift/SwiftUI/AppKit UI over a portable Rust core) whose MVP covers local disks, NAS/network shares (SMB, NFS, AFP), SFTP/SSH and Mac-to-Mac LAN peers.*

---

## 1. Editions, version, pricing, licensing, requirements

### Editions
SyncMate ships in two editions [1][2][3]:

- **Free Edition**: syncs **Contacts**, **Calendars**, and **background sync** (sync runs silently without opening the GUI). MacUpdate's overview additionally notes the Free edition can *read* SMS from Android/iOS on the Mac [27].
- **Expert Edition** (paid): everything in Free plus **Folders**, **Photos**, **iTunes/Apple Music**, **AutoSync**, **Mount Disk**, **SMS management**, **Safari bookmarks**, **Call History**, **Reminders**, and **Backup** [1][3].

Feature availability is per-connection-type ("sync options may differ for each supported device or account") [2][31].

### Current version
- **v8.11.604, released 27 Aug 2026** [2][5][6]. Changelog: "optimized for Macs with Apple silicon", improved Google Drive / OneDrive / Android sync, fixed iCloud contacts sync, **macOS 12.4+ now required** [6].
- Release cadence is roughly 1–2 releases/year lately: 8.10 (Jan 2026, macOS Tahoe compat), 8.9 (Feb 2025), 8.8 (Jun 2024), 8.7 (Dec 2023), 8.6 (Oct 2022)… back through 8.0 (Nov 2019) and a long 7.x line [5][6].
- The app **auto-updates via Sparkle 2.9.2** (bundled `org.sparkle-project.Sparkle` + Updater/Downloader/Installer XPCs found in the shipped DMG) — it is **not distributed on the Mac App Store** [26][42].

### Pricing (official store page, checked 2026-10-16) [4]
One-time purchase, checkout via **Paddle**:

| License | Price | Covers |
|---|---|---|
| Personal | $39.95 | 2 Macs |
| Family | $59.95 | 6 Macs |
| Business | $99.95 | 10 Macs |
| Unlimited | $199.95 | unlimited Macs |

Optional add-ons at checkout: **"Lifetime Upgrades Guarantee"** (all future major upgrades free) and a **Priority Support Plan** [4]. Previous-version owners upgrade at **50% off** [2][4]. Independent price check (itechguides, 24 Sep 2026) confirms the same tiers [38]. Deal resellers (Cult of Mac) have sold the Personal tier at $15.99 with "2 Macs / minor updates included / 30-day refund" terms [39].

### Licensing / activation (EULA) [15]
- Perpetual, non-transferable license; **Personal is personal/non-commercial only**; each license activates N Macs and can then sync "with any number of connected Macs and other supported devices" [15].
- **Minor updates free; major upgrades at 50%**; a grace period gives a free upgrade to buyers just before a major release [15][35].
- Online activation: hardware ID + product ID + activation code encrypted and sent to Electronic Team servers; apps "may be occasionally reactivated in the background" — requires internet. Offline activation possible via file exchange. **The Free edition also activates silently in the background "for statistics"** [15].
- **Refund policy**: "all sales are FINAL" except a **14-day grace period** that only applies if the product malfunctions, the user reports it in writing, and Electronic Team cannot fix it in reasonable time [35]. This is the direct source of many refund complaints (§6).

### Requirements
- macOS **12.4+** (as of 8.11; earlier releases went back to 10.10–10.15) [2][6][23].
- Android **4.0+**, iOS **5+** per KB [23] (the iOS connection doc still says "iOS 5.x–13.x", a stale 2021 doc [18]; 2025 release notes show ongoing iOS fixes [6]).
- Download size ~109 MB (site says 108.59 MB free space; DMG measured 113.8 MB) [2][42].
- iOS mounting requires **macFUSE/OSXFUSE** (strings in the binary + reviewer hit an osxfuse install prompt) [26][42].

---

## 2. What it syncs and with what

SyncMate is a **plugin-based sync hub**: each connected endpoint exposes a set of "plugins" (data types), each with its own direction/options. Plugins: **Contacts, Calendar, Folders, Photos, iTunes/Music, SMS, Calls, Safari bookmarks, Reminders, Backup, AutoSync, Mount disk** [7][19][20][21][22].

### Connection types
| Endpoint | Transport | Notes |
|---|---|---|
| **Android** | USB (ADB + auto-installed "SyncMate module" APK), **Wi-Fi** (module app + IP allow-list modes "Unknown IPs" / "Only allowed IPs" / "Anyone can sync"), **Bluetooth** | Binary contains `_adb._tcp` mDNS strings; sideloaded module required [16][42] |
| **iOS** | **USB only** in current docs (Wi-Fi fix mentioned in 8.9 notes — UNVERIFIED that iOS Wi-Fi exists) | Uses libimobiledevice/libplist/libusbmuxd stack [18][42]. File sync restricted to `~/User/Media` on device; files copied to iOS are "storage only, not usable" [14][18]. Mount Disk shows only `~/User/Media` [18] |
| **MTP** | USB | **One-directional only** (Mac→device *or* device→Mac; **no bidirectional**) [17]. No temp-file copy since 6.3 [6] |
| **Mounted devices** | Any volume mounted in Finder: "Mac partition, portable USB storage, any locally connected disk, devices connected via FireWire" — includes **network drives** (Folders plugin "handling of network drives" improved in 6.4.323) [11][6]. Finder-mounted **FTP is read-only → one-way sync only** (drive→Mac) [11][14]. PIM sync to a mounted device writes a special "sync files" folder = effectively a backup [11] |
| **Other Macs** | TCP **port 27384**; **Bonjour `_syncmate._tcp.` auto-discovery** on LAN (service type confirmed in binary); manual add by IP/DNS | §2.1 below [8][9][42] |
| **Cloud/accounts** | **iCloud**, **Google account / Google Drive** (google-api-objectivec-client: Drive, Calendar, People), **Dropbox** (ObjectiveDropboxOfficial SDK), **Microsoft** (MSGraphClientSDK + MSAL 2.14: OneDrive, Outlook Live, Office 365 Home & Business) [1][2][41][42]. Connections mounted via sibling app **CloudMounter** also appear as syncable drives [6] |

### 2.1 How Mac-to-Mac sync works [8][9]
- **SyncMate must be installed on every participating Mac** — there is no agent-less mode. The license counts activated Macs [15].
- **Discovery**: Bonjour advertises/discovers SyncMate peers on the LAN automatically; alternatively "Add a Mac manually" with IP/DNS + optional `:port` (default **27384**). For non-LAN connections the docs demand the remote side have a real, Internet-reachable IP [9].
- **Inbound connection policy** ("Configure your Mac's incoming connections"): *Allow connections from all hosts* / *Ask me before establishing connection* / *Allow connections from hosts in this list only*; configurable listen port; **optional password** requested by the connecting side [8].
- **Single-initiator model**: sync is started only on the "main" Mac; pressing Sync on it drives both sides — nothing to trigger on the remote Mac [9].
- **Security**: optional shared-password + host allow-list are the only documented controls. Whether the TCP protocol itself is encrypted is **UNVERIFIED** — the site makes a generic "advanced traffic encryption" marketing claim [37] but no protocol doc describes TLS on port 27384.

---

## 3. Folder sync specifics (Expert) [10][25]

- **Pair model**: `Add new pair` → choose **On Mac** folder + **On device** folder → **Sync direction** dropdown (e.g. *Mac computer ↔ Other side*). Multiple pairs per connection; each pair row shows a direction icon (bidirectional ⇄ blue, one-way → orange) and a **Filter** checkbox [10].
- **Directions**: three — *Mac→device*, *device→Mac*, *bidirectional* — except endpoints that restrict it (MTP & mounted iPhoto/iTunes playlists: no bidirectional) [11][17][25].
- **Filters** ("Configure Filters" dialog): exclusion masks applied to **Subfolder** name, **File** name, or **Path**; plus **Size from/to** and **Date modified from/to** columns; checkbox **"Ignore invisible files and folders"** (dotfiles). Filters are enabled per-pair via the Filter checkbox [10].
- **Conflict resolution** — dropdown *"How to resolve conflicts?"* per connection/plugin with exactly three policies (confirmed in KB screenshot *and* in 8.11 binary strings): **"Use from this Mac"**, **"Use from Other Side"**, **"Use the newest file"** [10][42]. A past bug fixed "incorrect behavior of 'Use from Other Side'" [6]. There is **no rename-both/conflict-copy option** and **no per-file interactive resolution** documented.
- **Deletion handling**: **UNVERIFIED.** The docs never state whether two-way folder sync propagates deletions or where deleted files go (no recycle/versioning feature documented). Contacts sync has an explicit *"Delete synchronized contacts"* + *"Reset history"* escape hatch for wipes/re-syncs [19]. A MacUpdate review reports a "trash issue" where sync "replicated everything" until 25 TB was consumed — consistent with deletes going to Trash and re-syncing, but unconfirmed [27].
- **AutoSync** (per connection): *"Sync when other side is available every N min"* (availability-triggered + interval) and *"AutoSync only when SyncMate's GUI is hidden"* [12][16]. Preferences add a periodic *"Remind me to sync every…"* prompt and *"Always keep app's icon in the menu bar"* [13]. There is **no filesystem-watch trigger** — it is availability + timer based.
- **Background agent**: background sync runs when the app is hidden/menu-bar-resident; Free edition includes "Syncing in background" [1][3]. Sync still depends on the app/agent running — nothing indicates a standalone launchd daemon.
- **Simultaneous connections**: multiple connections can sync in parallel; 7.4 notes "simultaneous sync of several connections is now more stable" [6].
- **Performance & limits**: no documented file-size/file-count limits. Signals from history & reviews: MTP used to copy via temp folder (fixed 6.3) [6]; a **critical Folders-plugin bug "could potentially result in data loss"** was fixed in 6.4.323 (2016) [6]; "if data is modified while sync is still in progress, certain changes might not be applied" (no mid-sync consistency) [14]; users report **full rescans of both trees** and multi-hour syncs that fail on sleep [27][30].
- **Activity log**: every sync writes a viewable log with *"Show log → Send to technical support"* (in-app support bundle) [16].

---

## 4. UX paradigm

**Device-centric sidebar.** The main window (seen in KB screenshots [10]) is a classic macOS window: a **dark left sidebar** headed *"Mac computer"* with a **CONNECTIONS** section listing each configured endpoint (e.g. *Other side*, *Dropbox*, *Disk Image*, *Google Drive*) plus a dashed **"Add new"** tile. Selecting a connection fills the right pane with a **tab bar — one tab per plugin** enabled for that endpoint (*Summary, Calendar, Folders, Calls, Contacts, Safari bookmarks…*) and a "+" to add plugin tabs [10][26].

**Onboarding**: *Add new* → wizard picks **device type** → **transport** (Android: USB/Wi-Fi/Bluetooth; iOS: USB) → **checkbox list of plugins** to enable → land on the connection's tabs to configure each plugin [16][18][26]. Flow per plugin is uniform: *Add new pair → set direction → optional filters/conversion → Apply → Sync* [10][20][22].

**Folder tab**: table `On Mac | Direction | On device | Filter`, *Configure Filters* + *Add new pair* buttons, *How to resolve conflicts?* dropdown, and a **status footer** — *"Last sync: never · Free: 13.14 GB · Used: 11.77 GB"* with Cancel/Apply [10].

**Menu bar / background**: preference to keep a status-bar icon; AutoSync can run "only when GUI hidden"; periodic sync reminders [13].

**Mount Disk**: mounts an endpoint as a local disk browsable in Finder — since **8.2 (Jun 2021) iOS-only** (macFUSE); Android/MTP mounting was moved to sibling app **MacDroid** (Expert upgraders got a 1-year MacDroid license); Dropbox/Drive/OneDrive mounting was removed in **7.2 (2018)** in favor of **CloudMounter** [1][6][16].

**Onboarding friction observed by reviewers**: iOS path demanded an **osxfuse** install mid-flow; the Android Wi-Fi path requires sideloading an APK that triggers security warnings; SMS sync stalled at ~40% [26]. Another reviewer notes music/iTunes plugins silently did nothing — they fell back to Folders sync [40].

---

## 5. Electronic Team, Inc. — company, related apps, business model

- **Company**: Electronic Team, Inc., 11890 Sunrise Valley Dr, Ste 111, Reston, VA 20191, USA; **formerly "Eltima" / Eltima Software** (founded ~2001; products moved under the Electronic Team brand around 2022 — Eltima LinkedIn now redirects). ~50–70 developers; self-claims 1M+ users in 198 countries; privately held [34][36][37].
- **Mac product line** [34]: **SyncMate** (sync hub), **MacDroid** (Android↔Mac file transfer incl. disk mounting — absorbs SyncMate's Android mount feature), **Commander One** (dual-pane file manager/FTP), **CloudMounter** (mount cloud storage in Finder — absorbs SyncMate's cloud-mount feature), **Elmedia Player**, **Folx**, **JustStream**, **DoCast**, **TVRem**, plus PhotoBulk/Uplet-type utilities and the serial/USB line (**USB Network Gate, FlexiHub, Donglify, Virtual Serial Port Driver, Serial Port Monitor, Serial to Ethernet Connector, HelpWire**) [34][36].
- **Business model**: freemium → perpetual per-machine-count license tiers, paid major-version upgrades (50% off; optional "Lifetime Upgrades Guarantee"), priority-support add-on, Paddle checkout, SDK/OEM licensing for port tech, and aggressive **cross-sell**: features removed from SyncMate were re-homed to paid sibling apps with license-transfer offers (MacDroid, CloudMounter) [4][6][34][35]. Refunds are essentially "final sale + 14-day malfunction-only grace" [35].

---

## 6. Strengths and weaknesses — user sentiment

**Aggregate ratings**: MacUpdate **3.4/5** (178 ratings, 197 reviews; sub-scores: Value 1.5, Ease 1.5, Features 1.5, **Reliability 1.3, Support 1.7**) [27]. PissedConsumer **2.0/5** (2 reviews: unresponsive support, billing complaints, data-loss reports) [32]. Legacy "SyncMate Plus" Mac App Store listing: **2.0/5** (30 ratings, mostly old/no-support complaints) [33]. The vendor site self-reports "4.5 based on 795+ users" — treat as marketing [2].

**Strengths (cited by users)**
- *Only tool* covering Mac↔Android and Mac↔many-endpoints in one app; no cloud middleman ("no third-party services… syncs directly") [1][26][41].
- Large library sync across drives: *"I use Syncmate to Synchronise large photobiblios (100.000 pics and more) on different Drives. Works great. Also super Support."* — MacUpdate, Sep 2023 [27].
- Contacts sync into Android "seamless"; Wi-Fi connect "recognized immediately" [26]. Android-as-disk praised on CNET [31].
- AutoSync: *"whenever I am in my house and both my devices are active and connected to the wifi, SyncMate starts the synchronization process in the background after every sixty minutes"* [31-review snippet via 40-related source — treat as anecdotal].

**Weaknesses (cited by users)**
- **Reliability under scale/sleep**: *"It takes several hours to sync, and the sync almost always fails in the end if either computer goes into sleep mode… requested a refund, but they flatly refused. So I wasted £100."* — MacUpdate, Dec 2021 [27]. *"Stay far away… it bogged down… spinning wheel… force quit… my sync folders were all messed up"* (82 GB job) — MacRumors [30]. *"On the slow side, and… has to check all of the files on both sides"* — MacRumors user comparing to GoodSync [30].
- **Destructive/duplicate-sync incident**: *"sync mate had made copious copies of my files to the point 25tb was taken up by about 200gb of data… NO REFUNDS"* — MacUpdate, Aug 2023 [27].
- **Refund/support friction**: "all sales FINAL" + 14-day malfunction grace [35]; *"NO REFUNDS. NO CUSTOMER SERVICE… mangled my computer"* — SaaSHub 2019 [28]; pre-sales answers fast, post-sale silence reported in MacRumors thread (later resolved via grace-period upgrade) [29]; PissedConsumer: unreachable phone support [32].
- **Feature drift vs. docs**: Office 365 sync accused of only working with Outlook Live [31]; Safari-bookmark sync broken on Android 6–8 (Chrome replaced stock browser) [14]; iOS features trimmed by Apple restrictions over time [27].
- **Install footprint/privacy**: expert install "installed pieces of itself all over my operating system" [28]; background activation of the Free edition [15]; anonymous usage stats opt-in [13].
- **Company reputation note**: Eltima's Elmedia Player/Folx briefly distributed malware via a compromised update channel in 2017 — referenced in the MacRumors SyncMate warning thread [29].

---

## 7. What transfers to Foldover's MVP — and what doesn't

### Worth borrowing
- **Endpoint-centric model**: a sidebar of *connected endpoints* (mounted volumes, shares, Macs, SFTP hosts) where each endpoint gets its own typed configuration page. For Foldover this collapses nicely to: endpoint = a filesystem root with a per-endpoint pair table [10].
- **Auto-discovery of peers and volumes**: SyncMate's `_syncmate._tcp` Bonjour + "Add new → pick from discovered Macs" is exactly the LAN-peer UX Foldover wants (mDNS/Bonjour via `NSNetService`/Rust `mdns-sd`); likewise auto-listing **mounted volumes** (`Disk Image`/`Mounted device` entries) instead of making users type paths [9][11][42].
- **Pair table UX**: `local path | direction icon | remote path | filter` rows is compact and instantly readable; direction as a first-class per-pair setting (⇄ / → / ←) [10].
- **Simple, nameable conflict policies**: *newest-wins / this-side-wins / other-side-wins* cover most needs — but Foldover should add a **rename-both (conflict copy)** option, which SyncMate lacks.
- **Availability-triggered AutoSync** ("sync when other side is available every N min") — the right trigger for removable drives/NAS that come and go, plus "only when hidden" and menu-bar presence [12][13].
- **Filter model**: mask × {subfolder, file, path} + size range + date range + "ignore invisible files" — maps cleanly to `ignore`-style rules in a Rust core [10].
- **Status footer + activity log → support bundle** ("Show log → Send to technical support"): cheap, high-trust feature [10][16].
- **Inbound-connection policy** for Mac-to-Mac: allow-all / ask-before / allow-list + optional password is a sensible LAN security baseline — Foldover should do the same **plus real transport encryption** (SyncMate's is undocumented) [8].
- **Freemium gating lesson**: SyncMate's free tier (contacts/calendars only) exists mainly as a trial for Expert; Foldover could gate advanced sync modes rather than core usability.

### Do not copy
- **PIM/media plugin architecture** (contacts, calendars, SMS, iTunes, bookmarks) — out of Foldover's scope by definition; SyncMate's complexity and half its reviews' pain comes from these endpoints.
- **Timer-only AutoSync**: availability+interval polling without filesystem watching means full-tree rescans both sides — the root of its "slow, hours, sleep-kills-it" reputation. Foldover should use `FSEvents`/`fsnotify` watchers + incremental state, and **power assertions / resumable checkpoints** so sleep doesn't corrupt a run [27][30].
- **Single-initiator, app-on-both-sides without service**: requiring the GUI app open on each peer (no documented daemon). Foldover should ship a small **always-on agent (launchd)** so unattended sync actually works [9][30].
- **Silent delete semantics**: no documented delete policy and a user-visible trash-duplication horror story — Foldover needs explicit, previewed delete propagation + a versioned trash/quarantine option [27].
- **Removing features into paid sibling apps** and **final-sale licensing** — both generate the most toxic reviews; avoid for trust [6][29][35].
- **Classic AppKit-era wizard UI**: functional but dated; a SwiftUI declarative settings surface is a differentiator [26].

---

## Sources

1. https://www.sync-mac.com/ — product page (editions, features, FAQ)
2. https://www.sync-mac.com/syncmate-features.html — devices/accounts supported; version/requirements box
3. https://www.sync-mac.com/syncmate-expert.html — Free vs Expert feature list
4. https://www.sync-mac.com/syncmate-upgrade/ — official purchase page (license tiers, add-ons)
5. https://www.sync-mac.com/rss/en-syncmate.rss — release-notes feed (all versions)
6. https://help.electronic.us/support/solutions/articles/44002222481-what-s-new-in-this-version — full "What's New" KB
7. https://help.electronic.us/support/solutions/44000802080 — SyncMate KB index
8. https://help.electronic.us/support/solutions/articles/44001762489-configure-your-mac-s-incoming-connections — inbound connection policy, port, password
9. https://help.electronic.us/support/solutions/articles/44001762623-establish-connection-with-another-mac — Bonjour discovery, port 27384, single-initiator
10. https://help.electronic.us/support/solutions/articles/44002222293-folders — Folders plugin (pairs, direction, filters, conflicts; screenshots viewed)
11. https://help.electronic.us/support/solutions/articles/44001763986-establish-connection-with-mounted-devices — mounted volumes, FTP read-only, PIM-as-backup
12. https://help.electronic.us/support/solutions/articles/44002222465-automatic-synchronization — AutoSync triggers
13. https://help.electronic.us/support/solutions/articles/44002222176-preferences — menu-bar icon, sync reminders, telemetry
14. https://help.electronic.us/support/solutions/articles/44002223376-known-issues — documented limitations
15. https://help.electronic.us/support/solutions/articles/44002235010-expert-personal-license — EULA (2-Mac cap, activation, update policy)
16. https://help.electronic.us/support/solutions/articles/44001762634-establish-connection-with-android-device — Android USB/Wi-Fi/BT, module, access control, MacDroid note
17. https://help.electronic.us/support/solutions/articles/44001763644-sync-with-mtp — MTP one-way only
18. https://help.electronic.us/support/solutions/articles/44001763980-sync-with-ios-devices-using-syncmate — iOS USB, ~/User/Media limit
19. https://help.electronic.us/support/solutions/articles/44002222245-contacts — Contacts plugin (delete/reset-history)
20. https://help.electronic.us/support/solutions/articles/44002222408-itunes-music — iTunes/Music plugin
21. https://help.electronic.us/support/solutions/articles/44002222468-backup — Backup plugin
22. https://help.electronic.us/support/solutions/articles/44002222343-photos — Photos plugin
23. https://help.electronic.us/support/solutions/articles/44002222505-is-my-os-supported- — OS compatibility
24. https://www.sync-mac.com/sync-several-macs.html — Mac-to-Mac marketing page + option matrix
25. https://www.sync-mac.com/options/sync-mac-folders.html — folder sync marketing page (3 directions, filters, priority)
26. https://macsources.com/syncmate-8-mac-app-review/ — hands-on review (onboarding flow, osxfuse, APK sideload, pricing)
27. https://syncmate.macupdate.com/ — MacUpdate listing + user reviews (3.4/5; quotes)
28. https://www.saashub.com/syncmate-reviews/review-859470e29931 — 2019 "AVOID AT ALL COSTS" review
29. https://forums.macrumors.com/threads/warning-eltima-software-syncmate.1884533/ — MacRumors thread (upgrade paywall, support, malware reference)
30. https://forums.macrumors.com/threads/best-mac-file-sync-program.2239077/ — MacRumors thread (slow full rescans, failed 82GB sync)
31. https://download.cnet.com/syncmate/3000-18511_4-75982156.html — CNET listing + user reviews
32. https://www.pissedconsumer.com/syncmate/RT-F.html — PissedConsumer 2.0/5
33. https://apps.apple.com/us/app/syncmate-plus/id423327943 — legacy App Store listing (2.0/5)
34. https://www.electronic.us/products/ — full product line
35. https://www.electronic.us/company/policies/terms/ — Terms: refund (§8, final + 14-day grace), upgrade policy (§9)
36. https://www.eltima.com/about-us/ — company history (Eltima → Electronic Team, ~2001, 70 staff)
37. https://www.sync-mac.com/about-us/ — company stats page (50+ devs, "traffic encryption" claim)
38. https://www.itechguides.com/products/syncmate/pricing/ — independent pricing check (24 Sep 2026)
39. https://deals.cultofmac.com/sales/sync-up-all-your-devices-online-accounts-w-syncmate-expert — deal listing (license terms)
40. https://www.patchworkoftips.com/syncmate-mac-review/7815/ — user review (music plugin failed → used Folders)
41. https://www.electronic.us/products/syncmate/ — product page w/ on-site user quotes
42. Local artifact analysis of the shipped `syncmate.dmg` (v8.11 build 604, downloaded from https://cdn.electronic.us/products/syncmate/mac/download/syncmate.dmg) — bundle contents: `com.eltima.SyncMate` 8.11 + `SyncMateService`/`SyncMateConverter`/`SyncMatePerformer`/`SyncMateCalendarHelper` helpers, `Appstatico`, Sparkle 2.9.2, ObjectiveDropboxOfficial, GoogleAPIClientForREST (Drive/Calendar/People/Core), GTMSessionFetcher, GTMAppAuth/AppAuth, MSGraphClientSDK + MSAL 2.14.0, FeedbackReporter; `_syncmate._tcp.` + `_adb._tcp` service strings; libimobiledevice/libplist/libusbmuxd; macFUSE/OSXFUSE strings; port 27384; UI strings ("Add new pair", "Use the newest file", "AutoSync only when SyncMate", "Allow connections from all hosts", port defaults); entitlements (Apple Events, disable-library-validation, addressbook/calendars/photos access)
