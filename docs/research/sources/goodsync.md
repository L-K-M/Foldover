> Raw AI research report (devin, SWE-2 max), 2026-10-16. Unedited. Corrections: [fact-checks](../README.md#corrections).

# Deep-Dive: GoodSync (Siber Systems)

*Research date: 2026-10-16. Prepared as competitive research for "Foldover", a native-macOS local/network file-sync app. All facts carry a source marker [n] → Sources list at the end. Items marked UNVERIFIED could not be confirmed from a primary or reliable secondary source.*

## TL;DR

GoodSync is a 20-year-old cross-platform (Windows/macOS/Linux/NAS/mobile) two-way sync and one-way backup tool built around a persistent per-folder state database (`_gsdata_`), a mandatory Analyze → review → Sync workflow, and its own P2P file-transfer protocol (GSTP / "GoodSync Connect") for device-to-device sync. It is one of the most protocol-complete sync tools on the market (own SMB client, own SFTP client, AFP, WebDAV, MTP, ~15 cloud services) and is cheap for multi-device households ($29.95/yr, 5 devices) — but it is widely criticized for a dated, non-native cross-platform UI, jargon-heavy "job" model, intermittent errors/lockups, mediocre support, and a subscription-only license that requires an online account. For Foldover it is the single best reference for the *semantics* of consumer two-way sync (state file, deletion propagation, conflict taxonomy, recycle/versioning) and a cautionary tale for UX.

---

## 1. Product family, editions, versions, pricing

### 1.1 Product lineup

| Product | What it is | Price (Oct 2026) |
|---|---|---|
| **GoodSync Free** | Full sync/backup app, capped at **3 jobs** and **100 files** per installation; "Number of allowed connections: 0" (the comparison table's wording — likely parallel FS connections; ambiguous) [1][2] | Free after 15-day trial |
| **GoodSync Personal** | Consumer edition, Win/Mac/Linux-NAS; "for consumers only, no business use" [3] | **$29.95/year**, subscription; one purchase covers **5 devices**; each device beyond 5 needs an extra license [4][5][6] |
| **GoodSync for Business — Workstation** | Per-endpoint (Windows PC or Mac), adds admin "Policies" limiting what users can change [2][7] | **$49.95/yr per workstation** |
| **GoodSync for Server OS** | For Windows Server / Linux Server, one license per physical or virtual server [7][8] | **$499.95/yr per server** |
| **GoodSync Backup Control Center** | Web-based central management (jobs.goodsync.com) that distributes jobs to "Runners" (a specific user on a specific computer) and collects run reports; recommended ≥10 machines [7][9] | $49.95/yr per workstation, $499.95/yr per server |
| **GoodSync Connect** | P2P transport layer built into every GoodSync install; "comes free with the purchase of any GoodSync product" (and is present in Free too) [10][11] | Free |
| **GoodSync Explorer** | Standalone file manager over every supported file system (LAN, GS Connect, FTP/SFTP, WebDAV, clouds, mobile); installed alongside the app on Windows and Mac [10][12][13] | Free/included |
| **GoodSync Storage (RealDisk)** | Siber's own cloud storage, addressable as `gstore://` via the GSTP stack; 10 GB / 100 GB tiers, ≤15-day trial; third-party review cites from $9.99/yr for 10 GB [1][14][15] | Subscription (UNVERIFIED current tiers) |
| **GoodSync2Go** | Portable build that runs from a USB drive; jobs+license live on the drive; no service install (no VSS, no GS-Connect server, no On-Schedule) [16] | Special GS2Go license [4] |
| **GoodSync for WD** | WD My Cloud-branded edition; Free tier (3 endpoints, 1 job, Google Drive/Dropbox/GS Storage only) and Premium (5 endpoints, unlimited jobs, adds FTP/SFTP/WebDAV and more clouds) [17] | Free / subscription |
| NAS packages | Native packages for Synology, QNAP, WD, ASUSTOR, "misc NAS"/Linux — all run gs-server + web UI [1] | Per-device license |
| Mobile | iOS (iPhone/iPad, iOS 14+) and Android (7+) apps that act as GS Connect clients *and* servers [18] | Included |

Licensing mechanics (v12): everything is anchored to a **GoodSync Account** (verified email); devices are *enrolled* into the account; licenses are *assigned* to devices via "Help → Activate"; new devices can be gated by **one-time passwords** (email/SMS/authenticator app) [4][19]. All current licenses are **annual subscriptions** — v12 dropped the old perpetual model, which is a recurring sore point in reviews [5][20]. A 15-day full-feature trial precedes the free tier [4].

### 1.2 Current versions (as of 2026-10-16)

- **Windows/Mac: 12.11.9, released 2026-10-06** [21][22]
- NAS/Linux: 12.11.8 (2026-10-02); Android 12.11.8 (2026-10-05) [23][24]
- Mac build: macOS 10.13+, universal 64-bit Intel + Apple Silicon; distributed **only direct** (`.zip`, `.dmg`, `.pkg`, shell-script installer) — no Mac App Store edition (the App Store listing is for the iOS companion app) [3][18]
- Notable 2025-2026 changes: own in-house SFTP client replacing a PuTTY-based one (12.9.6, Aug 2025); ssh-agent support; "new common cross-platform SyncTree" merged into the Mac GUI (12.9.14, Dec 2025); Jobs Web UI for headless NAS/Linux (12.8.8); macOS 26 input-control fixes (12.9.24); Kerberos/DFS support in their SMB client [22][25]
- Lineage: Siber Systems (the RoboForm company); GoodSync 9 introduced GoodSync Connect in 2012 at $29.95+$9.95 per extra license [26]; v12 moved everything under the account/subscription umbrella [11]

---

## 2. Sync engine

### 2.1 The Job model and Analyze → Sync workflow

The atomic unit is a **Job**: a named pair of folders ("Left" and "Right", which can live on any supported file system) plus General, Filter, Auto, Script, and per-side Folder options [27]. The canonical flow, drilled into users everywhere:

1. Create job, pick Left/Right folders via a Browse dialog that doubles as credential entry ("Server Account Manager" stores credentials per file-system account and is shared across jobs — fixing the old Macworld complaint that credentials had to be re-entered per job) [27][28][29]
2. **Analyze** — builds a "Sync Tree" diff: arrows L→R / R→L, ⊘ "Do Not Copy", crossed arrows for propagated deletions; green = effective action, grey = alternatives; the user can click to override direction per item [27][30]
3. **Sync** — executes the (possibly user-edited) action plan [27]

Job types: **Synchronize 2-way** (default), **Backup L→R**, **Backup R→L** [31]. Groups hold multiple jobs; "Analyze All"/"Sync All" exist; jobs sharing no folders can auto-run in parallel [32][33].

### 2.2 State: `_gsdata_` and friends

Every synced folder gets a hidden **`_gsdata_`** directory containing [34]:

- `_file_state._gs` — the sync-state database recording what was synced and how the sides compared (this is the "last synced" baseline that makes two-way sync work)
- `_insync.gsl` and other small internal files
- Job `*.log` files
- `_saved_` — previous version of every file GoodSync deleted or overwrote, **last version only**, ON by default, auto-purged after 30 days (configurable) [34][35]
- `_history_` — every version, timestamp inserted into the filename, directory structure preserved, off by default; mutually exclusive with `_saved_` [35]

"**No _gsdata_ folder here**" (per-side option) moves state into a `_mirrors_` folder in the local GoodSync profile (`~/.goodsync` on Mac/Linux) — intended for read-only sides or where writing into the synced folder is impossible, but dangerous if the folder is synced by other jobs [36]. A Mac menu command "Make gsdata visible" toggles Finder visibility [34]. Deleting `_gsdata_` resets a pair to "first sync" semantics [37].

First sync: with no stored state, files identical on both sides get "Do Not Copy"; files present on one side copy across; files that *differ* on both sides become **conflicts** (see 2.4) rather than blindly overwriting — the help center explicitly tells users to review Analyze before first Sync, "especially … when selecting a destination that already contains files" [38][39].

### 2.3 Change detection and propagation

- Default change test: **file size + modification time** vs. stored state; optional "**Compare Checksums when needed**" (MD5 when size matches but mtime changed) and "**Compare Checksums of All Files**" / "Compare Bodies" (slow, deliberate modes) [36][40]
- Diffs current state vs. stored state on each side independently → propagates L→R and R→L → conflicts when both sides changed the same file; deletions propagate by default (1-way jobs can disable "Propagate Deletions"); it correctly handles **chained sync** (A↔B, B↔C, C↔D, incl. sneakernet via USB) and detects when a user *reverts* a file to an older copy [41][42]
- **Move/rename detection** on by default — folder moves detected "when all files inside a folder are moved to the same destination"; caveat: identical size+mtime duplicates can defeat detection [43]
- Special actions: **Copy Time** (same length, different mtime → copy only the timestamp), **Copy Name Case** (case-only renames), **Sync State** (files equal but stored state differs — e.g. after folders were synced by other tools or state files deleted) [30]
- Time-zone/DST engineering: ignores ±2 s FAT mtime granularity, corrects whole-hour "Old-New" and "Left-Right" time shifts without re-copying, and implements "mod-time translation" for filesystems that can't store mtime [1][44]
- **Lock files** in the sync folder prevent two GoodSync instances (even on different computers) from syncing the same folder simultaneously; auto-jobs wait for locks to clear by default (default wait 20 min) [33][45]
- Safety valves: "**Safe Copy using temporary files**" (copy to temp, rename — on by default where the FS lacks atomic upload); "**Do not sync files that changed between Analyze and Sync**"; "**Do Not Sync if changed files more than 50%**" fuse on unattended 2-way jobs [33][36][46]
- Locked/open files: Windows uses **VSS snapshots** (from locked files only, local disks only, never into a locked file; unavailable in GS2Go); optional "snapshot sync folder before Sync" for backup consistency. Mac has no VSS — the workaround-ish "**Uber-unlocked mode**" lets sources be moved/deleted mid-upload and aborts the upload cleanly [31][47]
- Performance: **parallel transfer threads** (default 3; helps small-file-heavy remote jobs over FTP/SFTP/WebDAV/GSTP), per-job bandwidth cap in KB/s, automatic reconnect (default 120 s), resumable transfers [10][33]
- **Block-level/delta transfer exists only inside GSTP** (between GoodSync-Connect devices and GoodSync Storage): "only the parts of a file that differ are transferred." For SMB/SFTP/etc. it copies whole files [10][48]

### 2.4 Conflicts

A *conflict* = same file changed on both sides. Types [49]:

- **Modified on Both Sides** — resolvable by picking a direction
- **Vertical Delete–Change** — folder deleted on one side, files inside modified/deleted on the other
- **Case Conflict** — items differ only by name case; *non-resolvable*, user must fix
- **ACL/xattr-only conflict** — bodies equal; minor, resolvable

Resolution policy (Auto settings): "**Automatically Resolve Weak Conflicts**" on by default — weak = same size, differing mtime, one side lacks history/generations → the side with generations or the newer mtime wins. Strong-conflict policy defaults to **Do Not Copy** (surface to user); alternates: Left Wins / Right Wins / Newer Wins / Stop Sync. Optional "**Rename Losing File, not Delete**" produces `FileName (Conflicted Copy from COMPUTER on DATE).ext` [50].

### 2.5 Filters

Include/Exclude lists (exclusions win) over name wildcards, `path`/`name`/`size`/`mtime`/`ctime` terms, `any:`/`all:` side qualifiers, `exist-x` existence tests (e.g. `existone` = present on one side only); preset exclusions for hidden/system/temp files (checked by default in sync jobs); right-click post-Analyze "Exclude/Include only" shortcuts; filters are copy-pasteable between jobs [51].

### 2.6 Auto-sync triggers (the "Auto" tab) [32]

- **On File Change** — real-time: polls/monitors every ~3 s, waits a configurable *delay* (default 20 s, max 300 s) after the last change, force-starts after 5 min of continuous change. Works on: local Win/Mac disks, Windows Shares, **any of those accessed remotely via GSTP**, OneDrive/Google Drive/pCloud. **Not** available on FTP/SFTP/S3-style storage. In v12.11.3+ it can do *incremental* analyze [22][32]
- **On Folders Connect** — when a sync folder becomes available (checks every 3 min; volume insert/eject and network up/down events accelerate detection) — this is the "on drive connect" trigger [32]
- **On GoodSync start**, **On Logoff** (Windows; attended mode only), **Periodically** (default every 2 h), **On Schedule** — cron-style minute/hour/DoM/Month/DoW fields, can wake a sleeping Windows PC [32]
- **Unattended** — runs any auto trigger via the **Runner Service** (a system service that impersonates the user account, so it works logged-out and survives app quit; on Mac it moved to a system-service mode in 12.7.4) [22][32][48]
- Jobs that don't share folders auto-run in parallel (configurable); a second trigger event on a running job is ignored [32]

### 2.7 Mac metadata handling

Documented, per-side/per-job switches [36][52][53]:

- **Copy Permissions** (rwx, Mac/Unix, default on), **Copy Owner** (off), **Copy ACL** (off; *DACL* only, NTFS-to-NTFS / Mac-to-Mac / Mac-to-Linux jobs), **Copy Extended Attributes** (`/copy-ext-attrs`, Mac-only) and **Compare Extended Attributes** (off by default — xattr diffs only trigger a copy of attributes when the file body also changed, unless enabled); the features page advertises xattr copying "used to store custom icons on Mac" [1][52][53][63]
- **Copy File Creation Time** (off), **Copy original file system names** (Mac-only, on — doesn't transliterate `?\<>|*"` in Mac-to-Mac jobs) [52]
- Extended attributes on macOS subsume Finder tags (`com.apple.metadata:_kMDItemUserTags`) and resource forks (`com.apple.ResourceFork`), so tag/fork propagation is *implied* by xattr copying — **UNVERIFIED**: the docs never name Finder tags or resource forks explicitly, and there's no documented `.DS_Store` policy. ACL propagation "across the Internet" is advertised [1]

---

## 3. Protocols and endpoints

### 3.1 Local and network (the Foldover-relevant set)

| File system | URL scheme | Notes |
|---|---|---|
| Local disk | `file:///path` | Win + Mac FS drivers; Mac requires Full Disk Access prompt [3][54] |
| **Sib-SMB** | `smbd://server/share/path` | **Siber's own SMB client** on Win/Mac/Linux — claims better server discovery than Windows, requires explicit per-server credentials (ignores Windows-cached auth), added Kerberos 5 / DFS-referral support in 2026 [21][55] |
| Win-SMB | `smb://` / `\\server\share` | Windows-only, Microsoft Networking stack [56] |
| **AFP** | `afp://server/share/path` | Still supported on Mac [57] |
| FTP/FTPS | `ftp[s]://…` | Explicit/implicit TLS supported [58] |
| **SFTP** | `sftp://host[:port]/path` | SSH-2 only; in-house client since 12.9.6 — Strict-KEX (Terrapin fix), curve25519/ed25519, AES-GCM/CTR, OpenSSH+PuTTY key formats, **ssh-agent/Pageant**, host-key pinning, UTF-8 names [22][58] |
| WebDAV | `http[s]://…` | Socket-based default (fast) + WinInet fallback for complex proxies/auth [59] |
| MTP | `mtp://…` | Windows-only, USB media devices [60] |
| "One File" | `onefile://…ofz` | Whole tree stored in a single compressed/encrypted archive file [59] |
| **GSTP / GoodSync Connect** | `gstp(s)://…` | Own P2P protocol, below |
| NFS | — | **Not supported** — NFS appears nowhere in the file-system list (UNVERIFIED as a deliberate omission, but factually absent) [54] |

Cloud targets (context only): Google Drive/Team Drive/Docs/Photos/Cloud Storage, iCloud Drive & iCloud Photos, OneDrive/Office 365/SharePoint, Dropbox, Box, Backblaze B2, Amazon S3, Azure Blobs/Files, MEGA (+MEGA S4), pCloud, Wasabi, plus first-party `gstore://` RealDisk [1][54].

### 3.2 GSTP / GoodSync Connect — the P2P protocol [19][61]

- `gstps://computer.userid.goodsync/path` — **mediated addressing**: virtual `.goodsync` TLD resolved via Siber's **Mediator** server; each gs-server re-registers its IP with the Mediator **every 5 minutes**
- `gstps://host[:port]/path` — **direct addressing** (no account needed; **Business editions only**); plain `gstp://` allowed on LANs but discouraged
- **NAT traversal**: UPnP to open router port → falls back to manual port-forward config → falls back to a **Forwarder** relay chosen by the Mediator when direct TCP is impossible (double-NAT "cannot be fixed")
- Ports: **TCP 33333** serve; **TCP 11000** local-only Web UI; **TCP 80/443** outbound to `mediator.goodsync.com`; **UDP 33338/33339** LAN broadcast discovery; **TCP 22222** optional WebDAV serving; **TCP 33334** local Manage API [61]
- Security: TLS 1.2/1.3 via OpenSSL with a fixed AEAD-ish cipher list (ECDHE + AES-GCM/SHA384); digest auth; optional per-device OTP enrollment; secondary "limited" users restricted to a home folder [19][61]
- Features: **block-level delta**, resumable transfers ("instant resume at the point of failure"), server-side MD5, ACL/attribute copy, proxy support, idempotent transactions, remote **real-time file monitoring over GSTP** (so On-File-Change works on remote peers — a real differentiator) [10][19][32][61]
- Each machine can be **client-only or also a server** ("Serve files to other devices"); a primary admin user + secondary limited users per server; server configurable via the local Web UI [19]

---

## 4. Automation and operations

- **gsync** — full CLI (`analyze`/`sync` by name or `/all`; `job`, `job-new`, `job-tmp`, `job-update`, `job-delete`, `job-list`; profile/log options `/profile`, `/ll=0..3|9`, `/jobs-file`, `/no-load-tic`; run options `/list-changes`, `/override-locks`, `/comp-cksums`, `/comp-bodies`; exit codes 0/1-4 analyze vs 11-14 sync). Standard build on Linux; needs Personal+ license on Win/Mac. Activation headless: `gsync /gs-account-enroll=…` + `/activate` [62][63]
- **gscp** — CLI filesystem client (ls/cd/put/get/mv/rm/mstart/mstop across every supported protocol, like a scriptable FTP client) [16][64]
- **TIX** — readable XML-ish job-descriptor export/import format covering jobs, options, server accounts; can be machine-generated [65]
- **Scripts tab** per job: *Pre-Analyze*, *Analyze-no-Sync*, *Post-Sync* hooks — either email (any field containing `@` → SMTP send, `noattach:`/`errors:` prefixes, `%SYNCOK%`/`%CHANGED%` params) or run a program with `%JOBNAME%`, `%RESULT%`, `%LOGPATH%`, `%LF%/%RF%`, `%ERRORS%`, `%CONFLICTS%`, `%TELAPSED%`, `nowait:`/`errors:` prefixes [66]
- **Notifications**: tray/balloon messages (never / conflicts+errors [default] / changes), SMTP email alerts, "Open Left/Right Logs" for unattended runs [50][66]
- **Logs**: per-job logs inside `_gsdata_`, global log in profile dir, detail levels 0-3 (client) / 0-5 (server), retention days, custom folder [33][67]
- **Runner Service** for logged-out execution; "Start GoodSync when Mac/Windows starts", "Start in Menu bar" (Mac) / tray (Windows) [32]
- **Control Center**: cloud console — companies, authorized Runners (`user@computer`), push jobs + shared Server Accounts, run history, reports; runners deployable via MSI / Mac PKG / shell installer with flags (`-i login pass`, `-k appkey`, `--local-only`, `-d no-daemons`, `-n no-license-agreement`) [9][68]
- **Account Sync** — jobs/groups/options/server-accounts themselves sync across your devices via the GoodSync Account (jobs follow you; a v12 headline feature) [11][28]

---

## 5. Mac app UX

- **Paradigm**: left pane = Job Tree/List (jobs grouped, sortable columns: status, direction, folders, last run); right pane = **Sync Tree** diff with per-row action icons/arrows; toolbar (New Job, Account Sync, AutoRun, Analyze, Sync, L/R folder pickers, direction); bottom log pane (hidden by default); Filters pane [69]. Browse dialog is a 3-stage wizard: pick file-system → credentials → folder tree — doubling as the credential vault ("Server Accounts") [70]
- **Nativeness**: this is a **custom cross-platform UI**, not AppKit. Manual hedges everywhere ("Tools menu (Windows only)", "GoodSync menu (Mac only)", "Filters pane — Windows only"); the 12.9.14 note "integrated the new common cross-platform SyncTree" says the quiet part out loud [22][71]. Reviewers consistently call it dated/non-native: *"UI is simple to use but has a dated design"* (Tom's Guide), non-standard dialogs/buttons on Mac (Appletell), "distinct Windows flavor" (MacTech), "overdesigned" network browser with credentials hidden behind a small person-icon (richtong.org, 2024) [72][73][74][75]
- **Onboarding**: New Job → name → pick Synchronize/Backup → pick Left/Right → Analyze → Sync ("*Yes, it's that easy!*" — a slogan reviewers found optimistic) [76][74]. Install asks for **Full Disk Access**; menu-bar resident mode; "GoodSync" application menu holds account setup, runner service, export/import, check-for-updates [3][71]
- **Modes**: Full window, Mini Mode (Windows), Tray/Menu-bar icon mode — auto-drops to icon when closed with auto jobs [69]
- **Explorer** (separate app): sidebar of service icons (local, LAN, Connect, each cloud), icon-toolbar file manager over all protocols — the stylized screenshot shows the same custom-toolkit look: flat grey rows, yellow folder icons, non-Aqua controls [12]
- Ease-of-use verdict: powerful but jargon-dense ("jobs", "analyze", left/right, "server accounts"); reviewers say it "requires study" and the manual is nearly all prose [72][73]

---

## 6. Strengths and weaknesses (user sentiment)

**Ratings**: MacUpdate **3.5/5** (10 ratings, listing v12.8.6) [75]; Trustpilot page has ~15 reviews skewing negative on support/billing [77]; Capterra/SoftwareAdvice reviews are mixed-positive on capability, negative on support [78][79].

**Praise**:

- Breadth: one tool does LAN shares, NAS, SFTP, WebDAV, P2P and every major cloud; "backup my computers to my NAS and to Backblaze without hassle … also it works on Linux!" [78]
- Real-time + scheduled automation that "just runs": *"every file I save is automatically backed up a few seconds after I save it"* [78]; VSS locked-file copying "effortlessly" on Windows servers [79]
- Versioning/_saved_ safety net and preview-first Analyze [39][78]
- Decade-plus loyalists on both platforms ("using this since 2012 … never had an error") [75]

**Complaints**:

- **Non-native/dated UI and steep jargon** — the most consistent theme across 2010-2024 reviews [72][73][74][75]
- **Reliability bugs**: lockups mid-sync, "Undefined Error: 0", spontaneous mid-sync stops, intermittent errors → "garbage … bloated trashware" (MacUpdate/SoftwareAdvice) [75][78]
- **Footgun**: one-way backup can mass-delete pre-existing destination files with no explicit warning ("before actually beginning the backup, NO warnings appear") [80]
- **Support**: unanswered tickets, generic replies, refund friction [77][80]
- **Subscription resentment + online dependency**: "$30/yr subscription — NO!", latest releases "won't work if your PC is not online" (license check), license reactivation pain after reinstall [75][77][80]
- `_gsdata_` writes failing on NAS/network drives unless you find the hidden credentials icon or the "No _gsdata_" option [75]
- Lock serialization: jobs over the same data can't run concurrently ("lots of failures until a job gets its slot") [79]
- Error messages too cryptic for non-experts [79]

---

## 7. Lessons for Foldover (local/network MVP)

**Adopt:**

1. **Two-phase Analyze → Sync with an editable plan.** The sync tree with per-item direction overrides (and crossed-arrow delete semantics) is GoodSync's best idea — but present it with modern defaults: grouped/flattened view, "N files will be deleted" badges, plain-English explanations.
2. **Persistent per-pair state** — a `_file_state._gs` equivalent enables true two-way diff, deletion propagation, revert detection, and chained sync. Keep it out of the synced folder by default (profile dir / `.foldover` in profile, or hidden xattr on folder) — `_gsdata_` littering synced folders, failing on NAS ACLs, and confusing users is a documented annoyance [34][75].
3. **Safety defaults worth copying verbatim**: `_saved_`-style last-version recycle (or macOS Trash) ON by default, optional `_history_` versioning with timestamped names, safe-copy-via-temp+rename, "changed >50% → don't auto-sync" fuse, per-folder **lock files** so two machines can't sync the same share at once, and a first-sync review step when the destination is non-empty [33][36][38].
4. **Conflict taxonomy**: distinguish modified-both-sides vs delete-vs-change vs case-only vs metadata-only; auto-resolve *weak* conflicts (newer mtime when one side has no history), queue strong ones; conflicted-copy naming `name (Conflicted Copy from HOST on DATE).ext` is proven [49][50].
5. **Trigger set**: FSEvents-driven On-File-Change with ~20 s debounce (cap ~300 s, force after 5 min of churn), On-Volume-Mount (their "On Folders Connect"), periodic, cron-style schedule, and a LaunchAgent runner for logged-out sync — exactly GoodSync's matrix [32]. Bonus differentiator: remote change-notification over your own peer protocol, which GSTP supports and SFTP can't.
6. **Rename/move propagation** (folder move when all children moved) and **time-shift/FAT-2s tolerance** — cheap correctness wins users notice only when absent [43][44].
7. **A scriptable core**: `gsync`-style CLI + readable job export (TIX → use JSON), exit codes that separate "conflicts" from "errors", pre/post-sync hooks and email/webhook notifications, per-job and global logs with retention [62][65][66].
8. **Parallel transfers** for latency-bound remotes (default 3 threads) + resumable uploads + explicit bandwidth cap [10][33].
9. **Peer protocol with relay fallback**: GSTP's Mediator(5-min check-in)/UPnP/manual-forward/Forwarder-relay ladder is the full NAT-traversal playbook for Mac-to-Mac sync; Foldover's LAN-first scope can start at local discovery (their UDP 33338/33339 broadcast = Bonjour/mDNS in our world) [61].

**Avoid / do better:**

1. **Don't ship a cross-platform UI on macOS.** "Dated, Windows-flavored, non-standard controls" is GoodSync's #1 recurring criticism across 15 years of reviews — Foldover's native SwiftUI/AppKit shell is a direct competitive weapon [72][73][75].
2. **Don't make local sync depend on an online account.** Offline-license complaints and OTP/mediator machinery create real resentment; keep the peer pairing local (device keys, Bonjour) and treat any account purely as optional convenience [77][80].
3. **Warn before destructive first syncs.** The "one-way job silently deleted my destination" horror story is the exact UX fold-over moment — preview deletions prominently, require confirmation above a threshold, default to recycle/versioning [80].
4. **Credentials as first-class objects.** GoodSync fixed the "re-enter creds per job" problem with Server Accounts, but hid them behind a cryptic icon — make shares/peers/keys a visible, editable, shared entity list [28][75][78].
5. **Meaningful errors.** "Undefined Error: 0" is a meme for a reason; surface cause + affected file + remediation, and distinguish terminal/non-terminal like their exit-code scheme does internally [62][79].
6. **Right-size the free tier and pricing model.** 3 jobs/100 files is basically a demo; a subscription for local sync generates Refund/Cancel-department tickets — consider perpetual+paid-upgrades or a generous free local-sync tier (the DPReview thread shows users actively hunting "no subscriptions" tools) [75][81].
7. **Fill their gaps**: no NFS; block-level only inside GSTP (a Foldover delta-sync over SFTP would beat them); no documented Finder-tag/resource-fork guarantees; SMB credentials UX friction on NAS [54][75].
8. **Concurrency ergonomics**: folder-level lock files serialize jobs that touch the same folder — fine for safety, but let independent sub-jobs proceed and surface "waiting for lock" in the UI instead of failing jobs [45][79].

---

## Sources

1. https://www.goodsync.com/features — feature list, platform/protocol/cloud matrix, "Copy Extended Attributes… custom icons on Mac"
2. https://www.goodsync.com/free-vs-pro — Free vs Paid limits (3 jobs, 100 files, connections)
3. https://www.goodsync.com/download?os=macos — Mac builds: 10.13+, Intel+M1, zip/dmg/pkg/sh, ver 12.11.9, Full Disk Access note, Workstation PKG
4. https://www.goodsync.com/faq-licensing — 1 license/device, 5 devices per purchase, activation flow, GS Storage trial, GoodSync2Go special license
5. https://websitebuilder.org/reviews/goodsync/ — "$29.95/year … up to five devices" (third-party confirmation)
6. https://findalternative.net/item/goodsync/pricing — Personal $29.95/yr, 5 devices
7. https://www.goodsync.com/business-compare — Workstation $49.95/yr, Server $499.95/yr, Control Center pricing, ≥10-machine guidance
8. https://www.goodsync.com/business-server — Server OS edition (backup/sync/replication)
9. https://www.goodsync.com/goodsync-control-center-manual — Control Center: jobs.goodsync.com, Runners=user@computer, authorization, reports
10. https://www.goodsync.com/goodsync-connect — Connect benefits: block-level, resume, E2E AES-256, mediator/forwarder, free with product
11. https://www.goodsync.com/version12 — v12 account-centric licensing, Account Sync of jobs/options
12. https://www.goodsync.com/goodsync-explorer — Explorer scope and supported systems
13. /images/backgrounds/goodsync_explorer.webp on goodsync.com — Explorer UI screenshot (custom toolkit look)
14. https://www.goodsync.com/goodsync-storage — RealDisk storage, trial 10/100 GB ≤15 days
15. https://www.bestbackupreviews.com/reviews/goodsync-review/ — review noting $29.95/yr 5 devices, storage from $9.99/yr (third-party, may be stale)
16. https://www.goodsync.com/manual — GoodSync2Go section; GSCP section (same manual)
17. https://www.goodsync.com/for-wd-users — WD edition Free vs Premium matrix
18. https://apps.apple.com/us/app/goodsync/id550283470 — iOS app (client+server roles)
19. https://www.goodsync.com/goodsync-connect-manual — GSTP protocol, addressing, ports, OTP, users
20. https://goodsync.macupdate.com/ — 3.5/5, $29.95, subscription-resentment review quotes
21. https://www.goodsync.com/news-windows — 12.11.9 (2026-10-06) release notes; Sib-SMB DFS/KRB5
22. https://www.goodsync.com/news-mac — Mac release notes incl. cross-platform SyncTree (12.9.14), own SFTP client (12.9.6), macOS 26 fixes
23. https://www.goodsync.com/news-nas-linux — NAS/Linux versions, Jobs Web UI
24. https://www.goodsync.com/news-android — Android versions
25. https://www.goodsync.com/goodsync-version.txt — machine-readable current-version record (12.11.9, Oct-06-2026)
26. https://www.goodsync.com/press-releases/20122102-GS-9-Release.pdf — GoodSync 9 (2012): GS Connect launch, $29.95+$9.95 perpetual pricing
27. https://www.goodsync.com/manual — Jobs/Analyze→Sync workflow, Job Templates, Groups (same manual, respective sections)
28. https://www.goodsync.com/manual — "Server Accounts and Account Sync" (central credentials, shared across jobs)
29. https://www.macworld.com/article/219755/review-goodsyncs-features-are-rich-but-require-study.html — Macworld review: job jargon, per-job credential re-entry (pre-Server-Accounts era)
30. https://www.goodsync.com/manual — Actions chapter (Copy File/Time/NameCase/SyncState/DoNotCopy/Error/Conflict), icon semantics
31. https://www.goodsync.com/manual — Job Options → General (job types, Propagate Deletions, Fast 1-way NoState NoLock, Move Mode, VSS)
32. https://www.goodsync.com/manual — Job Options → Auto (all triggers, delays, unattended runner, parallel rules)
33. https://www.goodsync.com/manual — Speed/Limits (3 threads, 120 s reconnect, KB/s cap), Logs options
34. https://help.goodsync.com/hc/en-us/articles/44434603540237-What-is-the-gsdata-Folder — `_file_state._gs`, `_insync.gsl`, `_saved_`, `_history_`, logs
35. https://help.goodsync.com/hc/en-us/articles/34117066758541-Recycled-History — saved/history semantics, purge defaults, Mac "Make gsdata visible"
36. https://help.goodsync.com/hc/en-us/articles/115003939392-Job-Options — No _gsdata_, Do-Not-List-Folders, Fast 1-way, compare flags
37. https://help.goodsync.com/hc/en-us/articles/360002905091-Special-Operations — Delete State Files / Delete GS Data
38. https://help.goodsync.com/hc/en-us/articles/115003433032-GoodSync-Connect-GSTP-Backup-Sync — "review Analyze results before the first Sync, especially … destination that already contains files"
39. https://help.goodsync.com/hc/en-us/articles/115003536611-Command-Line-GoodSync-Job-GSYNC — analyze-before-sync guidance
40. https://www.goodsync.com/manual — Analyze options (checksums, estimate space, move/rename detection)
41. https://www.goodsync.com/manual — "Synchronization" algorithm outline; "Why Synchronization is not trivial"; Chained Synchronization
42. https://www.goodsync.com/manual — "detects … replace the newer version of the file with its older version"
43. https://www.goodsync.com/manual — "Detect File and Folder Moves and Renames"
44. https://www.goodsync.com/manual — Advanced Topics → Managing time shifts; FAT ±2 s; mod-time translation (features list)
45. https://www.goodsync.com/manual — Auto → "Wait for Locks to clear" (20 min default)
46. https://www.goodsync.com/manual — Sync options → "Do not Sync files that changed between Analyze and Sync"; Advanced Auto 50% fuse
47. https://www.goodsync.com/manual — "Uber-unlocked mode", VSS snapshot semantics
48. https://www.goodsync.com/goodsync-connect-manual — Block-level Sync section
49. https://www.goodsync.com/manual — Conflict action / Types of Conflicts
50. https://www.goodsync.com/manual — Errors/Conflicts options (weak/strong resolution, Conflicted-Copy naming, balloon notifications)
51. https://help.goodsync.com/hc/en-us/articles/115003228952-Filters — filter syntax, exist-x, context-menu filters
52. https://help.goodsync.com/hc/en-us/articles/34056184031501-Sync — Copy original filesystem names (Mac), Copy Permissions, Copy ACL NTFS-only note
53. https://help.goodsync.com/hc/en-us/articles/115003360272-Left-Side-Right-Side — Compare Extended Attributes (Mac only)
54. https://www.goodsync.com/manual — File Systems chapter (no NFS entry exists)
55. https://www.goodsync.com/manual — Net Shares Sib-SMB section
56. https://www.goodsync.com/manual — Net Shares Win-SMB section
57. https://www.goodsync.com/manual — Apple Filing Protocol (AFP) section
58. https://www.goodsync.com/manual — SFTP section (SSH-2, keys, ssh-agent, host-key pinning, feature list); FTP section (ftp[s]://)
59. https://www.goodsync.com/manual — WebDAV and One File sections
60. https://www.goodsync.com/manual — Media Devices (MTP) section
61. https://www.goodsync.com/goodsync-connect-manual — Ports used (33333/11000/80+443/UDP 33338-9/22222/33334), UPnP, Forwarder, Web UI
62. https://www.goodsync.com/manual — GSYNC chapter (commands, options, return codes 0-14)
63. https://help.goodsync.com/hc/en-us/articles/115003536611-Command-Line-GoodSync-Job-GSYNC — GSYNC help-center mirror
64. https://www.goodsync.com/manual — GSCP command list
65. https://www.goodsync.com/manual — TIX format chapter
66. https://www.goodsync.com/manual — Job Options → Scripts (email syntax, %VARS%, nowait:/errors:)
67. https://www.goodsync.com/manual — Program Options → Logs; → Server (local server Web UI port 11000, log levels)
68. https://www.goodsync.com/manual — GoodSync mass deployment (MSI / Mac shell-script flags)
69. https://www.goodsync.com/manual — User Interface overview (Job Tree, Sync Tree, Toolbar, Log pane, Mini/Tray modes)
70. https://www.goodsync.com/manual — Browse dialog stages
71. https://www.goodsync.com/manual — "GoodSync commands (Mac only)" (Preferences ⌘,, Account Setup, Runner Service, Make-gsdata-visible family of Mac-only menu items)
72. https://www.tomsguide.com/reviews/goodsync-review — "dated design", folder navigation complaint
73. https://www.macobserver.com/mac/goodsync-file-synchronization/ — manual is text-heavy, no in-app docs
74. https://www.mactech.com/2012/11/09/goodsync-a-good-back-up-sync-solution-for-macs/ — "distinct Windows flavor"; http://techwizardguy.blogspot.com/2010/12/appletell-reviews-goodsync-pro-for-mac.html — non-standard Mac controls
75. https://goodsync.macupdate.com/ + https://richtong.org/2024/06/29/nas-folder-and-file-comparisons-when-moving-and-deleting/ — MacUpdate 3.5★/lockup/"Undefined Error" quotes; _gsdata_-on-NAS credential friction
76. https://www.goodsync.com/tutorial — onboarding steps, "Yes, it's that easy!"
77. https://www.trustpilot.com/review/goodsync.com — subscription-cancel, support-silence, offline-license complaints
78. https://www.softwareadvice.com/file-sync/goodsync-profile/reviews/ — LAN/NAS praise, lockup/"Undefined Error" complaints, lock-slot failures
79. https://www.capterra.com/p/10010689/GoodSync/reviews/ — VSS praise, confusing errors, same-data locking complaint
80. https://download.cnet.com/goodsync/3000-2248_4-10508799.html + Amazon/GreatSoftwareCenter archived reviews — deletion-without-warning data-loss story; license-reactivation gripes
81. https://www.dpreview.com/forums/threads/best-file-sync-software.4752849/ — post-AllwaySync market, "no subscriptions" sentiment, GoodSync tethered-shooting use case

### Confidence notes
- All "GoodSync does X" statements above come from the official manual/Connect manual/help-center/release notes fetched 2026-10-16 unless marked.
- UNVERIFIED: exact GS Storage price tiers in Oct 2026; whether Free tier includes auto triggers (the comparison table's checkmarks didn't render — treat automation-in-Free as unconfirmed); Finder-tag/resource-fork propagation (implied by xattr copying, not documented by name); NFS support (absent from docs, never confirmed as a rejected feature); the "0 allowed connections" row meaning on the Free-vs-Personal table.
- GoodSync's actual reconciliation algorithm is explicitly proprietary ("several important inventions and trade secrets") — the outline in §2 is the documented behavior, not the internals.
