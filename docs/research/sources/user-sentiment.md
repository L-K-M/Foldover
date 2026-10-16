> Raw AI research report (devin, SWE-2 max), 2026-10-16. Unedited. Corrections: [fact-checks](../README.md#corrections).

# Real-user sentiment on Mac file-sync tools — research report

*Compiled 2026-10-16. Sources: Reddit/forum threads (MacRumors, MPU Talk, TidBITS Talk, Hacker News, Apple Community), review aggregators (MacUpdate, Trustpilot, Capterra/G2/SoftwareAdvice, Setapp), app-store review pages, vendor docs/release notes, and press reviews (Macworld, MacTech, MacObserver, TidBITS). Inline [n] markers map to the numbered Sources list. Time-sensitive facts carry dates; anything I could not confirm is marked UNVERIFIED.*

---

## 1. Per-app sentiment

### 1.1 GoodSync (Siber Systems)

**Product snapshot.** GoodSync for Mac is a Windows-first port (Siber Systems also makes RoboForm). Current Mac version is 12.x (download page showed 12.11.9; MacUpdate listed 12.8.6.6 as of 2025-03-17) [1][2]. It supports local folders, mounted servers, FTP/SFTP/WebDAV, S3, cloud drives, NAS, and its own GoodSync Connect P2P transport [3]. Free tier is capped at 3 jobs × 100 files [4]. Licensing switched from perpetual to subscription: MacUpdate lists "$29.95 / Subscription" monetization [2]; a third-party review lists GoodSync Personal at $29.95/yr for up to 5 devices [5]; business "Workstation" tier is $49.95/yr [6].

**Top praised aspects**

- *Breadth of endpoints.* FTP, SFTP, WebDAV, S3, NAS, another computer via GoodSync Connect — "sync over a network, to an FTP or SFTP server… Amazon S3… Easy to use Exclude and Include filters" [3]. Chained sync via a USB drive between unconnected Macs was a praised trick [3].
- *Automation.* Scheduled jobs, periodic syncs, and "run when a particular folder is connected" (plug-in trigger) were called the "biggest strength" [7].
- *Analyze-then-preview.* The Analyze pass shows what will be copied/deleted before Sync runs [8][9].
- *It does work for some.* "I've been using this since 2012 and have never had an error or issue" (MacUpdate, 2017-10-06) [2]. A photographer measured ~2.5 GB/s sustained GoodSync copies between Thunderbolt 4 NVMe enclosures (DPReview, 2025) [10]. Capterra aggregate sits ~4.4/5, pulled up mostly by Windows users [11].
- *Safety net exists.* By default, deleted/replaced files go to a `_gsdata_/_saved_` folder (30-day auto-cleanup), with optional `_history_` versioning [8][12].

**Top complaints**

- *Subscription switch is the single loudest grievance.* MacUpdate (2021-12-24): "the 'real-deal': SUBSCRIPTION, $30/yr. I'd purchase it for $30, but as a SUBSCRIPTION - NO!" [2]. G2 thread: GoodSync "discontinued the sale of perpetual licenses… the whole point of Goodsync was my data, stored on my devices, using software that I owned" [13]. A v9 buyer: "I bought V9 for Mac some years ago and now I can not use it anymore, so I have lost 30 dollars" [13]. Allway Sync lifetime-license holders were stranded after Siber Systems absorbed it: "Goodsync told us that they were no longer responsible… Goodsync's behavior is unlawful and not customer-oriented" (Trustpilot) [14]. Korean users noted renewal costs *more* than buying new (Clien, 2025-04) [15]. A MacRumors poster left GoodSync in ~2020 "until they went to a subscription service" [16].
- *Mac build reliability.* The canonical bad review (MacUpdate 2017-08-23, echoed on SoftwareAdvice): "Locks up while attempting to sync to a folder… 'Undefined Error: 0.' Constantly… Sync jobs just fail, spontaneously, mid-sync… The network browser is overdesigned and overcomplicated" [2][17]. An earlier review: "Sync settings are consistently lost and I constantly find that a job has not been executed because of that" (2013) [2].
- *Clunky UX, Windows flavor.* "The instructions could be, should be, clearer (they have a distinct Windows flavor)" (MacTech 2012) [7]. Macworld: "the learning curve… is challenging. This program isn't for beginners"; docs described Windows versions [18].
- *Network share handling.* "If you disconnect/re-connect to a network… GoodSync will not connect for you. You have to connect through the Finder" [2]. It depends on mounted shares rather than managing mounts itself.
- *Support reputation.* "Prompt-but-generic-and-totally-unhelpful feedback" [2]; "Arrogant customer support" (Capterra) [11]; "Horrible. They are thieves" (Trustpilot re: cancelled version) [14].
- *Folder litter.* The hidden `_gsdata_` state/log/saved folder is created inside every synced folder [12].

**Data-loss / corruption reports**

- Deletion propagation is by design (bidirectional sync deletes on the other side) [9]; the `_saved_` recycle safety net is on by default, but "Fast 1-way mode does not use the Recycle Bin. Any files deleted in this mode will be lost" [8].
- Synology forum (2016-10-03): a mirror job to a Synology NAS suddenly wanted to delete ~600 GB on target and re-copy it, unchanged files being treated as different — likely a state/timestamp mismatch over SMB; the same happened with ViceVersa, suggesting a NAS-metadata rather than app-specific cause [19].
- SoftwareAdvice reviewer: "automatic exception handling can really cause problem sometimes. One of my files was deleted not to be found again" [17].
- Trustpilot: "Do NOT buy GoodSync software, it is a scam! …simply not working, certainly on a Mac!" with unresponsive support [14].

### 1.2 ChronoSync & ChronoSync Express (Econ Technologies)

**Product snapshot.** ChronoSync 12.0 released 2025-11-07; 12.0.3 released 2026-09-14 with macOS Tahoe support [20][21]. $49.99 "Lifetime subscription… free updates for life," ChronoAgent $19.99, bundle $59.98, 15-day trial [22][23]. ChronoSync Express (Mac App Store / Setapp, $29.99) is the sandboxed entry version [24][25]. Express 1.3.8 (2022-09-22) added "Apple Silicon Native" [26].

**Top praised aspects**

- *Reliability.* Long-term users dominate the sentiment: "I've been using ChronoSync Express since 2014 and it has performed flawlessly" [27]; "Been using it since around 2009/10… great little program" [16]; "ChronoSync made the most sense to me. I've been using it for years… Amazingly good product, helpful support, lifetime updates" [27]. Macworld's verdict (2024-12-12): "quick, speedy, reliable and reasonably priced… complete control without overwhelming users" [28].
- *Scheduling depth.* Calendar schedules, volume-mount triggers, wake-to-run, event triggers, and (v12) remote ChronoAgent-triggered runs and scheduled validations [20][21][29]. App Store review (2025-07-10): "Mine are run when my drive mounts… when Chronosync starts up, and a standard weekly sync. It was so much easier and clearer to set up" [30].
- *Trial/preview + safety.* Trial Sync ("see exactly what will happen before it happens"), archive of deleted/overwritten files, safe-copy + verification [24]. The Analyze panel previews every pair [31].
- *Speed.* SmartScan (full version); a MacUpdate tester found ChronoSync "way faster" than CCC on a non-system backup; ChronoAgent "really made the grade" for Mac-to-Mac [32].
- *Accessibility.* TidBITS commenter on 11.0.3: "damn-near perfect for VO users… now it's essentially perfect" [33].
- *Pricing goodwill.* "Never had to pay for an upgrade" [34]; "I've been using ChronoSync since 2007… what I paid was a bargain" [35]. Econ markets "all updates are free. Always. No user is left behind" [20].

**Top complaints**

- *Learning curve / dated-feeling UI.* "Aside from being fussy it works" [35]; "the interface was too confusing, and the options were too complex" [36]; Macworld nits: awkward navigation home, no ETA in the progress bar, tutorial videos six years stale [28]. fainimade.blog (2022-08): "Chronosync kinda sucks… lacking real information about expected completion time and the real completed vs uncompleted files count" (that author built an alternative, "Mirror") [37].
- *ChronoAgent upsell.* Best Mac-to-Mac sync requires buying ChronoAgent per machine [38]; MPU users debated whether it was needed [39].
- *Express's sandbox limits are long.* No admin/root access to other users' files, no bootable backups, no wake-from-sleep scheduling, no pre/post scripts, no SmartScan, no Parallel Operations, no resume of interrupted syncs, Keychain-only server credentials, no File Validator, no cloud connectors [40]. Setapp reviewers: "couldn't sync to an SMB share"; two-way sync "takes way too long and too much data is transfered"; a volume-mount schedule showed "this task will never be executed" (French review) [25].
- *Support tone.* "On the 2-3 occasions I've contacted their support, they've made me feel like a burden" (MPU, 2025) [35].
- *NAS edge cases.* MacUpdate commenter: errors `-50` on many files (esp. `.webloc`) when syncing to a ZFS dataset — fine on HFS/APFS [32].

**Data-loss / corruption reports**

- No widespread "ate my data" stories found — ChronoSync's failure mode is crashing rather than corrupting. TidBITS commenter (2019-11-25): 4.9.6 "crashed spectacularly upon hitting the sync button," scheduler implicated [33].
- The scariest item: an M1 mini user's machine *crashed to fsck-requiring level* every time a specific 170 GB Pictures→server task ran; commenters suspected the macOS SMB kernel extension (kernel panics) rather than ChronoSync itself (MPU, 2022-11) [41]. ChronoSync Express 1.4.0 (2024-04-29) shipped "workarounds to SMB large file transfer limitations that have a tendency to cause kernel panics" — i.e., Apple-side SMB bugs blamed [26]. ChronoSync 12.0.2 added handling for "NFC unicode filenames on SFTP" (2026-05-13) [21].
- Express 1.3.7 fixed a real sync-correctness bug: "on an initial sync, if the data size trigger was in effect, it could result in an older file replacing a younger one" (2021-11-09) [26].

### 1.3 SyncMate (Eltima → Electronic Team)

**Product snapshot.** SyncMate 8 (latest 8.11.604, 2026-08-27 per vendor page) syncs folders, contacts, calendars, media between Mac and Android/iOS/MTP/cloud/other Macs. Free edition vs Expert ($39.95 personal/2 Macs; family $59.95/6; business $99.95/10; unlimited $199.95) [42][43]. Not on the Mac App Store (the old "SyncMate Plus" MAS build was a crippled OS X 10.6–10.8-era edition) [44][45]. MacUpdate: 3.4/5 across 178 ratings, 197 reviews [46]. Vendor claims "4.5 rank based on 795+ users" — on its own site [43].

**Top praised aspects**

- *Ambitious scope.* The only Mac tool covering Android devices, MTP, other Macs, cloud accounts, contacts/calendars/SMS in one app [42][45]. MacStories (2011): it made Android↔Mac sync "completely pretty easy" [47].
- *When it works.* "I use Syncmate to Synchronise large photobiblios (100.000 pics and more) on different Drives. Works great. Also super Support" (MacUpdate) [46]; multi-year users exist [48].
- *Feature naming users want.* AutoSync, Mount Disk, background sync — the right checklist items (all gated to Expert) [44].

**Top complaints**

- *Reliability and it gets ugly.* MacUpdate: "everything seemed fine until my server gave me a warning it was out of storage… sync mate had made copious copies of my files to the point 25tb was taken up by about 200gb of data" [46]. MacRumors (2020-05-30): an 82 GB folder sync "bogged down… spinning wheel… my synch folders were all messed up. I ended up erasing one of them" — after the app had *no trial* [16]. Electronic Team's own review site: "It used to work great on 10.11 but no way on 10.14: SBB until it fills the whole available disk space" [48].
- *Slow, brute-force approach.* "On the slow side, and when it syncs it has to check all of the files on both sides" (MacRumors) [16]. No persistent state for fast incremental updates, unlike GoodSync's state file.
- *Sleep kills unattended syncs.* "It takes several hours… the sync almost always fails in the end if either computer goes into sleep mode. So unattended syncing just won't work. Customer service says I need to use a third party app to keep machines awake" [46].
- *Refund refusal pattern.* "Reached out to their support and they said NO REFUNDS… RUN AWAY FROM THIS APP!" [46]; a long Trustpilot account describes weeks of circular troubleshooting and denial of a refund under a "14 day honor-based guarantee" (Eltima/Electronic Team) [49]. Trustpilot's sync-mac.com page (13 reviews) is polluted by reviews meant for the unrelated Sync.com — a sign of how neglected the product's reputation management is [49].
- *Rotting features.* Contacts/calendars sync broken after Apple API changes ("apple put a stop to that and sync mate hasn't been altered to reflect these changes") [46]; Android module must be sideloaded and targets old Android; "Does not connect to an Android phone from an M1 Mac Mini" [50]. Windows Mobile support silently dropped [50].
- *Creepy marketing math.* Vendor pages tout "4.5 rank based on 795+ users" and "5.0 based on 978 user reviews" on their own domains while MacUpdate shows 3.4 [42][43][46].

**Data-loss / corruption reports**

- The 25 TB duplication bug [46] and the "sync folders all messed up" incident [16] are the two worst concrete reports — both stem from a buggy state engine plus weak verification, not from deletion-propagation semantics. The 10.14 disk-filling bug is a third [48].

---

## 2. "Best sync app for Mac" threads (2023–2026, plus anchor older threads) — what gets recommended and why

**MacRumors "2x Macs - how best to sync" (2024-02):** iCloud as the no-brainer; for local-only sync the picks are Syncthing (open source) and Resilio ("paid, generally works better… maintains extended attributes e.g. Finder tags"), and "Chronosync is the best for that and is built for the Mac. Maybe a little costly, but I have never had to pay for an upgrade" [34].

**MPU Talk "Sync folders between two M1 Macs" (2022-04):** the canonical distinction — Resilio Sync for *continuous* real-time LAN sync vs ChronoSync for *scheduled* runs; users warn Resilio's "Remove all synced files" option is "prone to error" [51].

**MPU Talk "Looking for best way to sync files back and forth" (2024-08):** ChronoSync first answer; CCC noted as *one-way only* ("There's no two-way sync"); rsync still shipped in Sonoma then [39].

**MPU Talk "SyncThing - pro or con?" (2023-12):** Syncthing praised for LAN-first sync and an active forum; the disliked bit is reliance on an external discovery server (self-hostable) [52].

**MPU Talk "File syncing – my journey and result" (2025-01):** SugarSync → Resilio ("worked and was fast") → Synology Drive for 5 years, "very fast and reliable… a conflict file every few months is noise" — if you own a Synology, Drive is the default answer [53]. Counterpoint: German Synology forum calls Drive "superlangsam" for a 2 TB Lightroom/80k-file workload [54].

**MPU Talk "Looking for Mac Backup Software – Not ChronoSync" (2025-10):** ChronoSync loyalty cracks over licensing policy (2-pack discount denied post-purchase) and support tone; CCC + Arq named as the alternate stack [35].

**MPU Talk "Chronosync replacement (potentially) needed" (2022-11):** after the M1 mini kernel-panic saga, answers converge on rsync/rsyncUI and caution that macOS+SMB+hardware may be the real culprit [41].

**TidBITS Talk "Synchronizing folders on main and backup drives" (2025-12):** ChronoSync one-way sync + archive of replacements; rsync `-avE` + cron as the free path; CCC folder tasks with SafetyNet on/off depending on whether deletions should propagate [55].

**TidBITS Talk "Mac to Mac sync" / "Keeping two Macs in sync":** ChronoSync+ChronoAgent for folders; warning that syncing *apps and their state* is "weirdly much harder" and not really solved [56][57]. Dropbox praised for "always on" invisibility vs ChronoSync's run-it-yourself model [56].

**MacRumors "Best Mac File Sync Program?" (2020-05, still the reference thread):** FreeFileSync ("fast, reliable and free… comes with Realtimesync"), ChronoSync ("can monitor for file system changes"), and the cautionary SyncMate tale quoted above [16].

**MacRumors "Sync Two Folders" (2019):** SyncTime recommended for the common case — "indie developer, who fixes bugs quickly and actually implements user requests"; GoodSync as the "enterprise"-flavored granular option; Hazel for automation; Mountain Duck smart-sync for SFTP/WebDAV; DeltaWalker for manual sync [58].

**Hacker News (FreeFileSync thread, 2021-12):** the killer feature vote goes to *pre-run inspection and per-file conflict resolution* — "for some use cases I need 'eyes'… has let me catch accidental deletes countless times"; also praised for fast scanning and true cross-platform parity [59]. HN Syncthing threads: beloved for LAN-first multi-device sync with ignore patterns and versioning; iOS requires Möbius Sync/Synctrain workarounds; on macOS the official story is the syncthing-macos wrapper (menu-bar launcher around the daemon + web UI), which reached an official v2 release 2026-02 (v2.0.14-1) [60][61][62][63].

**Apple Community "sync two Macs without the internet":** FoldersSynchronizer (Softobe) + local File Sharing is the given answer; warns that in two-way sync "the earlier [modification] will either be deleted" — conflicts on you to manage; Dropbox LAN sync noted [64].

**MacUser.de (2024-12, German):** "Synctime can version but can't watch folders; FreeFileSync can do both but didn't run smoothly for me on Mac; testing ChronoSync — looks promising"; fallback = launchd + rsync `--backup` for versioning [65].

**EroScripts (2025-04):** "AllwaySync used to be my go-to, but they moved to a subscription model"; MirrorFolder ($45 perpetual) accepted happily — "the moment the drive appears it starts syncing" [66]. (Windows-centric thread, but the pricing sentiment generalizes.)

**Newer entrants users run into:** SyncTime (~$10, perpetual, MAS + free Lite; background watch, on-connect auto sync, menu bar, Shortcuts/AppleScript) [67][68]; Sync Folders (MAS, 3.9★/17 ratings, iOS+Mac) [69]; SyncTwoFolders, Get Backup Pro, SmartBackup, FolderWatch, Compare & Sync Folders (Apple Community list) [70]; ForkLift 4's dual-pane Sync-to with a preview of adds/updates/deletes — praised as "rsync with an easy to use UI… preview before I run it" [71][72]; Mountain Duck smart-sync [58]; DuoBolt, a newer paid app whose whole pitch is *compare-by-content first, conflicts wait for you* [73]; Unison — still maintained (2.54.0, ~2.5 maintainers) but CLI-heavy, version-matching finicky, and its Homebrew cask was disabled 2026-09-01 for failing Gatekeeper checks [74][75]; rsync GUIs rsyncUI/RsyncOSX [39][76]. **macOS rsync caveat:** Apple shipped rsync 2.6.9 (2006) until Sequoia, swapped in openrsync in 15.x, and removed the samba rsync entirely in 15.4 — users report hangs/'too many open files', so "just use rsync" now means installing rsync 3.x via brew anyway [77][78].

**Pattern across threads:** the answers cluster into (a) scheduled/previewable tools (ChronoSync, FFS, SyncTime, ForkLift, GoodSync), (b) continuous P2P daemons (Syncthing, Resilio), (c) DIY rsync/launchd, (d) NAS-vendor clients (Synology Drive). Nobody recommends SyncMate. GoodSync's recommendation rate collapsed after the subscription switch.

---

## 3. Unmet needs and wish-list items (ranked by frequency/heat)

1. **NAS/SMB reliability and speed.** The single hottest pain. macOS's own SMB client is blamed for Finder hangs, stalls mid-copy, `Error code -36`, dropped shares on Sequoia ("problem completely fixed as long as Apple's firewall is off" — Apple Discussions, 2024) [79], QuickLook-triggered freezes and corrupt-on-render files over QNAP SMB, and kernel panics [80][81]. Community workarounds are `/etc/nsmb.conf` incantations (`signing_required=no`, `notify_off=yes`, `port445=no_netbios`, `mc_prefer_wired`) and `DSDontWriteNetworkStores` to stop `.DS_Store` litter [82]. Sync apps inherit all of it when they just ride mounted shares (ChronoSync's kernel-panic workaround notes [26]; GoodSync "connect through Finder first" [2]; ZFS `-50` errors [32]).
2. **Many small files / big trees, fast.** Synology Drive "superlangsam" on 80k photos/2 TB [54]; SyncMate "checks all of the files on both sides" [16]; GoodSync "slow for large folders" [17]. Meanwhile FFS's scan speed and DB-backed change tracking are lauded [59]; ChronoSync's SmartScan exists but is gated to the full version and absent from Express [40].
3. **Clear, human-readable conflict handling.** Users want to *see* the diff plan: FFS's preview tree is called the killer feature [59]; DuoBolt's entire positioning is "nothing is copied until you review, conflicts wait for you" [73]; ChronoSync Trial Sync [24]; ForkLift's pre-sync add/update/delete preview [71][72]. Negative space: Synology Drive emits "conflict-…" files with no resolution UI [53]; iCloud silently lost Obsidian revisions for one user [83]; Resilio's "Remove all synced files" is "prone to error" [51].
4. **Trustworthy previews + safe defaults.** Mirror-mode deletes are the recurring horror (SyncMate's 25 TB bug [46]; GoodSync mirror re-copy [19]). Users want: dry-run, archive/trash of replaced-and-deleted files (ChronoSync archive, GoodSync `_saved_`, CCC SafetyNet), and explicit consent before mass deletions.
5. **Native, modern Mac UI — and real Apple Silicon.** Recurring adjectives: GoodSync "Windows flavor" [7]/dated; ChronoSync "fussy" [35]; FFS's 10.25 tree-view redesign got pilloried on its own forum ("Horrible interface… reverted to 10.24") [84], and pre-11.12 it ran under Rosetta with memory blow-outs [85]; SyncMate "in the years gotten" [86]. A SwiftUI-native app is a visible differentiator.
6. **No subscription.** See §4 — this is now a purchase criterion, not a preference.
7. **Menu-bar presence & glanceable status.** SyncTime runs from Dock or menu bar [67]; syncthing-macos is essentially a menu-bar launcher and users explicitly want "a menubar icon" [61]; ChronoSync Express can "run tasks… from system menubar" [24].
8. **Mac-to-Mac LAN sync without cloud.** Recurring explicit ask: "like iCloud without going through the internet" [64]; "2x Macs — how best to sync" [34]; "keep two folders in sync… when I take my computer off the network" [58]. Today's answers tax users (ChronoAgent $19.99/Mac, or Syncthing config). A built-in, zero-config LAN peer mode (AirDrop-grade UX) is unmet.
9. **Sync on drive plug-in.** Loved wherever it exists: "the moment the drive appears it starts syncing" [66]; ChronoSync volume-mount schedules [30]; SyncTime "start to sync automatically whenever source and destination become available" [67]; GoodSync on-folder-connect [3]. Absent or flaky elsewhere.
10. **Real-time change watching.** Users distinguish scheduled vs continuous sync explicitly [51][16]. SyncTime background monitoring [67], FFS RealTimeSync companion [16], FolderWatch [70] serve this; gaps remain for network destinations.
11. **macOS metadata fidelity.** Finder tags/xattrs (Resilio preserves them, praised [34]), `.DS_Store` noise on NAS [82], Unicode NFC/NFD mismatches on SFTP/SMB (ChronoSync 12.0.2 fix [21]), package files (ChronoSync's dissection/aggregation [24]), timestamp granularity differences across APFS/exFAT/SMB (DuoBolt's whole raison d'être [73]).
12. **Selective/partial sync.** Resilio selective sync and Synctrain's on-demand streaming on Mac/iOS are called out as differentiators [51][87]; ChronoSync Express syncs whole trees.
13. **Sleep/power awareness.** SyncMate's "fails if either computer goes into sleep mode" [46]; users want power assertions during runs and resume-after-interruption (Express lacks resume [40]; ChronoSync's "pick up where it left off" praised on GoodSync side [3]).

---

## 4. Pricing expectations and attitudes

- **Sync utilities are in the "should be perpetual" bucket.** "As soon as I see 'subscription,' alarms go off" (iDownloadBlog on subscription fatigue) [88]. The GoodSync backlash is the case study: perpetual→subscription conversion produced refund demands, "unlawful" claims from Allway Sync lifetime holders, and visible churn to FFS/ChronoSync/SyncMate(!!) [2][13][14][16].
- **Perceived fair anchors:** ChronoSync $49.99 *with lifetime updates* is the gold-standard deal ("never had to pay for an upgrade" [34]; "a bargain" since 2007 [35]); ChronoSync Express $29.99 MAS/Setapp [25][30]; CCC $49.99 Personal & Household with ~50%-off paid upgrades ($25) — "a bargain compared with the typical subscription" (mjtsai) [89][90]; SyncTime ~$10 [68]; Hazel $42 single/$65 family, $20 upgrades [91]; SyncMate $39.95/2-Mac perpetual Expert [44]; MirrorFolder $45 perpetual fine [66]. FreeFileSync free with an optional donation "Supporter Edition" (parallel copy etc. gated; some donation-payment friction reported) [92].
- **Price ceiling is low.** $30–50 one-time is the comfort zone for a consumer/prosumer sync tool; beyond that users expect either enterprise features or fleet licensing. GoodSync's $29.95/yr personal or $49.95/yr workstation is judged "excessive compared to the competition" [11][2][6].
- **App Store vs direct.** Express's MAS version is the cautionary tale: sandboxing forces a long feature-cut list (no admin access, no scripts, no SmartScan, no parallel ops, no resume, Keychain-only creds) [40], and review velocity is so low the US store shows no aggregate rating [30]. Power users learn to buy direct (MacGeneration: "mieux vaut acheter ChronoSync tout court") [93]. Setapp inclusion is accepted as a legit alternative channel [25]. Unsandboxed direct build + optional MAS-lite is the established pattern.
- **Licensing trust matters as much as price.** Stranding paid users (GoodSync v9, Allway Sync, Paragon NTFS, Rosetta Stone precedents) creates outsized anger [13][14][94]. "Lifetime" language must be honored or it becomes a liability.
- **Free tiers set expectations.** FFS, Syncthing, Resilio's free personal tier [51], SyncTime Lite [67], Sync Folders freemium [69] mean a paid app must justify itself via UX, reliability, and network/peer features — not merely "it syncs."

---

## 5. The 10 most important lessons for Foldover

1. **Own the network-share layer; don't inherit macOS SMB.** The #1 unresolved pain isn't sync logic, it's flaky mounts, hangs, `-36`/`-50` errors, dropped connections, and kernel panics on macOS's SMB client [79][80][26]. A robust in-app SMB implementation (or extremely defensive mount management with auto-remount, retry, and clear surfacing of transport errors) would be the standout feature vs. every incumbent that just rides `mount_smbfs`. Same for NFS/SFTP; treat "the share vanished mid-job" as a first-class, recoverable condition.
2. **Sell the preview.** Users repeatedly call the compare/preview tree the reason they trust a tool — FFS's pre-run inspection is described as the killer feature [59]; DuoBolt built a whole product on "see every difference first" [73]; ChronoSync's Trial Sync and ForkLift's add/update/delete counts are praised [24][71]. Foldover's Analyze step should be beautiful, fast, human-readable ("will copy 412 files, delete 3, 2 conflicts"), and reviewable per-item.
3. **Never destroy silently.** Default to trashing/archiving replaced-and-deleted files (GoodSync `_saved_`, ChronoSync archive, CCC SafetyNet are the expected baseline [8][24][89]); warn loudly before mass deletions or mirror ops; make two-way sync's deletion semantics explicit in plain language. SyncMate's 25 TB-duplication and mangled-folder stories [46][16] are what happens when the state engine and verification are weak.
4. **Win on many-small-files speed.** Fast scanning (persistent state file / file-ID or hash cache), parallel transfers, block-level/delta copy, resumable interrupted runs. ChronoSync gates SmartScan + Parallel Operations behind the paid version [40] and FFS gates parallel copy behind donation [92] — speed is a legitimate premium feature.
5. **Native SwiftUI/AppKit, Apple-Silicon-first, and menu-bar resident.** The incumbents are cross-platform and feel it (GoodSync's "Windows flavor" [7], FFS's toolkit UI [84], SyncMate's dated UI [86]). A menu-bar app with glanceable status, launch-at-login, Dock-optional mode, and proper macOS metadata handling (tags, xattrs, resource forks, packages, NFC/NFD normalization, `.DS_Store`/`._*` suppression on foreign filesystems) is visibly "for Mac people."
6. **Perpetual license in the $30–50 band, or freemium — not subscription.** The market's anchor points are SyncTime ~$10, ChronoSync $49.99-lifetime, CCC $49.99-with-paid-upgrades. Sync utilities get punished for subscriptions (GoodSync case study). If recurring revenue is needed, charge for major upgrades or for cloud/peer-relay services — never for the core sync engine. Honor "lifetime" claims or don't make them.
7. **Build Mac-to-Mac LAN sync in — don't charge a per-peer agent fee.** "Like iCloud without the internet" is a recurring explicit wish [64]; ChronoAgent ($19.99/Mac) and Syncthing's device-pairing friction are the taxes today. Bonjour/mDNS discovery + encrypted direct transfer on LAN is achievable with a Rust core and would be a headline feature. Do it better than Resilio (whose "remove all synced files" foot-gun and forgotten-shares bugs are complained about [51][95]).
8. **Triggers: on-mount, on-change, on-peer-visible, scheduled.** The most-loved behaviors are "syncs the moment the drive appears" [66], FSEvents-based background watching [67][16], and volume-mount schedules [30]. Combine a debounced watcher for local folders with availability triggers for network/removable targets.
9. **Speak sync plainly; pick safe defaults.** Users confuse backup/mirror/two-way constantly [16][53]. Offer three legible modes — Mirror A→B, Backup A→B (no deletes), Sync both ways — with conflict policy defaults that keep both versions. Conflict inbox beats scattered `conflict-` files [53]. First-run wizard per pair, like ChronoSync's assistants [96], but modern.
10. **Cross-platform core, platform-native shell.** A Rust core is right (FFI/UniFFI into Swift), and Linux/Windows demand exists — but the FFS/GoodSync lesson is that a shared UI layer reads as non-native and gets criticized. Keep the UI layer per-platform. Also: sandboxing is a real tax on sync apps (Express's limitations list is the warning [40]) — plan an unsandboxed direct build with a helper for full-disk/admin access, and treat MAS as a deliberately-scoped variant, not the flagship.

*Bonus lesson:* reliability *is* the brand in this category — every horror story above (SyncMate, GoodSync) pairs a sync failure with an unanswered support ticket, and every beloved app (ChronoSync, SyncTime) pairs reliability with responsive support. Ship good logs and a self-serve "what happened / undo" view.

---

## Sources

1. GoodSync for Mac download page — https://www.goodsync.com/download?os=macos (ver 12.11.9, macOS 10.13+, Intel+Apple Silicon)
2. MacUpdate — GoodSync for Mac (3.5/10 ratings; $29.95 subscription; user reviews 2013–2021) — https://goodsync.macupdate.com/
3. Mac Observer — "GoodSync for Mac: File Synchronization Made Easy" — https://www.macobserver.com/mac/goodsync-file-synchronization/
4. GoodSync for Mac desktop page (free tier limits) — https://www.goodsync.com/for-mac-desktop
5. WebsiteBuilder.org GoodSync review (Personal $29.95/yr, 5 devices) — https://websitebuilder.org/reviews/goodsync/
6. GoodSync business pricing (Workstation $49.95/yr) — https://www.goodsync.com/business-compare
7. MacTech — "GoodSync a good back-up/sync solution for Macs" (2012-11-09) — https://www.mactech.com/2012/11/09/goodsync-a-good-back-up-sync-solution-for-macs/
8. GoodSync Help — Job Options (Recycle/History defaults; Fast 1-way warning) — https://help.goodsync.com/hc/en-us/articles/115003939392-Job-Options
9. GoodSync Manual (deletion propagation; Analyze) — https://www.goodsync.com/manual
10. DPReview forum — GoodSync sync-speed observations (2025) — https://www.dpreview.com/forums/threads/observations-on-high-end-high-capacity-ssd-sync-speeds-and-my-sync-backup-strategy.4838178/
11. Capterra GoodSync reviews (4.4; pricing/support complaints) — https://www.capterra.com/p/10010689/GoodSync/reviews/
12. GoodSync Help — What is the _gsdata_ folder — https://help.goodsync.com/hc/en-us/articles/44434603540237-What-is-the-gsdata-Folder ; FAQ — https://www.goodsync.com/faq
13. G2 discussion — "GoodSync pricing is out of date?" (perpetual discontinued; v9 unusable) — https://www.g2.com/discussions/30944-pricing-is-out-of-date
14. Trustpilot — goodsync.com reviews (15; Allway Sync lifetime, refund, Mac complaints) — https://www.trustpilot.com/review/goodsync.com
15. Clien.net — GoodSync renewal-pricing complaint (2025-04-23, Korean) — https://www.clien.net/service/board/park/18960818
16. MacRumors — "Best Mac File Sync Program?" (2020-05-30; GoodSync subscription churn, SyncMate disaster, FFS/ChronoSync recs) — https://forums.macrumors.com/threads/best-mac-file-sync-program.2239077/
17. SoftwareAdvice — GoodSync reviews — https://www.softwareadvice.com/file-sync/goodsync-profile/reviews/
18. Macworld — "Review: GoodSync's features are rich but require study" (2012-11-20) — https://www.macworld.com/article/219755/review-goodsyncs-features-are-rich-but-require-study.html
19. Synology Community — "Problem with file sync" GoodSync 600 GB delete/recopy (2016-10-03) — https://community.synology.com/enu/forum/17/post/99299
20. Econ Technologies — ChronoSync 12 release announcement (2025-11-07) — https://www.econtechnologies.com/chronosync-12-0-and-chronoagent-12-0-released/
21. Econ Technologies — ChronoSync release notes (12.0.3 2026-09-14; 12.0.2 SFTP NFC fix) — https://www.econtechnologies.com/support/chronosync/notes.html
22. Econ Technologies — ChronoSync product/pricing ($49.99 lifetime) — https://www.econtechnologies.com/product/chronosync/
23. TidBITS Watchlist — ChronoSync 12 / ChronoAgent 12 pricing — https://tidbits.com/watchlist/chronosync-12/
24. Apple App Store — ChronoSync Express listing ($29.99; feature list) — https://apps.apple.com/us/app/chronosync-express/id904801687?mt=12
25. Setapp — ChronoSync Express user reviews — https://setapp.com/apps/chronosync-express/customer-reviews
26. ChronoSync Express version history, App Store (1.3.8 Apple Silicon 2022-09-22; 1.4.0 SMB kernel-panic workarounds 2024-04-29; 1.3.7 older-file-replacing-younger fix) — https://apps.apple.com/us/app/chronosync-express/id904801687?mt=12
27. MacUpdate — ChronoSync Express reviews — https://chronosync-express.macupdate.com/
28. Macworld — "ChronoSync Review: A Mac backup option with a justified fan base" (2024-12-12) — https://www.macworld.com/article/2554686/chronosync-review-a-mac-backup-option-with-a-justified-fan-base.html
29. MacAppCritic — ChronoSync review (scheduling depth; two-pass sync in v12) — https://macappcritic.com/chronosync-review/
30. App Store ChronoSync Express reviews (Tyl3rWyl3r 2025-07-10 volume-mount praise; witchbutter 2017; Photog345 2023-07-29) — https://apps.apple.com/us/app/chronosync-express/id904801687?mt=12
31. Econ Technologies — "Synchronize Two Folders" assistant guide — https://www.econtechnologies.com/chronosync/guide-assist-sync-two-folders.html
32. MacUpdate — ChronoSync page (ZFS error -50 comment; CCC speed comparison; ChronoAgent praise) — https://chronosync.macupdate.com/
33. TidBITS Watchlist — ChronoSync 11.0.3 (VoiceOver praise; 4.9.6 crash report) — https://tidbits.com/watchlist/chronosync-11-0-3/ and https://tidbits.com/watchlist/chronosync-4-9-6/
34. MacRumors — "2x Macs - how best to sync" (2024-02; iCloud/Syncthing/Resilio/ChronoSync) — https://forums.macrumors.com/threads/2x-macs-how-best-to-sync.2418294/
35. MPU Talk — "Looking for Mac Backup Software - Not ChronoSync" (2025-10) — https://talk.macpowerusers.com/t/looking-for-mac-backup-software-not-chronosync/39873
36. Musings from Mars — ChronoSync 3.3.1 review (2006) — http://www.musingsfrommars.org/2006/09/chronosync-powerful-flexible-synchbackup-utility.html
37. fainimade.blog — "Hot Take: Chronosync kinda sucks" (2022-08; progress-info complaint; 'Mirror' alternative) — https://www.fainimade.blog/2022/08/hot-take-chronosync-kinda-sucks.html
38. Econ Technologies — "How To Sync Files Between 2 Macs" (recommends ChronoAgent) — https://www.econtechnologies.com/how-to-sync-files-between-2-macs/
39. MPU Talk — "Looking for best way to sync files back and forth" (2024-08; CCC one-way only) — https://talk.macpowerusers.com/t/looking-for-best-way-to-sync-files-back-and-forth/38168
40. Econ Technologies — ChronoSync Express FAQ (full limitation list) — https://www.econtechnologies.com/support/chronosync-express/faqs.html ; Privileged File System Access guide — https://www.econtechnologies.com/chronosync-express/guide-privileged-access.html
41. MPU Talk — "Chronosync replacement (potentially) needed" (2022-11; M1 mini crashes, SMB kext suspicion) — https://talk.macpowerusers.com/t/chronosync-replacement-potentially-needed/31349
42. sync-mac.com (Electronic Team) — SyncMate pages incl. self-awarded ratings — https://www.sync-mac.com/options/sync-mac-folders.html ; https://www.sync-mac.com/backup-mac.html
43. eltima.com — "Best Mac sync software" (SyncMate 8.11.604, 2026-08-27) — https://mac.eltima.com/best-mac-synchronization-software.html
44. MacSources — "SyncMate 8 Mac App Review" (pricing tiers; not on MAS) — https://macsources.com/syncmate-8-mac-app-review/ ; https://macsources.medium.com/syncmate-8-mac-app-review-macsources-35064adc96b9
45. Apple App Store — SyncMate Plus (legacy crippled MAS version) — https://apps.apple.com/us/app/syncmate-plus/id423327943?mt=12
46. MacUpdate — SyncMate (3.4/178 ratings; 25 TB duplication report; sleep-failure/refund report) — https://syncmate.macupdate.com/
47. MacStories — "SyncMate for Mac" (2011) — https://www.macstories.net/reviews/syncmate-for-mac/
48. Electronic Team hosted reviews — SyncMate (incl. "10.11 yes / 10.14 no, SBB fills disk") — https://reviews.electronic.us/syncmate/
49. Trustpilot — sync-mac.com reviews (13; Eltima refund denial; wrong-company pollution) — https://www.trustpilot.com/review/www.sync-mac.com
50. Mac Informer — SyncMate user comments (M1 Mini Android failure; sideloaded module) — https://macdownload.informer.com/syncmate/comments/
51. MPU Talk — "Sync folders between two M1 Macs" (2022-04; Resilio vs ChronoSync; 'Remove all synced files' caution) — https://talk.macpowerusers.com/t/sync-folders-between-two-m1-macs/28621 ; MPU "rolled their own iCloud on a Synology" (Resilio free personal tier) — https://talk.macpowerusers.com/t/has-anyone-here-rolled-their-own-icloud-on-a-synology/39422
52. MPU Talk — "SyncThing - pro or con?" (2023-12) — https://talk.macpowerusers.com/t/syncthing-pro-or-con/35817
53. MPU Talk — "File syncing – my journey and result" (2025-01; Synology Drive happy path) — https://talk.macpowerusers.com/t/file-syncing-my-journey-and-result/39591
54. synology-forum.de — "Welche Sync Software (Einseitig) mit Mac und Synology" (Drive too slow) — https://www.synology-forum.de/threads/welche-sync-software-einseitig-mit-mac-und-synology-nutzen.133514/
55. TidBITS Talk — "Synchronizing folders on main and backup drives" (2025-12) — https://talk.tidbits.com/t/synchronizing-folders-on-main-and-backup-drives/32608
56. TidBITS Talk — "Mac to Mac sync" — https://talk.tidbits.com/t/mac-to-mac-sync/19506
57. TidBITS Talk — "Keeping two Macs in sync?" (apps/state hard) — https://talk.tidbits.com/t/keeping-two-macs-in-sync/22363
58. MacRumors — "Sync Two Folders" (2019; SyncTime/GoodSync/Hazel/Mountain Duck/DeltaWalker) — https://forums.macrumors.com/threads/sync-two-folders.2172665/
59. Hacker News — FreeFileSync thread (2021-12; preview/conflict-resolution praise) — https://news.ycombinator.com/item?id=29661539 ; HN "SyncThing and FreeFileSync make computing so much nicer" (2021-07) — https://news.ycombinator.com/item?id=27932866
60. Hacker News — Syncthing 2.0 release thread (2025-08) — https://news.ycombinator.com/item?id=44873043
61. Syncthing forum — dmg vs binary install; menubar icon rationale (2024-07) — https://forum.syncthing.net/t/syncthing-on-mac-dmg-install-vs-copying-binary-and-plist/22403
62. Syncthing forum — "Syncthing for macOS the road to v2 (official application)" — https://forum.syncthing.net/t/syncthing-for-macos-the-road-to-v2-official-application/26060 ; releases — https://github.com/syncthing/syncthing-macos/releases/
63. Hacker News — Syncthing discussion (Möbius Sync on iOS; wrappers) — https://news.ycombinator.com/item?id=35879039
64. Apple Community — "I'd like to sync two Macs, as in iCloud, without going through the internet" — https://discussions.apple.com/thread/251101839
65. MacUser.de — "Backupapp mit Versionierung und Ordner-/Dateiüberwachung" (2024-12) — https://www.macuser.de/threads/backupapp-mit-versionierung-und-ordner-dateiuberwachung.949935/page-2
66. EroScripts — "Sync app for folders" (2025-04; AllwaySync subscription churn; MirrorFolder $45 on-mount) — https://discuss.eroscripts.com/t/sync-app-for-folders/247867
67. Desairem — SyncTime product page (background watch, on-connect, menu bar, Shortcuts/AppleScript) — https://desairem.com/wordpress/synctime/ ; App Store — https://apps.apple.com/us/app/synctime/id590386474?mt=12
68. AlternativeTo — SyncTime (~$10 perpetual; indie dev) — https://alternativeto.net/software/synctime/about/
69. Apple App Store — "Sync Folders" (3.9★, 17 ratings; preview mode) — https://apps.apple.com/us/app/sync-folders/id530573877
70. Apple Community — "App for syncing folders" (tool list) — https://discussions.apple.com/thread/251016988
71. BinaryNights — ForkLift 4 manual / site (Sync window; 20× faster analyze; preview counts) — https://binarynights.com/manual ; https://binarynights.com/
72. Atpeaz — ForkLift 4 review ("rsync with an easy to use UI… preview before I run it") — https://www.atpeaz.com/forklift-4-review-comparison-with-forklift-3/
73. DuoBolt — sync-folders product page (compare-by-content; conflicts wait) — https://duobolt.app/sync-folders/
74. GitHub — bcpierce00/unison (2.54.0; ~2.5 maintainers note) — https://github.com/bcpierce00/unison ; Homebrew unison-app cask (disabled 2026-09-01, fails Gatekeeper check) — https://github.com/Homebrew/homebrew-cask/blob/master/Casks/u/unison-app.rb
75. Homebrew Formulae — unison — https://formulae.brew.sh/formula/unison
76. AlternativeTo — ChronoSync alternatives (RsyncOSX etc.) — https://alternativeto.net/software/chronosync/?platform=mac
77. Der Flounder — "rsync replaced with openrsync on macOS Sequoia" (2025-04-06) — https://derflounder.wordpress.com/2025/04/06/rsync-replaced-with-openrsync-on-macos-sequoia/ ; AppleInsider explainer — https://appleinsider.com/inside/macos-sequoia/tips/what-you-should-know-about-apples-switch-from-rsync-to-openrsync ; mjtsai — https://mjtsai.com/blog/2025/06/06/sequoias-new-rsync/
78. Ask Different — rsync/openrsync breakage on Sequoia (hangs; 'too many open files'; brew rsync 3.4.1 workaround) — https://apple.stackexchange.com/questions/479291/shell-scripts-that-call-rsync-no-longer-work-in-macos-sequoia
79. Apple Discussions — "Sequoia keep losing connection to QNAP NAS (SMB) and finder constantly hanging" — https://discussions.apple.com/thread/255766711
80. TrueNAS forums — "SMB ISSUES on MacOS" (stalls, -36, Finder crashes; Monterey–Sonoma) — https://forums.truenas.com/t/smb-issues-on-macos/4778 ; MacRumors — "How is SMB?" (QuickLook/Finder hangs; AFP retreat) — https://forums.macrumors.com/threads/how-is-smb.2244611/page-3
81. MacRumors — "SMB Contents visibility issue with BigSur" — https://forums.macrumors.com/threads/smb-contents-visibility-issue-with-bigsur-content-does-not-update-without-reconnection-any-ideas.2307474/
82. ralph-kokott.de — "OSX SMB Transfers incredibly slow (Terminal Fix)" (nsmb.conf tuning; DSDontWriteNetworkStores) — https://ralph-kokott.de/2019/osx-smb-transfers-incredibly-slow-terminal-fix/
83. MacRumors — real-time two-folder sync request (Obsidian vault; iCloud revision loss) — https://forums.macrumors.com/threads/are-there-or-good-apps-on-macos-to-real-time-continuously-sync-two-folders-on-the-same-drive-preferably-command-line-ones.2390132/
84. FreeFileSync forum — "Horrible interface in version 10.25" — https://freefilesync.org/forum/viewtopic.php?t=7465
85. FreeFileSync forum — native Apple Silicon request/11.12 ARM64 beta; M1 crash reports over SMB — https://freefilesync.org/forum/viewtopic.php?t=7997 ; https://freefilesync.org/forum/viewtopic.php?t=8039 ; changelog (11.12 ARM64) — https://github.com/Forger125/FreeFileSync/blob/master/Changelog.txt
86. Apfeltalk — "Ausprobiert: SyncMate" (2018-10-24, German) — https://www.apfeltalk.de/community/threads/ausprobiert-syncmate.535344/
87. Syncthing forum — Synctrain native macOS app (selective sync, on-demand streaming) — https://forum.syncthing.net/t/syncthing-native-app-for-macos-synctrain-ios-based/22885
88. iDownloadBlog — "App Store subscription fatigue" (2020-01-16) — https://www.idownloadblog.com/2020/01/16/app-store-subscription-fatigue-avoiding-being-bled-to-death/
89. Michael Tsai — "Carbon Copy Cloner 7" (price $40→$50; $25 upgrade "a bargain vs subscription") — https://mjtsai.com/blog/2024/05/17/carbon-copy-cloner-7/
90. Macworld — Carbon Copy Cloner review ($49.99 Personal & Household; NAS-enabled volumes; no cloud targets) — https://www.macworld.com/article/2405539/carbon-copy-cloner-review.html ; best-backup roundup — https://www.macworld.com/article/668696/best-mac-backup-software.html
91. Noodlesoft — Hazel 5 FAQ ($42/$65; $20 upgrades) — https://www.noodlesoft.com/kb/hazel-5-faq/ ; Hazel manual — Syncing Folders (one-way only; not for two-way) — https://www.noodlesoft.com/manual/hazel/advanced-topics/syncing-folders/
92. Capterra — FreeFileSync reviews (donation edition gating; UI nitpicks) — https://www.capterra.com/p/171146/FreeFileSync/reviews/ ; FFS forum donation-edition install issues — https://freefilesync.org/forum/viewtopic.php?t=10804 ; https://freefilesync.org/forum/viewtopic.php?t=11069
93. MacGeneration — "Une version bridée de ChronoSync dans le Mac App Store" (2014-09) — https://www.macg.co/logiciels/2014/09/une-version-bridee-de-chronosync-dans-le-mac-app-store-84306
94. Consumer Rights Wiki — Paragon NTFS license invalidation; Rosetta Stone perpetual-license bricking — https://consumerrights.wiki/index.php?mobileaction=toggle_view_desktop&title=Paragon_NTFS_software_license_invalidation ; https://consumerrights.wiki/w/Rosetta_Stone_bricks_legacy_CD-based_and_downloadable_software
95. Hacker News — Resilio Sync anecdote (forgotten share; kb/s off-LAN) — https://news.ycombinator.com/item?id=39080351
96. Econ Technologies — ChronoSync guides — https://www.econtechnologies.com/support/chronosync/guides.html

**UNVERIFIED items:** Resilio Sync's current per-tier pricing (free personal tier confirmed via MPU Talk, paid-tier prices not captured); DuoBolt's price; SyncTime's exact current MAS price (~$10 per AlternativeTo); FreeFileSync RealTimeSync's availability inside the macOS bundle (documented for Windows/Linux, macOS bundling inferred from user reports); MirrorFolder and "Mirror" (fainimade) are noted as adjacent tools, not Mac-verified products.
