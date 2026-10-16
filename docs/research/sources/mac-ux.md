> Raw AI research report (devin, SWE-2 max), 2026-10-16. Unedited. Corrections: [fact-checks](../README.md#corrections).

# Foldover — Design Research Report

*A design-research report for a new native Mac file-sync app. Compiled 2026-10-16.*
*Scope: local + network sync (internal/external disks, NAS/SMB/NFS/AFP, SFTP/SSH, Mac-to-Mac LAN). Cloud providers out of MVP. Native macOS UI (Swift/SwiftUI/AppKit) over a portable core (likely Rust).*

Every factual claim is cited inline as [n] and resolved in **Sources** at the end. Anything I could not confirm is marked **UNVERIFIED**. Dates are given for time-sensitive facts.

---

## Executive summary

1. **The baseline moved.** "macOS 26 Tahoe" is no longer current: **macOS 27 "Golden Gate" shipped 2026-09-14** with a *revised* Liquid Glass (user-tunable opacity slider, edge-to-edge sidebars, colored sidebar icons, uniform corner radii) and is **Apple-silicon only**; Tahoe was the last Intel release [5][6][7][8]. Design Foldover for the Golden Gate refinement, not the original Tahoe look — users can now dial glass translucency down, so do not depend on heavy translucency for legibility [7][8].
2. **Adopt SwiftUI where it's strong, AppKit where it's proven.** SwiftUI scenes (`Settings`, `MenuBarExtra`, `Window`), `Form`, toolbars, and SF Symbols 7 are genuinely good now; SwiftUI `List`/`OutlineGroup`/`Table` still cannot handle 100k-row trees at 60fps — `NSOutlineView`/`NSTableView` remain the answer for the diff/preview tree [18][19][20][21][22][23]. Plan a hybrid shell from day one.
3. **Target macOS 26+.** Tahoe hit ~86% adoption by late July 2026 and macOS 27 is Apple-silicon only, so "26 and later" costs almost nothing in users and buys the entire modern design system and API surface [11][12][6].
4. **The product is the preview.** Every incumbent makes "Analyze/Trial Sync" the moment of truth — GoodSync's arrow columns, ChronoSync's Trial Sync sheet, Transmit's Simulate, ForkLift's faster analyzer. Foldover's diff preview should be the hero screen: instant, incremental, filterable, with per-item overrides and plain-language consequences [44][45][50][51][33][35][36].
5. **Finder badges are a trap right now.** Legacy Finder Sync extensions are broken on macOS 26 Tahoe (`FinderSyncExtensionHost` no longer exists) and users still report failures on macOS 27 — Dropbox and Nextcloud both shipped broken Finder integration. Ship context menus via the extension if it works in testing, but design the app so status does not *depend* on Finder badges [60][61][62][63].
6. **Permissions are the product's first impression.** Full Disk Access, folder access and the post-Sequoia Local Network permission all have flaky edges; the winners (CCC, Codex-style onboarding) guide with a floating card + draggable app icon + live polling, ask in context, and explain *why* in one sentence [28][29][71][72][73][74][65][66].
7. **Status must be glanceable and boring.** Time Machine's model — animates while working, `!` when something's wrong, one menu line saying "Latest backup: …" — is the bar. iStat Menus 7.5 and Ice show what a modern menu-bar presence looks like on Golden Gate [93][91][92].

---

## 1. Apple's current design language (late 2026)

### 1.1 Where macOS design actually is

- **macOS 26 Tahoe** introduced Liquid Glass at WWDC25 (announced 2025-06-09, released 2025-09-15): a translucent material for controls/navigation, floating glass sidebars and toolbars, larger window corner radii, concentricity between glass elements and their containers [1][3][4][11].
- **macOS 27 "Golden Gate"** was announced at WWDC26 (keynote 2026-06-08) and **released 2026-09-14** [5][6]. It is a correction release for the design: a **Liquid Glass slider** in System Settings > Appearance that adjusts translucency (default is mid-frost, not full clear), improved diffusion, darkened edges and brighter highlights for depth, **sidebars back to edge-to-edge with colored icons**, **uniform window corner radius** (the oversized Tahoe radii are gone), uniform toolbars, and clearer active-window shadows [5][7][8].
- **macOS 27 is Apple-silicon only**; Tahoe was the last release for Intel Macs. Golden Gate supports MacBook Air/Pro M1+, iMac 2021+, Mac mini 2020+, Mac Studio, Mac Pro 2023 [6][Apple compatibility list in 5/6].
- HIG updates in the 27 cycle: **new guidance for sidebar icon colors** and the "adaptable" sidebar style, **menus**: reduced menu-item images (SwiftUI now hides symbol images in most menus by default; use `labelStyle(.titleAndIcon)` to force one), updated Liquid Glass guidance for color, buttons, toolbars, tab bars; new scroll-edge-effect guidance [9][10].
- SF Symbols 7 (26-era, carried forward): Draw On/Off animations, **Variable Draw** for strength/progress, Magic Replace improvements, gradients — explicitly intended as "informative, dynamic progress indicators" [86].

**Implication for Foldover:** Build to Golden Gate. Use standard components so glass, edge effects and toolbar grouping come free; test with the Liquid Glass slider at both ends (and Reduce Transparency) because *users can now opt out of your translucency* [2][7]. Use SF Symbols 7 variable-render symbols for live progress in the menu bar and in-window.

### 1.2 Window idioms to use

- **NavigationSplitView sidebar** for pairs/devices/history — edge-to-edge, colored SF Symbols now expected again [4][9].
- **Glass toolbar** with automatic logical grouping (`NSToolbarItemGroup` to override); sidebar toggle leading, inspector toggle trailing-most, share next, search last; keep the sidebar's own toolbar area to ≤2 items [4][13].
- **Inspector** on the trailing edge for "details of the selected thing" (pair settings, selected file diff) — inspectors use edge-to-edge glass alongside content via `NSSplitViewController` `.inspector` behavior / SwiftUI `.inspector()` [4].
- **Settings** via the SwiftUI `Settings` scene + `TabView` (⌘, wired automatically); `SettingsLink`/`openSettings` to deep-link a pane [17].
- **MenuBarExtra** scene (macOS 13+) with `.menuBarExtraStyle(.window)` for a popover panel — the modern replacement for hand-rolled `NSStatusItem` + popover [15][16].
- Menu bar icon conventions: working area is **22pt high**, ~**16pt** for a circular glyph, **template (monochrome) images preferred**, ~35% opacity reads as "disabled" [14].

### 1.3 State of SwiftUI on macOS in 2026

Works well now: app scenes (`Window`/`WindowGroup`/`Settings`/`MenuBarExtra`), toolbars, `Form`/`LabeledContent`, `Table` for moderate data, `.searchable`, scroll edge effects, symbol effects, App Intents/Focus/widget surfaces [15][16][17][25].

Still painful / where AppKit wins:

- **Large flat lists & trees.** SwiftUI `List` on macOS is effectively non-lazy; a 100k-item hierarchical `List` left a Mac Studio M2 unresponsive for *minutes* while `NSOutlineView`/`UICollectionView` versions are "blazing fast" on identical data [19][20]. `OutlineGroup` eagerly evaluates `children` even for collapsed nodes [21]. `Table` *is* lazy and is the escape hatch for flat rows, but still shows hitches at scale and lacks AppKit's column controls [22][18]. Apple's own WWDC26 lab advice boils down to: keep row count constant, pre-filter in the model, avoid per-row `if`s — i.e., work *around* the framework [23].
- **Right-click select + context menu:** `.contextMenu` doesn't select the clicked row or expose the target — the classic Mac behavior requires AppKit [18][25].
- **Drag and drop introspection** historically lagged AppKit; `.onDragSessionUpdated` (26/27-era) narrows the gap but AppKit still gives the full session [25].
- **Quick Look:** `QLPreviewPanel` is AppKit/responder-chain only; from SwiftUI you bridge it (custom `NSViewController`/`NSResponder` in the chain) [27].
- AppKit `NSTableView`/`NSOutlineView` recycle views so memory stays flat regardless of row count; that's the practical requirement for a sync diff tree [26][24].

**Recommended split:** SwiftUI for everything except the **diff/preview outline** (use `NSOutlineView` via `NSViewRepresentable`, or an AppKit-backed table with SwiftUI cells as several shipping apps do [18]) and **Quick Look bridging**. This matches how serious Mac apps are actually built in 2026: SwiftUI shell, AppKit for the fast data grid [18][24][25][26].

### 1.4 Minimum macOS version

- TelemetryDeck (dev-indie-skewed): Tahoe **86.0%** of installs week of 2026-07-27; Sequoia 12.2%; Golden Gate beta already ~1% [11].
- Homebrew analytics (30-day events, mid-Oct 2026): Tahoe 33.8%, **Golden Gate 18.3%**, Sequoia 15.4% [12].
- **Recommendation: deployment target macOS 26.** Rationale: (a) Liquid Glass/HIG baseline is assumed anyway; (b) the interesting APIs (`MenuBarExtra` is 13+, but the refined sidebar/menu behavior, SF Symbols 7 effects, `.onDragSessionUpdated`) are 26/27-era [9][10][25]; (c) supporting 15 means designing two visual languages; (d) macOS 27 being Apple-silicon-only means 26+ implies ARM64 — which also simplifies the Rust core target matrix. Cost: the ~12% still on Sequoia shrinks weekly [11][12]. If you must hedge, 15 is the only realistic backstop — earlier OSes predate the design system entirely.

---

## 2. Exemplary Mac utilities — what to borrow

| App | Steal this | Don't steal this |
|---|---|---|
| **Carbon Copy Cloner 7** | Task "plan" described in plain language; **Backup Volume Setup Assistant** (dedicate destination, pick filesystem/snapshot/encryption in 2 clicks); Task Notes; Product Tours triggered from help; "run when source/destination is reconnected" trigger; SafetyNet concept (protect destination items not in source) [28][29] | Density of option panels; privileged-helper friction is inherent, not a model |
| **Hazel 6** | The rules editor: Folders → Rules → Rule Editor three-column layout, *detachable* rule editor window, **Preview Rule** (does this rule match this file?), Rule Status log, error highlighting on save, revert-via-Finder-context-menu [31][32] | Layout preference panes (3 layout modes = over-configurable) |
| **Transmit 5** | **Simulate** = dry run producing the exact list of what would transfer/delete *before* doing it; selectable per-file inclusion after simulate; per-file progress; sync between *any* two locations local/remote [33][34] | Sync hidden inside a dual-pane FTP client model — Foldover should make sync the object, not a command |
| **ForkLift 4** | Real-time counter while analyzing ("scanning 4,211 folders…"), 20× faster analyze vs v3, **per-item deselect in the sync window**, activities panel pinned in the preview pane [35][36][37] | Reverse filter logic they admit has "a steeper learning curve" [35] |
| **Kaleidoscope** | Folder compare that filters by modification/type/name; inline diffs + **Quick Look inside comparison**; color semantics: green=added, red=deleted, blue=changed (changed-words highlighted in deeper blue); collapse unchanged regions; recent-comparisons history [38][40] | Git-centric changeset model (your unit is a sync pair, not a commit) |
| **Arq 7** | Backup *plans* as first-class objects; wizard: pick storage → optional encryption password → scope (all drives / user data / pick folders) → advanced tabs you can ignore; logs integrated into main window and reachable from plan; global menu shows per-plan status; "super-clean interface… boring in backup is a good thing" [41][42][43] | Arq 5/6's Electron era — they rewrote native and users noticed [43] |
| **CleanShot X** | The Quick Access Overlay: the instant an action completes, a small floating panel offers the next likely actions — Foldover equivalent: post-sync completion card w/ "Reveal 12 new files / Review 2 conflicts / Undo" [90] | Nothing structural — its polish *is* the lesson |
| **Raycast** | Keyboard-first everything; every command one shortcut away; consistent verb patterns across commands [88] | Architecture: Raycast v1 was native AppKit, but they rebuilt on a **web stack for cross-platform (Windows)**. Foldover's Rust-core + native-shell is the opposite, viable bet *because* the UI is per-platform anyway — but note the cost they cite: "the majority of Raycast's codebase is UI" [88] |
| **Things 3** | Two Apple Design Awards; philosophy: content first, minimal chrome, animations prototyped in After Effects and kept only "if you don't get quickly annoyed by them" [89] | Nothing directly applicable beyond the bar it sets |
| **iStat Menus 7.5** | Liquid Glass menus on 27; per-item configurable menu-bar presence (icon/graph/text/combined modes); 1/5th the CPU of v6 — menu-bar extras must be cheap to run [91] | Nine separate items — Foldover should ship **one** item with a good popover |
| **Ice / Thaw / macOS 27 built-ins** | Expectation-setting: Golden Gate now has native ⌘-drag reorder + a double-arrow overflow for notched MacBooks; Ice (~30k★, macOS 14+) is inactive, Thaw/icemelt forks target 26/27. Your extra must coexist with hiding/overflow gracefully [92] | — |
| **Time Machine** | The status contract: icon animates during work, `!` on failure, menu line 1 = "Latest backup: <date>", items: Back Up Now / Skip This Backup / settings link. Silent success, loud failure [93] | Its opacity about *what* it did — Foldover's history should be readable |
| **Syncthing (cautionary)** | Powerful engine, no Mac story: the official UX is a **web GUI at localhost:8384**; Mac wrappers are thin menu-bar launchers; users report "close the browser, forget the port, think it's broken"; corporate users abandoned it for Dropbox despite it working [58]. The lesson isn't features — it's that a sync app that isn't a real Mac app loses by default. | All of its UI conventions. Conflicts as littered `*.sync-conflict-*` files that then propagate to other devices is the specific anti-pattern Foldover's conflict center should beat [57] |

**Incumbent sentiment** (why there's an opening): GoodSync reviews praise capability but repeatedly call the GUI overwhelming/confusing for non-experts [52]; ChronoSync's Organizer + per-document windows + dated tutorial videos read as legacy (v11 added Task Groups + search because task lists were "unmanageable") [46][48]; FreeFileSync is "interface stuck in 2010… cluttered layout, technical jargon" per reviewers [54][55]; SyncMate still ships (v8.11.604, 2026-08-27) but is a contacts/devices sync legacy product [56]. None of them looks or feels like a 2026 Mac app. That is the entire opportunity.

---

## 3. Sync-specific UX patterns

### 3.1 Creating a sync pair

- **Drag two folders onto the app.** Two clearly-labeled wells ("Keep this folder" ⟷ "in sync with this folder") that accept Finder drags, a `+`/`Choose…` browse fallback, and paste-a-path for power users. GoodSync/FreeFileSync already train users to think Left/Right; name them by *role* (this Mac / that place), not left/right [50][53].
- **Location picker is typed, not just a folder dialog.** Every "endpoint" is one of: Local folder, Mounted volume (auto-listed from `NSWorkspace`), Network share (SMB/NFS/AFP URL or browsed), SFTP host (reuse `~/.ssh/config` entries — Citadel-style pure-Swift SFTP exists if needed; in the Rust core, SSH crates like `russh`/`ssh2` — verify choice, **UNVERIFIED**), or **another Mac running Foldover** discovered via Bonjour `NWBrowser` (`_foldover._tcp`, requires `NSBonjourServices` + Local Network permission) [75][76][77][78][65].
- **Presets, not modes.** Incumbents expose "one-way/two-way/mirror" as radio buttons with manual-ese tooltips. Offer three cards with consequences written out:
  - *Mirror to backup drive* — "The backup drive will be made to look exactly like the source. Files deleted from the source are moved to Foldover's archive on the destination (recoverable)." (CCC SafetyNet semantics) [28][29]
  - *Keep two Macs in sync* — "Newest change wins both ways. Same file edited on both sides → flagged as a conflict, never silently picked."
  - *Sync with NAS* — "One-way or two-way, plus handling for NAS quirks (case sensitivity, `._` AppleDouble files, no filename versioning)."
- **Auto-detect.** On launch and on `NSWorkspace.didMountNotification`, list newly attached volumes; browse `_smb._tcp`/`_nfs._tcp`/`_afpovertcp._tcp`/`_sftp-ssh._tcp` Bonjour services for NAS/Mac targets so "MyNAS.local" appears without typing an address [75][76][77]. CCC's "run when source or destination is reconnected" is the trigger model to copy for mount-based automation [29].
- **Mac-to-Mac:** ChronoAgent is the incumbent answer — a small always-on agent on the other Mac that gives direct, encrypted, faster-than-SMB access and parallel scanning [49]. Foldover's equivalent: any Mac running Foldover is both client and peer (no separate $$$ agent product); pairing via Bonjour + one-time code confirmation.

### 3.2 Explain before the first sync

The single most consistent differentiator between incumbents and good modern apps: **a plain-language sentence of what will happen before anything is touched.** CCC's "Task Plan" summarizes intent in the main panel [29]; ChronoSync's Trial Sync exists because "no other synchronizing application lets you manage your data so thoroughly" [44]. Foldover's version, always visible in the pair header:

> "Every day at 18:00, or when 'Backup SSD' connects: copy new & changed files **from** `~/Projects` **to** `Backup SSD/Projects` (≈4.2 GB pending). Deleted files are kept in Foldover's archive for 30 days. Last synced today 09:41 — everything up to date."

Live-composed from actual settings; clicking any underlined phrase jumps to that setting. This is the Task Plan idea, modernized.

### 3.3 Preview / diff — the hero screen

- **Two-phase like the best incumbents, but streamed.** ChronoSync documents the tradeoff honestly: one-pass scanning is faster but can't show meaningful progress; two-pass gives an accurate plan + ETA at the cost of a pre-pass [45][47]. Foldover: **stream results incrementally** — start populating the diff tree as the scan proceeds (counts ticking up, ForkLift-style live counter [35]), with "still scanning…" shown per-branch; allow *Sync Now* to enable once the plan is complete, or allow "sync what's been analyzed so far" for huge trees. This is better than both: GoodSync's analyze-then-review flow [50][51] plus ForkLift's immediate feedback.
- **Presentation:** single unified outline (not two panes — Transmit/ForkLift dual-pane is a file-manager metaphor; a diff wants one tree). Row = file; leading glyph = action: `→` copy left-to-right, `←` right-to-left, `⇄` update-both-ways, `⌫→archive` deletion, `⚠` conflict, `=` unchanged (hidden by default). GoodSync's arrow/circle/crossed-arrow column language is proven [50]; Kaleidoscope's color discipline (green added, red deleted, blue changed) is the palette [40].
- **Filters as chips, not a dialog:** [New 1,204] [Changed 312] [Deletions 48] [Conflicts 3] [Skipped ~ ] + search field + "show unchanged" toggle (off by default). FreeFileSync's category checkboxes and Kaleidoscope's changeset filters are the pattern [53][38].
- **Per-item overrides:** click the action glyph (or right-click) → popover: *Copy → / ← Copy / Skip once / Always skip this file / Treat as conflict*. ChronoSync lets you uncheck items or flip delete-vs-sync per row in its Trial sheet [44]; GoodSync lets you click any arrow to change direction [50]; ForkLift added per-item deselect because users demanded it [36]. This is table stakes.
- **Quick Look** on Space for any row (bridge `QLPreviewPanel`; also enables the "compare" affordance for images/PDFs) [27]; double-click a conflict → conflict resolver (3.4); "Compare contents" for text files → built-in mini-diff or hand off to user's diff tool (Kaleidoscope/BBEdit integration is a cheap delight — Kaleidoscope already catalogs external-editor support) [39].
- **Estimate footer:** "12,407 items · 38.4 GB to copy · ~6 min over this connection · 214 GB free on destination." GoodSync computes required disk space during Analyze; ChronoSync estimates space in Trial Sync [51][44].

### 3.4 Conflict resolution

- **Never litter.** Syncthing renames the loser to `name.sync-conflict-<date>-<time>-<device>.<ext>` and *propagates the conflict files to every device* — documented behavior, widely disliked [57]. Dropbox's "(Alice's conflicted copy 2026-10-16)" is more readable but still passive: resolve it yourself [59].
- **Foldover Conflict Center:** a dedicated view (and a menu-bar badge state) that shows each conflict as **two file cards side-by-side**: name, location, size, mtime, which device/side, Quick Look thumbnail, one-tap diff for text/images. Actions: **Keep this one**, **Keep both** (auto-rename `name (from MacBook, Oct 16).ext`), **Skip for now**, plus "apply choice to remaining 6 conflicts" and a remembered default per pair [59][40].
- Modified-vs-deleted gets explicit language: "Deleted on NAS but edited on this Mac 2 hours later — restore the edited copy, or accept the deletion?" (Syncthing resolves this silently; make it a choice) [57].
- Conflicts are surfaced *in the preview* (⚠ rows, filterable) and *after* sync in the completion card — not only as files on disk.

### 3.5 Progress that tells the truth

- Two numbers users actually want: **throughput** and **ETA**. ChronoSync shows elapsed + bar but no ETA — called out in reviews as a miss [48]. Show: items done/total, bytes done/total, current throughput (MB/s, file/s), ETA from smoothed rate, current file name.
- Be honest about indeterminacy: during the scan phase show the live counter + indeterminate state (ChronoSync's one-pass lesson: don't fake determinate progress when you don't know the total [45]); once the plan exists, the bar becomes real. SF Symbols Variable Draw can make the icon itself a progress indicator [86].
- Menu bar icon states mirror Time Machine's contract: idle glyph, animated glyph while syncing, `!` badge on error/attention, pause indicator when paused; popover shows per-pair mini-status + throughput sparkline [93][15][14].

### 3.6 History & undoing deletes

- Per-pair **History** view: each run = start/end, counts (copied/updated/deleted/skipped/conflicts/errors), throughput, and the itemized list with "Reveal in Finder" per row. CCC's Task History + Task Notes (annotate *why* a task/run exists, e.g. "backup made before upgrading") are the model [28].
- **Deletion safety = a feature, not a warning.** Default mirror behavior: destination deletes go to a per-destination **Archive** (`<dest>/.foldover-archive/…` with date structure) with retention rules (keep 30 days / keep 10% of disk / unlimited), browsable and restorable — CCC's SafetyNet and "Protect root-level items on the destination" generalized to sync [28][29]. Restore = select → "Put back"; deleted-file history should be browsable in-app like Kaleidoscope's File History [38].
- Undo the *last sync run* where feasible (copies are reversible; deletions are, via archive; overwrites are not unless versioning is on — say so honestly in the UI).

### 3.7 Errors, notifications, quietness

- **Actionable errors or nothing.** Every error row: what happened, why it probably happened, one-click fix ("Reconnect 'Backup SSD' → Retry", "Foldover needs permission to read ~/Documents — Grant access", "NAS refused login — Update password"). ChronoSync's "Readiness Warnings" concept (pre-flight checks) is right; its presentation is dated [46].
- **Notification policy:** default = sync failures + conflicts needing you + first-run completion of a brand-new pair. Optional: per-pair "notify on completion" (off by default), daily/weekly digest, never per-file. Time Machine never notifies on success; be that quiet [93]. Use `UNUserNotificationCenter` with categories/actions (Retry / Review conflicts / Open pair); note `.provisional` is ignored on macOS and the permission prompt shows **once** — so ask only when the user first enables something that needs it [83].
- **Menu bar popover** = the real status surface (see §7, Screen E). iStat Menus' configurability (choose icon/graph/text) is worth offering minimally: icon only / icon + last-sync time [91][14].

### 3.8 Accessibility & localization

- VoiceOver: Mac VoiceOver is keyboard-driven and navigates **containers first** — get the container structure right (sidebar / toolbar / diff outline / inspector), label every glyph-state ("will copy to destination", "conflict, 2 versions"), make per-row actions reachable via rotor/actions not just mouse-click glyphs [84].
- Full keyboard: ⌘N new pair, ⌘R/⌘⇧R run/preview, ⌘E edit pair, arrows navigate diff, Space = Quick Look, ⌘⌫ reject item, ⌘K resolve conflict — every command in the menu bar and in Shortcuts. Raycast/Things are the reference bar for "never needs the mouse" [88][89].
- Dynamic Type isn't a Mac thing, but **Increase Contrast / Reduce Transparency / Reduce Motion** are: honor them (they also affect how Liquid Glass renders for those users) [2][7].
- Localization from day one via **String Catalogs**: auto-extraction from SwiftUI, plural variants ("%lld items"), device variants, `.xcloc` export for translators; Xcode 27 can translate catalogs with coding agents — cheap to keep 10+ locales [85]. Beware sync-domain plural/inflection pitfalls ("1 conflict" vs "2 conflicts"), and give translators context comments on terse strings like "Mirror".

---

## 4. System integration

| Integration | Recommendation | Notes/sources |
|---|---|---|
| **Menu bar extra** | `MenuBarExtra` scene, `.window` style popover; template icon with state variants (idle/animating/paused/error); show-in-menu-bar toggle in Settings honoring `isInserted` binding [15][16][14] | Time Machine contract; coexists with Golden Gate's built-in ⌘-drag reorder + overflow [92] |
| **Finder badges & context menu** | **Risk flag.** Legacy `FIFinderSync` is broken on macOS 26 (`FinderSyncExtensionHost` missing; Dropbox/Nextcloud both affected; still broken for some on 27). File Provider is for *virtual* file sets — not a fit for real-folder sync. Plan: implement the extension (badges + "Sync now / Exclude / Reveal in Foldover" context menu) because when it works it's the expected UX — but verify on 26.6/27.x before ship and degrade silently [60][61][62][63][64] | Do not gate any status UI on it working |
| **Shortcuts / App Intents** | Entities: `SyncPair`, `SyncRun`. Intents: Run Sync (pair?), Pause/Resume Pair, Pause All, Preview Changes, Resolve Conflicts. Expose as App Shortcuts so they appear in Shortcuts **and Spotlight** (26-era feature) [80] | Foundation for Focus + widgets + Siri-era automation |
| **Focus filters** | AppFocusFilter: "During Work focus, only sync pairs tagged Work / suppress completion notifications." Small win, real HIG fit [79] | Optional for MVP |
| **Widgets & controls** | Small/medium desktop widget: pair status + last-sync + Sync Now button; Tahoe adds controls to **Control Center** — a "Sync all now"/"Pause" control is a perfect fit [81] | WidgetKit timeline fed by app group |
| **Spotlight** | Donate pairs & recent runs to the private index via Core Spotlight (`CSSearchableIndex`) so "foldover photos sync" finds the pair; keep volume small — Apple guides "a few thousand items" (index *pairs*, never *files*) [82] | Cheap, high polish |
| **Login items** | `SMAppService.mainApp` for "Open at Login"; helper/agent via `SMAppService.agent` if a persistent engine is split out — lands in "Allow in Background" with correct responsibility attribution [68][69][70] | macOS 13+ API; no plist hacks |
| **Drive plug-in triggers** | `NSWorkspace.didMountNotification` (and unmount) → run pairs whose trigger is "when <volume> connects"; match volumes by UUID not name [75][29] | CCC-proven UX |
| **Network discovery** | `NWBrowser` for `_foldover._tcp` peers; advertise with `NWListener`+Bonjour; browse `_smb._tcp`/`_afpovertcp._tcp`/`_sftp-ssh._tcp` for NASs; `NSBonjourServices` in Info.plist; all of this needs the **Local Network** permission (below) [76][77][65] | The "Macs & NASs just appear" moment |
| **SFTP/SSH** | Endpoint type, not an app mode. Rust core: SSH crate (evaluate `russh`, `ssh2` — **UNVERIFIED** suitability); Swift fallback: Citadel (SwiftNIO SSH) is proven in shipping apps [78] | Honor `~/.ssh/config`, agent + Keychain |
| **Quick Look** | Space-bar previews via `QLPreviewPanel` (AppKit bridge) in diff & conflict views [27] | Expected Mac behavior |

---

## 5. Onboarding & permissions UX

The first-run sequence is a funnel, not a form. Order matters — ask each thing **in context, at the moment it unlocks something**, never all at first launch:

1. **Welcome (0 friction):** what Foldover does in one line + "Create your first sync" button. No account, no tour slideshow (CCC embeds short Product Tours *on demand* inside help/task UI — copy that) [28][29].
2. **Folder access:** dragging a folder in already grants it (user intent via drop / `NSOpenPanel` security-scoped bookmark — persist bookmarks so scheduled syncs keep working). Only if the engine needs whole-disk visibility (e.g., "back up your whole home folder"), step 3.
3. **Full Disk Access (only when needed):** there is **no API** — the app can't grant itself FDA [71][73]. Best-known pattern, proven by CheatSheet/HoudahSpot/Codex-style onboarding and packaged in OSS (AskForPermission, inket/FullDiskAccess): open System Settings deep-linked to Privacy & Security → Full Disk Access; float a card *beside* the window with your **draggable app icon** ("drop me on the list") + numbered fallback steps; **poll every ~1s and flip the card to a green check the instant it grants**; position next to the Settings window using `CGWindowListCopyWindowInfo` [72][71][73][74]. Tahoe wrinkle: apps don't always auto-appear in the list — guide users to the `+` button too [74]. Frame it positively: "so Foldover can see every folder you choose to sync — including system-protected ones like Desktop and Documents." Skippable ("I'll choose specific folders instead") — FDA is needed for some flows, not all.
4. **Local Network (the first time they pick "a Mac or NAS"):** macOS 15+ gates all Bonjour/local traffic; first browse triggers the system prompt [65]. Pre-empt it with an in-app explanation ("macOS will ask to let Foldover see devices on your network — that's how it finds your NAS and other Macs") so the prompt doesn't arrive cold; declare `NSBonjourServices` for every service type browsed; handle the documented Sequoia-era bugs (re-prompts after updates, forgotten grants — detect "browse yields nothing while permission=denied/undetermined" and link to Settings; the toggle-off-and-on workaround exists for a real bug) [65][66][67].
5. **Notifications:** ask only when the user first turns on "notify me…" or the first conflict/error lands — prompt explains itself [83].
6. **Login item:** expose "Launch Foldover at login" as a normal toggle wired to `SMAppService` — the system handles consent & the Login Items list; do not ask at onboarding at all [68][70].

Golden rule: each permission screen = one sentence of *why*, one button that does the system-level thing, one visible confirmation when it worked, and a visible "skip" so nobody feels trapped. CCC gates FDA+helper auth on the *first task run*, not on launch — same sequencing instinct [29].

---

## 6. Perceived performance

"Fast" is mostly *looking* fast. Concretely for Foldover:

- **Instant window.** Cold start → last-view rendered in <300ms from cached state (last-known pair statuses, cached diff summary from previous run). Skeleton/placeholder rows for what's re-scanning — skeleton screens and progressive/incremental rendering measurably improve perceived speed vs spinners [96].
- **Stream the analysis.** Populate the diff tree as the scan finds items (ForkLift's live folder counter proves users love watching it work [35]); don't block the screen behind "Analyzing…". ChronoSync's honest two-pass tradeoff — indeterminate scan phase, then accurate ETA — is the model; improve on it by making the scan phase *visible progress* rather than a spinner [45][47].
- **The diff tree must be AppKit.** 100k+ changed-file trees are normal in sync (initial sync of a big home folder). `NSOutlineView` with view recycling renders this at 60fps where SwiftUI `List`/`OutlineGroup` measurably falls over (minutes of beachball in published benchmarks) [19][20][21]. Keep row views constant-shaped, pre-filter in the model, fixed row height [22][23][18].
- **Background scanning, opportunistic refresh.** Cheap mtime/size scan continuously (FSEvents-driven), deep content-hash only for candidates; the UI always shows *something* true.
- **Honest numbers.** Throughput/ETA only once the plan exists (§3.5); indeterminate ≠ broken.
- **Cheap ambient presence.** Menu-bar extra must idle at ~0% CPU — iStat Menus 7 advertises 1/5th the CPU of its predecessor as a *feature*; users notice [91].
- **Rust core fits the story:** portable engine doing hashing/copying in parallel; SwiftUI shell stays on the main thread with everything async — UniFFI or swift-bridge are the standard, shipping-proven glue (generate Swift bindings; keep FFI calls coarse-grained: "scan pair" → stream events, not per-file FFI round trips) [94].

---

## 7. Deliverables

### 7.1 Design principles

1. **One window, everything reachable.** Sidebar (pairs/devices/history) → detail → inspector. No document-per-task model (ChronoSync's Organizer+windows is the dated pattern) [46].
2. **Say what will happen before it happens.** Every pair carries a live plain-language plan; every destructive choice shows its consequence in-place (CCC Task Plan, modernized) [29].
3. **Preview is free, sync is deliberate.** First run of any pair forces preview; later runs can skip it but it's one click. Per-item overrides always available [44][50][36].
4. **Glanceable trust.** Idle icon = fine; motion = working; `!` = needs you. One line in the menu bar popover answers "are we good?" (Time Machine contract) [93].
5. **Never lose anything quietly.** Deletes go to a browsable archive; conflicts become a queue to resolve, not files sprinkled with `.sync-conflict` suffixes [28][57].
6. **A real Mac app.** Native controls, Liquid-Glass-correct, keyboard-complete, VoiceOver-clean, Quick Look, Finder-aware when the OS cooperates. The anti-Syncthing-web-GUI [58].
7. **Quiet by default.** Silent success; one notification channel for needs-attention only; granular per-pair notification settings [93][83].
8. **Performance you can watch.** Stream results, live counters, real throughput/ETA, AppKit-speed at 100k rows [35][45][19].
9. **Respect the user's system.** Template menu-bar icon, honors Reduce Transparency/Increase Contrast/Liquid Glass slider, no menu-bar spam (one item), minimal background footprint [2][7][14][91].
10. **Presets in human language, power in inspectors.** Defaults: three preset cards. Everything else (filters, schedules, retention, per-folder rules à la Hazel) one disclosure deeper [31][32].

### 7.2 Information architecture

```
Foldover (LSUIElement-capable: menu-bar app + on-demand main window)
│
├── Menu bar extra (MenuBarExtra, .window style)
│     └── Popover: global status · per-pair mini-rows (state, last sync, Run ▶)
│         · Pause all · Conflicts badge · Open Foldover · Quit/Settings
│
├── Main window (single window; ⌘N opens "New sync pair" tab/sheet)
│     ├── Sidebar (edge-to-edge, colored symbols)
│     │     ├── SYNC PAIRS — each pair: name, state dot, last-sync
│     │     ├── DEVICES — This Mac · other Macs (Bonjour) · NASs · shares
│     │     └── HISTORY — All runs · Conflicts inbox · Archive (deleted files)
│     │
│     ├── Content (selected object)
│     │     ├── Pair view = status card + live plan sentence + activity +
│     │     │   [Preview Changes] [Sync Now] + tabs: Overview·Filters·Schedule·Archive
│     │     ├── Device view = reachable? + pairs involving it
│     │     └── History/Conflicts/Archive browsers
│     │
│     ├── Toolbar (glass): ◧ sidebar | ▶ Sync Now | Preview | + New Pair | share | 🔍 search | inspector ◨
│     └── Inspector (trailing): details of selection — pair settings,
│           selected file info+Quick Look, run details (context-sensitive)
│
├── New Sync Pair (sheet or full-width first-run view)
│     ├── Step 1: drag/drop two locations (local · volume · share · SFTP · Mac)
│     ├── Step 2: preset cards (Mirror / Two-way / NAS) + live plan sentence
│     └── Step 3: name it, first preview runs automatically
│
├── Preview window (per pair, ⌘R): streamed diff tree + filter chips +
│     per-item overrides + estimate footer + [Sync Now]
│
├── Conflict Center (window or inspector-level sheet):
│     side-by-side cards + Quick Look + Keep/Keep both/Skip
│
├── History window section: runs list ↔ run detail (itemized + Reveal + Restore)
│
├── Settings (⌘,): General · Sync behavior · Notifications · Folders&Permissions
│     (FDA/Local Network status+repair) · Advanced (SSH, bandwidth, archive retention)
│
├── Widgets (desktop/NC) + Control Center control (Sync now / Pause)  [81]
├── Shortcuts/App Intents + Spotlight entities  [80][82]
└── Finder Sync extension (context menu + badges — feature-flagged, verify on 26/27) [60][63]
```

### 7.3 Key screens

**Screen A — Main window, pair selected.** Sidebar shows pairs w/ state dots; the content header is the live plan sentence; activity stream below; inspector shows the editable settings for whatever is selected.

```
 ┌─────────────────────────────── Foldover ───────────────────────────────────────┐
 │ ◧      ▶ Sync Now   ◨ Preview    ＋ New Pair            🔍 Search          ◨ │
 ├──────────────┬─────────────────────────────────────────────────┬───────────────┤
 │ SYNC PAIRS   │  Photos ⇄ NAS                          ● up to │ Pair settings │
 │ ▸ ● Photos   │  ────────────────────────────────────────────  │───────────────│
 │   ⇄ NAS      │  Every day at 18:00 or when "MyNAS" is         │ Direction:    │
 │ ▸ ▶ Projects │  reachable — copy new & changed files          │ Two-way       │
 │   → Backup   │  both ways between ~/Pictures/Photos and       │               │
 │   SSD        │  smb://mynas/photos. Deletions are kept in     │ Filters: 3    │
 │ ▸ ⏸ Media    │  Foldover's archive for 30 days. Conflicts     │ rules ▸       │
 │   archive    │  are never auto-resolved.                      │               │
 │              │  [ edit direction ] [ filters ] [ schedule ]   │ Schedule:     │
 │ DEVICES      │                                                │ Daily 18:00 + │
 │ ▸ This Mac   │  Last run: today 09:41 — 214 files, 1.8 GB,    │ on connect    │
 │ ▸ MyNAS.local│  45s · Next: today 18:00                       │               │
 │ ▸ MacBook    │                                                │ Archive:      │
 │   (Anna's)   │  ACTIVITY                       [View all]     │ 30 days       │
 │              │  ──────────────────────────────────────────    │               │
 │ HISTORY      │  ⚠ 09:41  1 conflict — header.psd  [Resolve]   │ [Pause pair]  │
 │ ▸ All runs   │  → 09:40  copied IMG_2041…IMG_2187 (2.1 GB)    │ [Edit…]       │
 │ ▸ Conflicts  │  ← 09:40  pulled edits from MyNAS (14 files)   │               │
 │   inbox (1)  │  ✓ 09:39  analyzed 48,211 items in 3.2s        │               │
 │ ▸ Archive    │                                                │               │
 └──────────────┴─────────────────────────────────────────────────┴───────────────┘
   legend: ● ok   ▶ running   ⏸ paused   ⚠ needs you
```

**Screen B — New sync pair.** Drag-first, presets-second, plan-sentence-always. Locations are typed objects; the right well lists auto-discovered targets (volumes, Bonjour Macs/NASs) so no addresses get typed.

```
 ┌────────────────── New Sync Pair ───────────────────────────────────────────────┐
 │                                                                                │
 │   FROM (this Mac)                    TO                                        │
 │  ┌────────────────────────┐         ┌─────────────────────────────────────┐   │
 │  │                        │         │  ▸ This Mac — choose a folder       │   │
 │  │   📂 ~/Pictures/Photos │   ⇄     │  ▸ Backup SSD (just connected)      │   │
 │  │                        │         │  ▸ MyNAS.local (found on network)   │   │
 │  │  48,211 items · 212 GB │         │  ▸ MacBook — Anna's (Foldover peer) │   │
 │  │                        │         │  ▸ Other… (network share, SFTP)     │   │
 │  └────────────────────────┘         └─────────────────────────────────────┘   │
 │        drop a folder here          smb://mynas/photos  ✓ reachable (1.1 TB)    │
 │                                                                                │
 │   WHAT KIND OF SYNC?                                                           │
 │  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐                            │
 │  │ ◉ Keep both  │ │ ○ Mirror to  │ │ ○ Copy one   │                            │
 │  │   in sync    │ │   the target │ │   way only   │                            │
 │  │ Newest wins. │ │ Target looks │ │ (backup—no   │                            │
 │  │ Conflicts    │ │ like source. │ │ changes flow │                            │
 │  │ are flagged. │ │ Deletes are  │ │ back.)       │                            │
 │  │              │ │ archived.    │ │              │                            │
 │  └──────────────┘ └──────────────┘ └──────────────┘                            │
 │                                                                                │
 │  "Photos on this Mac and MyNAS will be kept identical. 1.8 GB of changes      │
 │   pending right now — you'll review everything before the first sync."        │
 │                                                          [ Create & Preview ] │
 └────────────────────────────────────────────────────────────────────────────────┘
```

**Screen C — Preview (the hero).** One unified streamed tree; action glyphs; filter chips; per-item override; estimate footer with real ETA once analysis completes.

```
 ┌────────── Preview: Photos ⇄ NAS ───────────────────────────────────────────────┐
 │ Analyzing… 48,211 items scanned · 3.2s        ▓▓▓▓▓▓▓▓░░░░░░ scanning 2025/…   │
 │                                                                                │
 │ Filters: [All changes ▾] (●1,516) [New 1,204] [Changed 312] [Deletions 48]     │
 │          [Conflicts 3] [Skipped]  🔍 filter files…        [ ] show unchanged   │
 │ ──────────────────────────────────────────────────────────────────────────── │
 │   ✓  Action     Item                              Size        Modified       │
 │ ──────────────────────────────────────────────────────────────────────────── │
 │   ▸  → 4,211    2025/                             1.1 GB                     │
 │   ▸  ←   104    2026/Trip/                        212 MB                     │
 │      ⚠ conflict  2026/Trip/header.psd             96 MB   mac 09:12·nas 08:55│
 │      →  copy →   2026/Trip/day2/IMG_2041.RAW      41 MB       today 09:12    │
 │      ←  ← copy   2026/Trip/day2/notes.md          12 KB       today 08:55    │
 │      ⌫  archive  2024/drafts/old-banner.psd       66 MB       (deleted here) │
 │      ⊘  skip     2026/Trip/.cache/thumb.db        —           (filter rule)  │
 │   ▸  →   312    Shared/                           880 MB                     │
 │ ──────────────────────────────────────────────────────────────────────────── │
 │ Selected row ▸  [Copy →] [← Copy] [Skip once] [Always skip] [Compare] [Quick │
 │ Look]                                                                        │
 │                                                                               │
 │  1,516 changes · 38.4 GB to copy · est. ~6 min at 112 MB/s · 214 GB free     │
 │  on MyNAS                                         [ Cancel ]   [ Sync Now ] │
 └───────────────────────────────────────────────────────────────────────────────┘
   (Space = Quick Look · click a glyph = override popover · ⌫ on row = exclude)
```

**Screen D — Conflict Center.** Two cards, one decision, batch option. No `.sync-conflict` litter [57][59].

```
 ┌────────── Resolve conflicts — 3 remaining ──────────────────────────────────────┐
 │                                                                                │
 │   header.psd                                                                   │
 │  ┌───────────────────────────────┐   ┌───────────────────────────────┐        │
 │  │  THIS MAC              ◉ pick │   │  MyNAS                 ○ pick │        │
 │  │ ┌─────────────────────────┐ │   │ ┌─────────────────────────┐ │        │
 │  │ │      [Quick Look]       │ │   │ │      [Quick Look]       │ │        │
 │  │ │      thumbnail          │ │   │ │      thumbnail          │ │        │
 │  │ └─────────────────────────┘ │   │ └─────────────────────────┘ │        │
 │  │ Modified: today 09:12       │   │ Modified: today 08:55       │        │
 │  │ Size: 96.2 MB               │   │ Size: 94.8 MB               │        │
 │  │ "exported layers + fix"     │   │ "color pass from NAS edit"  │        │
 │  └───────────────────────────────┘   └───────────────────────────────┘        │
 │                  [ Compare contents… ]  [ Open both in Finder ]               │
 │                                                                                │
 │   ▸ Keep the version from This Mac   ▸ Keep the version from MyNAS            │
 │   ▸ Keep both — save the other as "header (from MyNAS, Oct 16).psd"           │
 │   ▸ Skip — decide later                                                       │
 │                                                                                │
 │   [ ] Apply this choice to the remaining 2 conflicts          ← previous  next →│
 │                                                    [ Resolve & continue ▸ ]   │
 └────────────────────────────────────────────────────────────────────────────────┘
```

**Screen E — Menu bar popover.** Time Machine's contract, modernized: status line first, per-pair controls, one tap to anything [93][15][14].

```
        ┌──────────────────────────────────────────┐
        │  ● All good — last activity 09:41        │
        │  ──────────────────────────────────────  │
        │  Photos ⇄ NAS          ● up to date  ▶   │
        │  Projects → Backup SSD ▶ 62% · 112 MB/s  │
        │  Media archive         ⏸ paused      ▶   │
        │  ──────────────────────────────────────  │
        │  ⚠ 1 conflict needs you           Resolve │
        │  ──────────────────────────────────────  │
        │  ⏸ Pause all      ⧉ Open Foldover        │
        │  ⚙ Settings…                    ⏻ Quit   │
        └──────────────────────────────────────────┘
   icon states:  ◌ idle · ◍ animating (syncing) · ◌+! attention · ◌∥ paused
```

### 7.4 Architecture note (UI ↔ core boundary)

- Rust core (scan/hash/copy/schedule/state) + Swift shell via **UniFFI or swift-bridge** — both used in shipping apps; keep the boundary coarse: commands in, event streams out (`scanProgress`, `planReady`, `itemDone`, `conflict`) [94].
- UI stack: SwiftUI for shell/scenes/settings/popover; **AppKit `NSOutlineView`** for the diff tree; AppKit bridge for `QLPreviewPanel`, `NSWorkspace` notifications, `SMAppService` [19][21][27][75][68].
- One binary contains: app + `MenuBarExtra` + Settings + widgets + intents + (feature-flagged) Finder Sync ext. Optional launch-agent engine only if syncs must run while the GUI is closed — `SMAppService.agent` handles consent cleanly [68][69][70].
- The Raycast counterexample is worth naming: they went web-stack *for Windows*; Foldover's stated path (portable core + native shells) is the other valid answer and the one that keeps the Mac app feeling like a Mac app — at the cost of a second UI later for Linux [88].

### 7.5 Open questions / UNVERIFIED

- **Finder Sync on macOS 27 GA** — still partially broken per late-2026 user reports; needs device testing before committing badge/context-menu scope [61][63].
- Golden Gate Liquid Glass slider behavior at the extremes for custom glass controls — needs hands-on, docs are thin [7][10].
- Rust SSH/SFTP crate choice (`russh` vs `ssh2` vs spawning `ssh`) — **UNVERIFIED**; evaluate.
- Whether macOS 27 restored or intends to restore full `FinderSyncExtensionHost` behavior in a point release — UNVERIFIED.
- SwiftUI `Table` real-world ceiling on Golden Gate (reports are 26-era; Apple claims ongoing improvements) — prototype with 100k rows [19][23].

---

## Sources

[1] Apple Newsroom — "Apple introduces a delightful and elegant new software design" (2025-06-09): https://www.apple.com/newsroom/2025/06/apple-introduces-a-delightful-and-elegant-new-software-design/
[2] Apple — Adopting Liquid Glass (Technology Overviews): https://developer.apple.com/documentation/technologyoverviews/adopting-liquid-glass.md
[3] WWDC25 — "Build a SwiftUI app with the new design": https://developer.apple.com/videos/play/wwdc2025/323/
[4] WWDC25 — "Build an AppKit app with the new design" (glass toolbar, floating sidebar, concentricity, NSSplitViewController inspector, NSItemBadge): https://developer.apple.com/videos/play/wwdc2025/310/
[5] The Verge — "Apple announces macOS 27 Golden Gate…" (WWDC26): https://www.theverge.com/tech/943695/apple-wwdc-2026-macos-27-macbook-mac-announcement-features
[6] TechRadar — "macOS 27 Golden Gate… everything you need to know" (release 2026-09-14; Apple-silicon only): https://www.techradar.com/computing/mac-os/macos-27-golden-gate-announced-at-wwdc-2026-heres-everything-you-need-to-know
[7] The Verge — "Apple dials down Liquid Glass, and the Mac looks way better for it" (opacity slider, edge-to-edge sidebars, colored icons): https://www.theverge.com/tech/946567/apple-macos-27-golden-gate-dev-beta-hands-on
[8] MacRumors — macOS 27 Golden Gate roundup: https://www.macrumors.com/roundup/macos-27/ and WWDC 2026 recap: https://www.macrumors.com/2026/06/08/wwdc-2026-recap/
[9] Apple — Design "What's new" (HIG changelog: sidebar icon colors, adaptable sidebar, menus icons, scroll edge effects, Icon Composer 2): https://developer.apple.com/design/whats-new/ and https://developer.apple.com/wwdc26/guides/design/
[10] Apple — macOS 27 Golden Gate Release Notes (menu item images hidden by default in SwiftUI menus; tabs picker style in sidebars/inspectors): https://developer.apple.com/documentation/macos-release-notes/macos-27-release-notes
[11] TelemetryDeck-derived stats via aboutchromebooks (Tahoe 86.0% wk of 2026-07-27; launch 2025-09-15): https://www.aboutchromebooks.com/macos-version-distribution-statistics/
[12] Homebrew Formulae analytics, OS versions 30d (Tahoe 33.8%, Golden Gate 18.3%, Sequoia 15.4%): https://formulae.brew.sh/analytics/os-version/30d/
[13] Mario Guzmán — Toolbar Guidelines (sidebar toggle anchoring, inspector/search ordering): https://marioaguzman.github.io/design/toolbarguidelines/ ; Apple HIG Toolbars: https://developer.apple.com/design/human-interface-guidelines/toolbars ; HIG Designing for macOS: https://developer.apple.com/design/human-interface-guidelines/designing-for-macos/
[14] Bjango (Marc Edwards) — "Designing macOS menu bar extras" (22pt height, 16pt glyph, template images, 35% opacity): https://bjango.com/articles/designingmenubarextras/
[15] Apple — MenuBarExtra docs: https://developer.apple.com/documentation/swiftui/menubarextra ; isInserted init: https://developer.apple.com/documentation/swiftui/menubarextra/init(_:isinserted:content:)
[16] WWDC22 — "Bring multiple windows to your SwiftUI app" (MenuBarExtra scene, menu vs window styles): https://developer.apple.com/videos/play/wwdc2022/10061/
[17] Apple — SwiftUI `Settings` scene + TabView pattern; `openSettings` deep-link: https://developer.apple.com/documentation/swiftui/settings ; https://developer.apple.com/documentation/swiftui/opensettingsaction
[18] Michael Tsai — "NSTableView With SwiftUI" (SwiftUI Table perf issues; context-menu-on-right-click gap; hybrid pattern): https://mjtsai.com/blog/2024/04/15/nstableview-with-swiftui/
[19] Apple Developer Forums — "SwiftUI Hierarchical List Performance" (100k items → minutes unresponsive on Mac Studio): https://developer.apple.com/forums/thread/761829
[20] lemonmojo — swiftui-hierarchical-list-performance (NSOutlineView/UICollectionView fast on same data): https://github.com/lemonmojo/swiftui-hierarchical-list-performance/
[21] Apple Developer Forums — "OutlineGroup is not lazy loading children": https://developer.apple.com/forums/thread/662937
[22] Stack Overflow — "Bad performance of large SwiftUI lists on macOS" (List not lazy; Table is lazy; fixed row height trick): https://stackoverflow.com/questions/72070486/bad-performance-of-large-swiftui-lists-on-macos
[23] WWDC26 — Power and Performance Group Lab (large-list guidance: constant count, filter in model): https://developer.apple.com/videos/play/wwdc2026/8003/
[24] Eclectic Light — "Explainer: AppKit and SwiftUI" (2026-04-04; still-common AppKit dips; Observable macro floor): https://eclecticlight.co/2026/04/04/explainer-appkit-and-swiftui/
[25] Paulo Andrade — "A WWDC 26 Update on Building a Mac-assed App with SwiftUI" (.onDragSessionUpdated; context-menu gap; two-OS-version support norm): https://pfandrade.me/blog/swiftui-mac-assed-wwdc26-update/
[26] DigitalBlake — "AppKit vs SwiftUI on macOS: Layout, Performance, and Trade-offs" (2026-04-28; view recycling): https://digitalblake.com/2026/04/28/swiftui-vs-appkit-macos-ui-performance/
[27] QLPreviewPanel from SwiftUI (responder chain bridging): https://stackoverflow.com/questions/59696367/how-do-i-show-qlpreviewpanel-with-swiftui-on-macos ; NSTableView + Quick Look: https://devgypsy.com/post/2023-06-06-quicklook-swift-tableview/
[28] Bombich — "What's new in CCC 7" (Task Notes, product tours, network-condition rules, Quick Update default): https://support.bombich.com/hc/en-us/articles/20686482705687-What-s-new-in-CCC-7 ; press release: https://bombich.com/press
[29] CCC 7 documentation PDF (Backup Volume Setup Assistant; "when source or destination is reconnected" scheduling; FDA + privileged helper on first run): https://bombich.com/doc-pdf/ccc7-documentation-en.pdf ; Task Plan summary description: https://manualzz.com/doc/23185281/bombich-carbon-copy-cloner-user-manual ; SafetyNet explanation: https://www.podfeet.com/blog/2019/11/carbon-copy-cloner/
[30] Bombich — CCC download/version support (Ventura 13.1+ → Golden Gate 27): https://bombich.com/download
[31] Noodlesoft — Hazel manual: main window (toolbar, preview rule, detach editor): https://www.noodlesoft.com/manual/hazel/hazel-basics/main-hazel-window/ ; edit/detach rules: https://www.noodlesoft.com/manual/hazel/work-with-folders-rules/create-edit-rules/edit-or-remove-a-rule/ ; layouts: https://www.noodlesoft.com/manual/hazel/hazel-basics/hazel-preferences/
[32] Noodlesoft — "What's New in Hazel 6" (macOS 13+; revert via Finder menu; error highlighting): https://www.noodlesoft.com/whats-new-in-hazel-6/ ; https://www.noodlesoft.com/kb/hazel-6-0-release-notes/ ; https://www.noodlesoft.com/kb/hazel-6-faq/
[33] Panic — "Synchronizing Folders with Transmit 5" (Simulate dry-run; report): https://help.panic.com/transmit/transmit5/synchronize/ ; video: https://help.panic.com/transmit/videos/all-about-file-sync/
[34] Panic — "What's new in Transmit 5" (local-local/remote-remote, selectable sync, per-file progress): https://help.panic.com/transmit/transmit5/features/ ; https://www.panic.com/transmit/
[35] BinaryNights — "ForkLift 4 beta is finally here" (faster analyze + live counter; filter learning curve; preview pane): https://blog.binarynights.com/2023/04/04/forklift-4-beta-is-finally-here/
[36] BinaryNights — "ForkLift 4.0.3" (per-item deselect in Sync Window): https://blog.binarynights.com/2023/11/14/forklift-4-0-3-is-available/
[37] BinaryNights — ForkLift 4 homepage (compare local/remote, one-click 1- or 2-way sync, 20× faster analyzation): https://binarynights.com/
[38] Kaleidoscope homepage (folder compare, Quick Look, filtering, file history): https://kaleidoscope.app/
[39] Kaleidoscope release notes (live working-copy updates; external-editor diff hand-off incl. BBEdit/VSCode/Zed): https://cloud.kaleidoscope.app/support/release-notes
[40] Kaleidoscope help — "Text Diffs and Colors" (green=added, red=deleted, blue=changed): https://kaleidoscope.app/help/docs/text-diffs-and-colors
[41] PCWorld — "Arq 7 Backup review" (2026-03-04; super-clean UI; boring is good): https://www.pcworld.com/article/1953731/arq-7-backup-review.html
[42] Arq docs — "Creating a Backup Plan" (storage → encryption → all-drives/user-data/pick-folders): https://www.arqbackup.com/documentation/arq7/English.lproj/createBackupPlan.html
[43] Michael Tsai — "Arq 7" (Electron→native rewrite; integrated logs; global menu per-backup status): https://mjtsai.com/blog/2021/02/15/arq-7/
[44] Econ Technologies — ChronoSync features (Trial Sync; per-item override in selector sheet): https://www.econtechnologies.com/chronosync/features.html ; https://www.econtechnologies.com/chronosync/guide-sync-doc-fundamentals.html
[45] Econ — "Understanding ChronoSync's Progress Feedback" (one-pass vs two-pass; honest ETA): https://www.econtechnologies.com/chronosync/tn-cs-progress-feedback.html
[46] Econ — "What's New in v11" PDF (Task Groups, Task Search): https://downloads.econtechnologies.com/manuals/Whats_New_in_v11.pdf ; PR: https://www.econtechnologies.com/pr/110623.html ; release notes (VoiceOver fixes, Tahoe compat): https://www.econtechnologies.com/support/chronosync/notes.html
[47] Econ — "What's New in v12" PDF (optional two-pass synchronizer; deletions-first): https://downloads.econtechnologies.com/manuals/Whats_New_in_v12.pdf
[48] Macworld — ChronoSync review (2024-12-12; no ETA shown; dated tutorials; organizer quibbles): https://www.macworld.com/article/2554686/chronosync-review-a-mac-backup-option-with-a-justified-fan-base.html
[49] Econ — ChronoAgent (remote-Mac agent; encrypted, faster than File Sharing; parallel scanning): https://www.econtechnologies.com/chronoagent/what-is-chronoagent.html ; https://www.econtechnologies.com/chronoagent/overview.html ; setup guide: https://www.econtechnologies.com/how-to-sync-files-between-2-macs/
[50] GoodSync — manual (Analyze → review arrows → per-item direction change → Sync): https://www.goodsync.com/manual ; job creation: https://help.goodsync.com/hc/en-us/articles/115002574052-How-to-create-a-new-GoodSync-job-Windows-and-Mac
[51] GoodSync help — "Analyze" options (estimate disk space; detect moves/renames; checksum options): https://help.goodsync.com/hc/en-us/articles/34055969744653-Analyze ; Left/Right side options: https://help.goodsync.com/hc/en-us/articles/360003108112-Left-Side-Right-Side
[52] SoftwareAdvice — GoodSync reviews ("GUI is improvable… overwhelming" sentiments): https://www.softwareadvice.com/file-sync/goodsync-profile/ ; GetApp summary: https://www.getapp.com/it-management-software/a/goodsync/
[53] FreeFileSync — manual quick start & main dialog (compare → categories → sync): https://freefilesync.org/manual.php?topic=freefilesync ; comparison variants: https://freefilesync.org/manual.php?topic=comparison-settings
[54] PCWorld — FreeFileSync 13.1 review ("not the prettiest", daunting options, morphable panes): https://www.pcworld.com/article/2144389/freefilesync-review.html
[55] MakeUseOf — FreeFileSync review ("interface feels like it's stuck in 2010… cluttered"): https://www.makeuseof.com/freefilesync-open-source-file-backup-app-is-so-good-i-can-tbelieve-its-free/
[56] Electronic Team — SyncMate product page (v8.11.604, released 2026-08-27; macOS 12.4+): https://www.electronic.us/products/syncmate/ ; release notes: https://help.electronic.us/support/solutions/articles/44002222481-what-s-new-in-this-version
[57] Syncthing docs — Conflicting Changes (`.sync-conflict-<date>-<time>-<modifiedBy>` naming; propagated; case conflicts): https://docs.syncthing.net/users/syncing.html
[58] Syncthing forums — macOS UX critiques: wrapper vs web GUI (road to v2): https://forum.syncthing.net/t/syncthing-for-macos-the-road-to-v2-official-application/26060 ; "This is why we need a mac app installer": https://forum.syncthing.net/t/this-is-why-we-need-a-mac-app-installer/11709 ; Synctrain native-app thread: https://forum.syncthing.net/t/syncthing-native-app-for-macos-synctrain-ios-based/22885
[59] Dropbox Help — "What's a conflicted copy?" (naming convention; manual merge): https://help.dropbox.com/organize/conflicted-copy
[60] Dropbox Community — "Finder sync icons aren't working on macOS 26 Tahoe" (`FinderSyncExtensionHost` gone): https://community.dropbox.com/en/discussion/861142/dropbox-finder-sync-icons-arent-working-on-mac-os-26-tahoe-both-legacy-file-provider-versions
[61] Dropbox Community — "Sync status icons missing… (macOS 27 Golden Gate, still broken)": https://community.dropbox.com/en/discussion/862015/sync-status-icons-missing-in-finder-after-reverting-from-file-provider-to-legacy-macos
[62] Nextcloud — "The FinderSync extension does not work" (issue #10478): https://github.com/nextcloud/desktop/issues/10478 ; fix PR: https://github.com/nextcloud/desktop/pull/9529 ; File-Provider-vs-FinderSync conflict: https://help.nextcloud.com/t/no-more-classic-sync-virtual-files-in-v34/247803/7
[63] Apple Developer Forums — "FIFinderSync Extension fails to load on macOS 26.3.1" (pluginkit `!`, host process missing): https://developer.apple.com/forums/thread/819937
[64] Apple — Finder Sync extension point (badges, context menus, toolbar button; sandboxed XPC): https://developer.apple.com/library/archive/documentation/General/Conceptual/ExtensibilityPG/Finder.html
[65] Apple — TN3179 "Understanding local network privacy" (macOS since Sequoia; NSBonjourServices; all Bonjour ops need it): https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy.md
[66] Michael Tsai — "Local Network Privacy on Sequoia" (re-prompt bugs in 15.2/15.4): https://mjtsai.com/blog/2024/10/02/local-network-privacy-on-sequoia/ ; duplicate-bundle-ID prompt bug: https://openradar.appspot.com/15568200
[67] Rogue Research — "Local Network access on macOS 15 Sequoia" (off/on workaround; forgotten grants): https://www.rogue-research.com/2025/05/local-network-access-on-macos-15-sequoia/
[68] Apple — SMAppService (macOS 13+; register login items/agents/daemons): https://developer.apple.com/documentation/servicemanagement/smappservice.md ; register(): https://developer.apple.com/documentation/servicemanagement/smappservice/register().md
[69] Apple Deployment — "Manage login items and background tasks on Mac": https://support.apple.com/guide/deployment/manage-login-items-background-tasks-mac-depdca572563/web
[70] Apple Developer Forums — "Porting existing LoginItem logic to SMAppService" ("Open at Login" vs "Allow in the Background" semantics): https://developer.apple.com/forums/thread/713830
[71] inket/FullDiskAccess — query + prompt helper; auto-adds app (unchecked) to list on 10.15+: https://github.com/inket/FullDiskAccess
[72] riko2chen/AskForPermission — floating guide card + draggable app icon, tracks Settings window: https://github.com/riko2chen/AskForPermission
[73] Michael Tsai — "People Don't Read Similar Dialogs" (drag-app-icon-into-Settings pattern; CheatSheet/HoudahSpot videos; window positioning): https://mjtsai.com/blog/2019/01/28/people-dont-read-similar-dialogs/
[74] vdavid/cmdr commit — FDA onboarding notes incl. Tahoe: app may not auto-appear; deep-link pane; poll/probe techniques: https://github.com/vdavid/cmdr/commit/16918218
[75] Apple — NSWorkspace.didMountNotification: https://developer.apple.com/documentation/appkit/nsworkspace/didmountnotification ; volume URL in userInfo: https://stackoverflow.com/questions/30768137/detect-volume-mount-and-get-its-path
[76] Apple — NWBrowser: https://developer.apple.com/documentation/network/nwbrowser ; service-type browsing caveats: https://stackoverflow.com/questions/76576518/swift-nwbrowser-bonjour-service-error-nwbrowser-has-no-member-service
[77] WWDC19 — "Advances in Networking, Part 2" PDF (NWBrowser/NWListener Bonjour patterns): https://devstreaming-cdn.apple.com/videos/wwdc/2019/713b310k72bjpcmz6xn/713/713_advances_in_networking_part_2.pdf?dl=1
[78] Citadel SFTP (pure-Swift SwiftNIO SSH SFTPClient): https://github.com/orlandos-nl/Citadel/blob/main/Sources/Citadel/SFTP/Client/SFTPClient.swift ; macSCP (shipping app built on it): https://www.macscp.co/
[79] Apple — Defining your app's Focus filter; WWDC22 "Meet Focus filters": https://developer.apple.com/documentation/appintents/defining-your-app-s-focus-filter ; https://developer.apple.com/videos/play/wwdc2022/10121/
[80] WWDC25 — "Develop for Shortcuts and Spotlight with App Intents" (run app actions from Spotlight on Mac): https://developer.apple.com/videos/play/wwdc2025/260/ ; App Shortcuts docs: https://developer.apple.com/documentation/appintents/app-shortcuts
[81] Apple — WidgetKit (Mac widgets on desktop + Notification Center); WWDC25 "What's new in widgets" (controls in Control Center on macOS Tahoe): https://developer.apple.com/documentation/widgetkit.md ; https://developer.apple.com/videos/play/wwdc2025/278/
[82] Apple — Core Spotlight app-content indexing (private on-device index; "a few thousand items" guidance): https://developer.apple.com/library/archive/documentation/General/Conceptual/AppSearch/AppContent.html ; WWDC24 semantic search: https://developer.apple.com/videos/play/wwdc2024/10131/
[83] Apple — UserNotifications handling/actions docs: https://developer.apple.com/documentation/usernotifications/handling-notifications-and-notification-related-actions ; ask-in-context pattern: https://flaviocopes.com/courses/build-macos-apps-swiftui/request-notification-permission-in-context/ ; macOS quirks (.provisional ignored; prompt once): https://github.com/LaughingJackalope/macos-skills/blob/main/push-notifications-macos/SKILL.md
[84] WWDC25 — "Make your Mac app more accessible to everyone" (container navigation; keyboard control): https://developer.apple.com/videos/play/wwdc2025/229/ ; App Store Connect VoiceOver evaluation criteria: https://developer.apple.com/help/app-store-connect/manage-app-accessibility/voiceover-evaluation-criteria/
[85] Apple — String Catalogs (auto-extraction; plural variants; .xcloc export): https://developer.apple.com/documentation/xcode/localizing-and-varying-text-with-a-string-catalog ; plurals: https://developer.apple.com/documentation/xcode/localizing-strings-that-contain-plurals ; WWDC26 agent translation (Xcode 27): https://developer.apple.com/videos/play/wwdc2026/213/
[86] WWDC25 — "What's new in SF Symbols 7" (Draw On/Off, Variable Draw for progress): https://developer.apple.com/videos/play/wwdc2025/337/ ; https://developer.apple.com/sf-symbols/ ; https://9to5mac.com/2025/06/11/apple-releases-sf-symbols-7-beta/
[87] (merged into [27])
[88] Raycast — "A Technical Deep Dive Into the New Raycast" (v1 native AppKit; rebuilt on web stack for cross-platform): https://www.raycast.com/blog/a-technical-deep-dive-into-the-new-raycast ; Console interview (custom keyboard-first AppKit UI): https://console.dev/interviews/raycast-petr-nikolaev ; Launch HN: https://news.ycombinator.com/item?id=22466994
[89] Cultured Code — Things Design Award posts (ADA 2009 & 2017): https://culturedcode.com/things/blog/2017/06/back-from-wwdc/ ; https://culturedcode.com/things/blog/2009/06/things-wins-apple-design-award-2009/ ; sspai interview (animation philosophy): https://sspai.com/post/39724
[90] CleanShot X — features (Quick Access Overlay): https://cleanshot.com/features ; https://cleanshot.com/ ; TYPENORM teardown: https://typenorm.com/apps/cleanshot
[91] Bjango — iStat Menus (7.5, Liquid Glass menus for macOS 27; 1/5th CPU of v6; combined mode; macOS 13.5+): https://bjango.com/mac/istatmenus/ ; https://bjango.com/mac/istatmenus/versionhistory/ ; https://bjango.com/help/istatmenus7/combined/
[92] Ice (GPL menu-bar manager, macOS 14+; ~29.8k★; development state): https://github.com/jordanbaird/Ice ; Thaw fork for 26/27: https://github.com/stonerl/Thaw/ ; icemelt fork noting Golden Gate's built-in hiding: https://github.com/pnikolaidis/icemelt ; macOS 27 overflow/⌘-drag: https://crestnotch.app/menu-bar-icons-hidden-behind-notch ; https://brow-app.com/blog/macos-27-menu-bar ; Apple ⌘-drag: https://support.apple.com/guide/mac-help/whats-in-the-menu-bar-mchlp1446/mac
[93] Time Machine menu-bar behavior (animate while copying, `!` on failure, status line, Back Up Now/Skip): https://trymainspring.com/blog/time-machine-menu-bar-icon-mac ; icon states archive (pondini): https://tommyang.github.io/pondini.org/TM/24.html
[94] Rust↔Swift bridging: swift-bridge: https://github.com/chinedufn/swift-bridge/blob/master/README.md ; UniFFI→Swift walkthrough: https://strathweb.com/2023/07/calling-rust-code-from-swift/ ; production Rust-core/SwiftUI example: https://forgeandcode.io/blog/building-rust-swift-ffi-bridge/ ; jayjay UniFFI shell: https://deepwiki.com/hewigovens/jayjay/1.1.2-uniffi-bindings-and-cli-dispatch
[95] heise — Mac backup/sync tools comparison (version landscape: Arq 7.26.6, ChronoSync 11.0.1, GoodSync 12.5.4, ForkLift 4.1.8): https://www.heise.de/en/guide/Flexible-cloud-storage-for-backups-how-it-works-with-the-Mac-10252669.html?seite=3
[96] Perceived performance: NN/g "Skeleton Screens 101": https://www.nngroup.com/articles/skeleton-screens/ ; MDN "Perceived performance": https://developer.mozilla.org/en-US/docs/Learn_web_development/Extensions/Performance/Perceived_performance ; ACM "false front" study: https://dl.acm.org/doi/full/10.1145/3735593

*End of report.*
