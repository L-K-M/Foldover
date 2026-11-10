# Foldover improvement backlog

Foldover previews and syncs folder pairs through one Rust agent and a native
Mac app. Archives provide endpoint-local, short-term recovery.

Consolidated 4 November 2026 from the prior Sol/Fusion/Mimo/Muse/Step reviews
and the local Sol review of `origin/main` at `d081589`. Historical identifiers,
evidence and acceptance conditions are retained for unresolved work. Source
line numbers describe their review baseline unless current evidence is stated.
Stronger historical priorities are preserved when optional findings overlap.
Resolved entries, obsolete PR result tables, findings implemented in open PRs
209–211 and 213–228, and work merged through PR 212 are omitted. Feature PRs
remain open for owner review unless already merged.

The original review assumed one person, disjoint sync roots, 100k–1M tracked
files and interrupted large transfers. Linux source and CI evidence, simulations
and microbenchmarks do not qualify physical NAS behavior, native frame time,
mixed displays or VoiceOver. The current native store regressions passed; they
do not establish those broader performance or accessibility claims. Retained
scan-derived claims require reproduction before fixes, particularly lease races,
FSEvents replay, platform hardening and SwiftUI tab/layout behavior. The owner
retains hardware validation, hands-on VoiceOver and release decisions.

Current validation: the combined native app build and all 267 Swift tests passed.
The full native Rust gate remains blocked by unchanged foldover-fs tests
`root::extended::tests::changing_resource_forks_cannot_extend_the_observed_work_or_publish_a_digest`
and `root::write::tests::quarantine_stays_behind_where_a_run_leaves_it_alone`.
Targeted reruns reproduced both failures under umask 022; the feature changes do
not modify filesystem-driver or types code. Resource-fork progress includes an
OS-added 11-byte com.apple.provenance attribute, while the test compares it with
the fork length alone; Darwin also overwrites shorter forks without truncating.
These are test/host qualification issues, not evidence of new data loss. The
quarantine metadata issue below has independent native reproduction.

Preserve the single-writer engine, review-token approvals, endpoint-local
archives and layer boundaries. Automatic manual and scheduled runs are an
explicit owner decision (PLAN.md §3.6); optional UI ideas do not override it.

### SOL-04: Searching a preview searches only downloaded rows

missing features — important

What this is: The toolbar offers path search over a preview.

Problem: `PairSyncViewModel.swift:386-449` filters locally; the
protocol's first response contains at most 100 items
(`crates/foldover-proto/src/local/sync.rs:14-16`). A matching path later
in a large plan can yield an empty search until Show More is clicked
repeatedly. Existing pagination is useful, but it does not make search
complete.

Suggested fix: Add path filtering to token-bound server paging. Say how many
results are loaded and retain a clear search scope in the interim.

If skipped: Users can mistake an unseen destructive change for a
change that does not exist.

Acceptance: Find a match beyond row 10,000 without downloading the
preceding rows; stale review tokens still fail closed.

Related shipped work: PR 160 (`sync ID --changes`) pages the full
preview over the CLI; server-side filtered search remains open.

Mimo addendum: until search reaches the whole plan, the empty
state should not claim "No Matching Changes" while unloaded pages
remain; say the search covers the changes loaded so far.

Merged SOL26-12: Current source: `apple/Foldover/ViewModels/PairSyncViewModel.swift:618` and `Views/FoldSheet.swift:525`. Hidden-row counts disclose part of the scope; the empty search result still needs that context. Effort: one day.

### SOL-05: Large-file progress looks frozen

performance problems, stuttering, and jank — important

What this is: Progress counts committed operations and their bytes.

Problem: `crates/foldover-engine/src/run/copies.rs:166-175` reports
copied bytes when a whole file lands. `PairSyncViewModel.swift:305-330`
displays that count. One 20 GB transfer can show no byte progress for
minutes; no throughput or reliable remaining-time estimate is exposed.

Suggested fix: Report bytes in flight through the endpoint abstraction,
separately from committed bytes. Throttle publication, show the current
file, and estimate remaining time only after enough stable samples.

If skipped: Normal transfers resemble hangs and invite cancellation.

Acceptance: A throttled large file advances while being copied;
retries do not double-count, and cancellation remains responsive.
Add a percentage caption and an explicit accessibility value to the
existing progress bar as a small interim improvement.

Related shipped work: PR 164 adds the cumulative copy rate
(`SyncProgress.rate`) and an `N MB/s · about T left` status segment,
hidden while the job waits on another Mac. In-flight per-file byte
progress remains open. The consolidation retains 169's unit promotion,
duration formatting and tests over 164's agent-owned clock.

Related optional ideas (MUSE-06c, MIMO-25a): a menu-bar progress ring or a filling arrow can reuse reliable byte progress after in-flight bytes exist; avoid adding another unverified progress clock.

### SOL-06: Profile cached preview rendering at scale

performance problems, stuttering, and jank — important

Implemented: PR 178 caches rows across renders by store revision,
including state, pages and kind filters. Search filters cached rows.

Acceptance: Profile 10k, 100k and 200k loaded rows on macOS; typing
and selection must avoid repeated full presentation rebuilds and meet
PLAN.md's interaction budgets.

Merged SOL26-08: Current source: `apple/Foldover/Views/FoldSheet.swift:153-155,192` and `ViewModels/PairSyncViewModel.swift:490`. FoldLine mapping and grouped entries remain outside the row cache. Native jank is unmeasured. Effort: half a day.

Additional evidence (fifth pass): `PairSyncViewModel.swift:618-621` still filters loaded rows on each keystroke. Profile selection and typing before adding filtered-result caching or a debounce. The fold also maps and groups presentation collections after row formatting has been cached. No native frame-time regression has been measured.

Suggested fix: profile the retained acceptance sizes, including the fold's
mapping/grouping and search/selection paths. Cache by row revision only where
measurements show material repeated work. If search is the measured bottleneck,
cache results by `(searchText, rowsRevision)` or debounce input by roughly 100
milliseconds, then verify typing against at least 10,000 loaded rows.

### SOL-07: Pair-local copy limits do not protect a shared disk

performance problems, stuttering, and jank — important

What this is: Copies have sensible concurrency within one pair.

Problem: The documented limit is per pair, not per physical device
(`docs/guide/limitations.md:97-108`). Several independent pairs on one
spinning disk or NAS multiply workers and can turn sequential I/O into
seeks and queueing. This is a known limitation.

Suggested fix: Budget workers by held volume/device identity, with a
conservative fallback for unknown devices. Keep pair bandwidth limits
separate and cancellation-aware.

If skipped: Sync All can make each job slower and the Mac less
responsive even though each pair stays within its own limit.

Acceptance: Two HDD-backed pairs share the disk's worker count;
independent disks still run concurrently. Benchmark total completion
time and foreground latency.

Related shipped work: PR 161 (`sync --all`) already caps CLI fan-out
at four concurrent pair jobs; the per-device worker budgeting it
approximates remains engine work.

### SOL-08: Retained archive runs eventually disappear from browsing

missing features — important

What this is: Endpoint archives retain versions for 30 days.

Problem: `crates/foldover-service/src/archive.rs:27-42` truncates
runs to `ARCHIVE_RUN_LIMIT` (1000) and exposes only `more`; there is no
run cursor. Frequently changing pairs can retain older versions that
the app cannot reach. The UI admits older runs are omitted.

Suggested fix: Cursor-page run history, preserving newest-first ordering;
bind cursors to the pair revision, add a Load Older action and offer date
narrowing without loading every run into the app.

If skipped: Users cannot browse all versions that retention promises
are still present.

Acceptance: Reach a version older than the first 1000 runs through
both app and CLI; additions during paging must not duplicate rows.

Merged SOL26-13: Current source: `crates/foldover-service/src/archive.rs:33-34` and `apple/Foldover/ViewModels/ArchiveViewModel.swift:92`. The 1,000-run cap has a more flag and no run cursor. Effort: one day.

PR 211 pages archived items while restoring one run. It does not page the
archive-run list and therefore does not make older runs browseable.

### SOL-09: Background failures need an app-independent signal

missing features — important

What this is: Sync keeps running when the app quits.

Problem: Failure notifications are delivered by the app; with it
closed, stopped automatic runs do not alert anyone
(`docs/guide/limitations.md:38-41`). This is documented, but undermines
the background-agent workflow.

Suggested fix: Persist attention events in the agent and deliver them through
the existing native notification boundary with explicit permission.
Coalesce repeats and preserve events until acknowledged.

If skipped: A scheduled backup can remain stopped unnoticed.

Acceptance: Quit the app, cause a scheduled failure, and receive one
actionable alert. Restarting must not duplicate an acknowledged alert.

### SOL-10: Recovery needs error details, not only a failure count

usability — important

What this is: The service already returns individual restore
failures with paths and messages.

Problem: `CoreArchiveClient.putBackAll` reduces the response to
`RunRestore.failures: UInt64` (`ArchiveStore.swift:33-39, 70-72`). Users
learn that files stayed archived but cannot see why or retry just those
failures from that result.

Suggested fix: Preserve the bounded failure list and total count through the
store. Show failed items with actionable causes and retry selection.

If skipped: Permission, disconnected-folder and name-collision
problems all look like the same unexplained count.

Acceptance: A mixed restore names each returned failure, retains
the total when truncated, and never labels a partial result successful.

Merged SOL26-14: Current source: `apple/Foldover/Stores/ArchiveStore.swift:39-43,75-77`. The store keeps only the failure count while the agent returns bounded paths/messages. Effort: half a day.

### SOL-11: Qualify real storage and UI performance before broad trust

general issues — important

What this is: The project has simulations, filesystem image tests
and five Rust microbenchmarks.

Problem: They do not establish real NAS throughput, end-to-end
million-file memory, 200k-row UI smoothness, agent idle power, or write
fidelity on physical foreign filesystems. These gaps are acknowledged
in `docs/guide/limitations.md` and `docs/hardware-validation.md`.

Suggested fix: Execute the existing hardware checklist and add retained
measurements for the missing PLAN.md section 5.3 budgets. Include cold
and warm runs, sleep/wake, low free space and disconnected volumes.

If skipped: Passing synthetic tests can be mistaken for verified
performance and recovery on a user's actual disks.

Acceptance: Record hardware, dataset, build SHA, measurements and
failures. The owner retains release and hands-on VoiceOver decisions.

Historical measurement context for SOL-11: the original Linux review at
`8fb2405` passed cargo xtask ci and five microbenchmarks: scan 10k files
620.8 ms, hash 64 MiB 17.7 ms, plan 100k entries 114.6 ms, DB commit 100k
254.5 ms, DB read 100k 76.3 ms. These are dated host measurements, not proof
of NAS throughput, app frame time or current native-driver behavior. Prior
single-occurrence failures in peer metadata scanning and installed-agent
registration warrant stabilization if they recur; neither was established as
a defect in the shipped improvements.

Merged PR 212 adds measurements from an M4 Max Mac Studio and internal APFS
for the ADR 0046 snapshot, archive, polling and relayed-SFTP changes. The host
was heavily shared, and the record explicitly does not qualify a physical NAS
or real WAN. On the same review host, one busy-host run also missed the
Linux-calibrated hash and 10,000-file scan budgets, 114 ms against 60 ms and
1,790 ms against 1,000 ms respectively. That sample identifies a qualification
gap, not a stable regression. The P6 matrix still lacks retained release
evidence for OpenSSH SFTP, a physical spinning disk, and Samba both with and
without `vfs_fruit`.

The retained checklist should automate Samba with and without `vfs_fruit`, NFS
and OpenSSH where practical, and record the physical SFTP and HDD cases for
each release candidate without weakening product budgets from one noisy run.

### F-01: `pairs update` re-asks for everything to change one option

usability — important

What this is: Updating a pair requires re-supplying NAME LEFT RIGHT
MODE positionally even to toggle `--schedule`.

Problem: `crates/foldover-cli/src/main.rs` `pairs update` treats
roots as required positionals; they print as JSON byte arrays in `pairs
show` output, so a one-flag change means copying opaque bytes.

Suggested fix: Add `pairs set ID REVISION [OPTION VALUE]...` that fetches the
pair, applies named option overrides and calls `update_pair`; reuse the
existing option parser.

Acceptance: `pairs set ID R --schedule changes` keeps every other
option; a stale revision is still refused.

### SOL-23: Many failed or unavailable paths multiply per-row work

performance problems, stuttering, and jank — important

Evidence: `run.rs:1574-1576` linearly searches failed ancestors per
operation; `plan.rs:317-331` searches all skip roots per row;
`run/undo.rs:237-278` repeatedly normalizes unavailable and unstable
paths for every journal entry. Endpoint observation filtering has the
same entries-times-unavailable shape in `local.rs`, `sftp.rs` and
`remote.rs`.

Impact: Thousands of unreadable paths in a 100k-file tree can turn
degraded but otherwise safe syncs or Undo into prolonged CPU work.

Suggested fix: Precompute canonical protected-root indexes once per phase.
Test the path's own ancestors against the index, or maintain a minimal
prefix-free root set. A single greatest lexical predecessor is unsafe:
an unrelated sibling can hide an earlier matching ancestor.

Acceptance: Compare indexed decisions against the existing linear
reference on generated trees, including `a`, `a-`, `a/z`, deep paths,
Unicode aliases, unreadable parents and readable siblings. Benchmark
100k entries with thousands of protected roots and cancellation.

Merged SOL26-07: Current source: `crates/foldover-engine/src/plan.rs:320` scans prior skipped roots per row. The quadratic shape is confirmed; no latency was measured. Effort: two hours.

Partial delivery: PR 185 indexes unavailable subtrees during local observation, and d5fa08f reduces protected-state ancestor traversal. Retain this finding for remaining per-row skip, failed-path and Undo normalization work; do not reimplement the shipped local observation index.

### SOL-26: The SFTP chooser ignores a selected enclosing folder

bugs — important

Evidence: `apple/Foldover/Views/PairEditorView.swift:586-593`
accepts a highlighted child only when it is in `folders`, but the
Enclosing Folder row is represented separately by `parent`.

Impact: Highlighting Enclosing Folder and choosing Use This Folder
silently selects the currently browsed folder instead.

Suggested fix: Validate highlighted selection against both parent and child
entries before committing the locator. Keep the visible path and
effective chosen root consistent.

Acceptance: Tests for parent, child, no selection and stale
selection; a UI tour verifies the saved root bytes match the selection.

### SOL-27: Deleted pairs keep their loaded preview state in memory

general issues — important

Evidence: `MainViewModel.removePair` removes configuration but
`SyncStore` keeps its per-pair states, listings, histories and searches.

Impact: Repeated create/review/remove cycles retain potentially
large listings throughout a long-running menu-bar app session. Random
pair IDs make ID reuse an unsuitable basis for claiming a separate bug.

Suggested fix: Prune per-pair data against each authoritative catalog refresh.
Invalidate outstanding read/paging tickets so removed state cannot be
reintroduced by a delayed response.

Mimo addendum: the agent leaks the same way: `foldover-service/
src/sync.rs:169` keeps `jobs`, `skips` and `mutating` entries after
`PAIR_REMOVE`, so `status()` and `items()` keep answering for a
deleted id. Prune both sides. `MainViewModel.changeSelections` and
the `SyncStore` search/decline maps have the same shape.

Acceptance: Remove a pair with loaded pages and an outstanding
request; its state stays forgotten after that request completes.

### MUSE-02: WorkProgress equality is a tautology

general issues — important

Evidence: `apple/Foldover/Stores/SyncStore.swift` `WorkProgress.==
always returns true`, with a comment that equal-looking working states
must not look like new results.

Impact: Harmless under `@Observable` rendering today, but any
future `onChange(of:)` on a state embedding progress will never fire.
The comment documents intent; the type still lies.

Suggested fix: Prefer explicit progress events or a dedicated equality
that ignores counters but compares `waitingFor`/job identity; document
whichever contract stays.

Acceptance: A regression test shows a job-level transition (idle →
working, waiting → copying) is observable and a pure counter tick is
not.

### MIMO-06: Undo can panic on edge-case and corrupt entries

bugs — important

Evidence: `crates/foldover-engine/src/run/undo.rs:243,256,271`
`expect("a name key is a valid path")` after `name_key`, which
lowercases and can grow a path past `SyncPath`'s byte limit (e.g.
`İ`), and on malformed stored rows.

Impact: Undo of a run touching such names panics the job; the
`catch_unwind` keeps the agent alive but the undo fails. Undo's
reporting also counts a folder restored `Beside` as unrestored
(`undo.rs:171`, `205-217`).

Suggested fix: Return the existing soft errors instead of panicking; count
`Placed::Beside` restores as refilling their parent.

Acceptance: Journal entries near the name limit and with bad bytes
produce skipped rows in `not_undone`, never a panic.

Review 2026-11-10: no trigger was found for the panic. `SyncPath` has no
byte limit, journal rows are validated into `SyncPath` when read, and
`name_key` (lower-casing and NFC) cannot produce a separator, a NUL or
a `.` or `..` component. The `Beside` reporting claim was not checked.

### MIMO-08: Per-file fsyncs dominate many-small-file runs

performance problems, stuttering, and jank — important

Evidence: `crates/foldover-fs/src/root/write.rs` syncs the file
and one or two directories per placement and per removal, at both
durability levels (sites listed in the ADR 0019 area).

Impact: A run over N files in one folder costs ~2N+ fsyncs; on
spinning disks and shares this, not copying, is the wall time.

Suggested fix: Batch directory syncs per run or per commit batch under
`Durability::Standard`; keep per-op syncs under `Full`. Respect
ADR 0019's durability contract and staged-write recovery.

Acceptance: Equal safety under injected crashes at batch
boundaries; benchmark 10k small files before and after.

### MIMO-10: Each review choice re-runs the whole preview

performance problems, stuttering, and jank — important

Evidence: `apple/Foldover/Stores/SyncStore.swift` `choose` follows
every Skip Once / Keep A / Keep B / Keep Both with `preview(id)`.

Impact: Ten decisions on a 100k-entry plan cost ten full rescans,
each with only "Resolving…" to look at; reviewing a plan feels slow in
exactly the moment the product promises responsiveness.

Suggested fix: Either batch choices (multi-select, MIMO-27) and preview
once, or refresh incrementally and show the re-scan's counters.

Acceptance: Resolving several conflicts on a large plan performs
at most one preview per batch and shows progress while it runs.

### MIMO-11: Status is polled; the agent's events are unused

performance problems, stuttering, and jank — important

Evidence: The agent publishes `scanProgress`, `opDone`,
`runFinished` and friends (PLAN.md §4.3), but the UniFFI surface is
request/response: `SyncStore` polls working pairs every 500 ms,
`MainViewModel.monitor` reads every pair every 15 s, `MacToMacStore`
polls two RPCs per second while its tab is open, and jobs the agent
starts (scheduled, on-change) update only on the 15 s tick
(`SyncStore.swift:598`), so their progress looks frozen between ticks.

Suggested fix: Either carry the event stream through UniFFI (callback
interface, batched) or make polling adaptive: fast for app-started
jobs, slower for agent-started ones, backoff when idle.

Acceptance: An agent-started sync updates the app at sub-second
cadence while it copies; an idle app polls no pair more than once a
minute.

Partial delivery: PR 195 makes catalog-wide status reads concurrent. Polling transport, idle wakeups and the time before an agent-started job is first noticed remain distinct work. Confirm the current follow-up polling behavior before retaining the historical claim that a working job only updates every 15 seconds.

### SOL2-17: Run history collides at millisecond resolution

general issues — nice-to-have

`crates/foldover-db/src/database.rs:128-145` keys `run_history` by
`(pair_id, started_ms)`, and `database/history.rs:45-79` records rows with
`INSERT OR REPLACE`. Two runs that begin within one millisecond replace one
another. A backward clock step can also sort a new row behind the retained 100
and remove it, weakening the audit trail. PR 216 gives archive and journal run
scopes unique names but does not change this history schema.

Suggested fix: migrate history to a monotonic insertion or unique run
identifier as its key and order, retaining wall time only for display. Test
same-millisecond starts and a backward clock step across the retention limit.

### SOL2-18: Cancelling an unknown pair reports success

general issues — nice-to-have

`foldover sync UUID --cancel` prints success and exits zero for any
syntactically valid but unknown UUID. The responsive job service accepts the
cancel request without an authoritative pair lookup, so automation cannot
distinguish a typo or deleted pair from a stopped job.

Suggested fix: retain or query the authoritative pair-ID set at the job
service boundary and return `PAIR_NOT_FOUND` for unknown IDs. Cover an
existing idle pair, a running pair and an unknown pair in the CLI regression.

### F-02: CLI JSON output prints paths as byte arrays

usability — nice-to-have

What this is: Every item line prints `"path":[104,101,…]`, exact
for any encoding, unreadable for people.

Suggested fix: A `--text` flag rendering UTF-8 paths as strings (arrays stay
for non-UTF-8) across preview items, archive items and history output.
Medium surface; cosmetic gain.

Acceptance: `sync ID --changes --text` prints quoted paths; a
non-UTF-8 name still round-trips through the array form.

### F-03: Export or copy the change list

missing features — nice-to-have

What this is: Preview results cannot leave the app: no Copy for
multi-selection, no CSV/JSON export.

Suggested fix: A "Copy Paths" context command and/or File › Export; small
Swift-side add on the same store boundary.

Acceptance: A filtered selection copies its paths; export writes a
parseable file the UI tour can screenshot.

Mimo addendum: the change table is single-selection today
(`Table(rows, selection: ChangeRow.ID?)` in `PairDetailView.swift`);
multi-select with bulk actions is MIMO-27 and should land with or
before Copy Paths.

Evidence: The table selects one row (`PairDetailView.swift`
`Table(rows, selection: ChangeRow.ID?)`) while its context-menu
closure already takes a set and then acts on `rows.first`; the archive
window selects sets happily. F-03's export assumes a multi-selection
that does not exist yet.

Suggested fix: Set-valued selection; Skip Once / Always Skip / Keep
(conflicts) / Copy Paths over the selection, in one request batch per
action (which also halves MIMO-10's re-preview churn).

Acceptance: Select several rows across kinds and act once; the
inspector reports the selection size; each action remains
review-token-bound.

F-16: a CLI `sync ID --report out.html` could export the same typed preview or result as a static page for an administrator. The current fold workspace selects one change; single-path Edit > Copy exists, but bulk selection and export remain proposals. Preserve review-token binding and batch actions rather than re-previewing once per row.

### F-04: Notify when a run recovers leftovers

novel, delightful, or quirky ideas worth trying — nice-to-have

What this is: `recovered > 0` on a finished run means a previous
run was interrupted; alerts currently cover needsApproval/failed only.

Suggested fix: One coalesced notification for recovered leftovers; opt-in
under the existing permission flow.

Acceptance: An interrupted rerun produces exactly one alert; none
when the app already foregrounds the pair.

### F-06: Linux service packaging

missing features — nice-to-have

What this is: The engine and CLI already run headless on Linux
(verified end-to-end: pair add → preview → approve → sync).

Suggested fix: A `packaging/systemd` user unit for `foldoverd`; GTK4 app
remains the planned larger piece.

Acceptance: `systemctl --user status foldoverd` runs on a clean
Linux host; `foldover status` answers over the default socket.

### F-07: NAS pre-flight capability probing

missing features — nice-to-have

What this is: `pairs check` warns on SFTP roots; the limitations
doc leaves "per-volume capability probing" unimplemented.

Suggested fix: Probe a scratch file, create, statfs, xattr attempt, to
predict xattr/permission/records support before the first sync, and
surface the result in the pair editor.

Acceptance: A capability-weak server reports predicted gaps before
a sync, not as failures during one.

### SOL-12: Qualify the menu bar on physical displays

visual issues and layout problems — nice-to-have

Implemented: The consolidation retains 168's Stop routing and
scaled, scrolling rows, 170's bounded selectable errors, and a list
budget based on the smallest attached display. Default and large-text
UI tests exercise a 44-pair fixture.

Acceptance: Verify short and mixed-height displays, keyboard
access and VoiceOver with the popover on each display.

Suggested fix: run the retained mixed-display, keyboard and VoiceOver acceptance checks before calling the menu bar qualified.

### SOL-13: A conflict inspector that compares both versions

usability — nice-to-have

What this is: Current conflict choices identify Folder A and B but
show little evidence for choosing one.

Problem: `PairInspector.swift:24-38` shows one path and action;
`ChangeRowActions` offers keep-side commands without side-by-side
sizes, dates or content previews.

Suggested fix: Add two compact version cards using actual endpoint names,
size and modification time. Keep Both remains the safe visible choice;
text/image comparison can follow through the same store boundary.

If skipped: Users leave the app to investigate each conflict.

Acceptance: Missing and remote versions are explicit; dates never
imply which version is correct; every choice remains review-token-bound.

Related proposals (MUSE-06, MIMO-25): two-pane local Quick Look and keyboard conflict triage with auto-advance. The fold already supports arrow-key steering, Return confirmation and Space Quick Look; any additional shortcuts must complement that flow. Acceptance: compare actual versions, retain Keep Both and avoid presenting a date as proof that one version is correct.

### SOL-14: Quiet theming with a consistent semantic palette

aesthetics and theming — nice-to-have

What this is: Native controls already inherit the system appearance.

Problem: Warning, failure and activity styling is repeated across
views. Existing light/dark tours do not establish increased-contrast
behavior or a shared visual contract for future components.

Suggested fix: Centralize semantic tints and spacing for touched components.
Support System, Light and Dark only if an in-app preference is useful;
preserve system accent and Increase Contrast. Use restrained warmth in
empty states, not decorative backgrounds behind file tables.

If skipped: Future additions drift in density, color and emphasis.

Acceptance: Screenshot light/dark and increased-contrast states at
minimum widths; status remains understandable without color.

Fusion addendum: per-pair color/tag (Finder-style) in sidebar and
menu bar, a monogram glyph from pair initials, a Reduce-Motion-aware
pulse on the status dot while connecting, `.badgeProminence` on the
numeric conflict badge, and deletion counts in the approval button or
dialog title all fit the same palette work.

Mimo addendum: tint the sync modes themselves (two-way, mirror,
backup) so a screenshot is readable at a glance; adopt the icon's
blue as the default in-app accent (still user-overridable); and keep
one status-symbol vocabulary (spinner, warning, waiting, idle) shared
by sidebar, detail, menu bar and notifications, today each surface
names its own glyphs.

Merged SOL26-20: Current source: `apple/Foldover/Views/FoldMaterials.swift:20`,
PairSidebar and MenuBarView. Preserve the fold palette while checking status
semantics across surfaces. Effort: half a day.

Fifth-pass evidence: status tints are repeated in PairSidebar, MenuBarView,
ArchiveView and other surfaces. `MenuBarArrowAttention.imageset/arrow.svg`
uses `#ff3b30` with original rendering intent, while window attention uses
orange. Consolidate semantic attention/failure/working/connected colors and
verify native light/dark and Increase Contrast rendering before recoloring.

### SOL-15: Make the editor's first three decisions obvious

usability — nice-to-have

What this is: The editor exposes folders, mode, schedule, rules,
metadata and advanced options in one form.

Problem: First-time users must scan specialist controls before
saving; its minimum width is 800 points (`PairEditorView.swift:20-33`).

Suggested fix: Keep Name, Folders and Direction visible. Collapse optional
rules and advanced controls while surfacing active nondefaults and
warnings. Preserve the live plan and explain when saving a scheduled pair can start its first run.

If skipped: Pair creation feels more technical than the core task.

Acceptance: A new user creates a safe local pair in under three
minutes; keyboard navigation and edited nondefaults remain discoverable.

Merged SOL26-16: Current source: `apple/Foldover/Views/PairEditorView.swift:33` sets an 800-point floor. Narrow-display and enlarged-text impact requires native verification. Effort: half a day.

Merged MIMO-15: `MainView.swift:22-24` sets an 820-point window minimum;
the sidebar can request 340, detail 320 and inspector 280 points, totalling
940 before dividers. `SettingsView.swift` fixes a 520-point width and the SFTP
picker uses 500 × 520. These source constraints need a current native
wide-sidebar/minimum-window reproduction; clipping was not established here.
Acceptance: extend LayoutStressTests to that case, retain each pane's floor
and verify enlarged text on a 1280 × 800 display. Let the sidebar yield or
adjust min/ideal constraints if the issue reproduces.

Merged MIMO-16, remaining scope: long plan sentences still use a line limit and tooltip; consider an expansion/selection affordance. Large plans should state truncation and may load pages automatically to an explicit cap. The old missing Sync Now action is superseded by the fold workspace. SOL-04 owns complete preview search.

### SOL-16: A trust summary with honest freshness

usability — nice-to-have

What this is: Sidebar status says Ready, In sync or Needs Attention.

Problem: These labels do not distinguish a recent successful check
from one performed yesterday or show when a held pair will run next.

Suggested fix: Show Last checked, Last successful sync and the next scheduled
opportunity, with explicit Waiting for disk/network/review states.
Never call a disconnected folder current.

If skipped: A calm icon can convey more confidence than the evidence.

Acceptance: Frozen-clock tests cover stale, paused, offline and
attention states; relative dates update without frequent global redraws.
A Dock badge can expose pending review while the main window is closed.

Fusion addendum: the menu-bar variant is F-05; a "time travel"
one-liner ("Everything in sync · last checked 10:42 · next ~10:52")
and a sync forecast ("≈ 3.2 GB to copy, about 4 min at recent
speeds", using the rate plumbing shipped in PR 164) are the same data
shown earlier and cheaper.

What this is: The extra shows "In sync" style text but not *when* —
Time Machine's key glanceable. History is already fetched.

Suggested fix: "Synced 12 min ago" under the pair name; complements the
deeper trust summary in SOL-16.

Acceptance: Fixture pairs render relative times; no extra polling
beyond the existing history refresh.

`apple/Foldover/Views/PairSidebar.swift:104-110` and `:134-154`. The sidebar's
three-tier freshness tint, primary within an hour, secondary within a day, soft
orange beyond, is the quietest, most useful trust signal in the app, and it
exists nowhere else. The menu bar's pair rows and the inspector's "Recent Syncs"
both show the same age as flat grey text.

Suggested fix: reuse the same tier function for the menu bar's pair status and
the inspector's history rows, so "how stale is this?" reads the same way in all
three surfaces.

MIMO-25 also proposed an optional "in step for 12 days" history streak. Keep the distinction between last checked and last successful sync; a streak must never hide a failure or disconnected folder.

### SOL-17: A named safety rehearsal for a first pair

novel, delightful, or quirky ideas worth trying — nice-to-have

What this is: The product offers preview, removal protection and optional required approval; automatic runs are permitted by the owner decision in PLAN.md §3.6.

Problem: People still need to understand what archive and Undo
actually recover before trusting them.

Suggested fix: Offer an opt-in rehearsal with disposable sample folders:
create, edit, delete, preview, sync and restore one sample. Label it
clearly and never use a user's selected roots.

If skipped: Recovery remains something first learned under stress.

Acceptance: No real pair configuration is changed; all samples are
removable and the rehearsal can be skipped permanently.

Fusion addendum: a `foldover sim` CLI variant, run a pair in a
scratch dir, is nearly free with `--socket` +
`StateLocation::Directory` (both already work) and demonstrates the
product without touching real data.

MUSE-06: an optional first-sync receipt ("First sync done, N files now protected") can explain the rehearsal outcome without changing a real pair.

### SOL-18: Give versions an expiration receipt

missing features — nice-to-have

What this is: Archives have a fixed 30-day retention policy.

Problem: The browser shows a run's time but not when its recoverable
versions expire.

Suggested fix: Show a compact expiration date and explain that versions live
on that endpoint. Offer a non-destructive Export Copy action before
expiry; do not change retention implicitly.

If skipped: A user can postpone recovery past the pruning date.

Acceptance: Expiry matches the engine's cutoff; exporting preserves
archived originals, metadata and occupied destination files.
Add read-only Quick Look and Reveal for local archive items first, so
users can inspect a version before restoring it.

Fusion addendum: a symlink or `.fileprovider` view of
`.foldover/archive` would give power users Finder-native recovery
without opening Foldover.

`apple/Foldover/Views/ArchiveView.swift:165-170`. The only command is "Put Back".
A user recovering a file cannot preview it, confirm it is the version they want,
or see where it came from. For a local side the archived copy is reachable at
`.foldover/archive/<run>/<path>`, so Quick Look and "Show in Finder" are both
implementable without new agent support.

Suggested fix: offer Quick Look and "Show in Finder" for rows on a local side,
built from the pair's root plus the archive path. For remote archives, provide
an explicit downloaded preview rather than implying local access.

`apple/Foldover/ViewModels/PairSyncViewModel.swift:621-624`. A finished sync
already tells the user the archive folder name. Making that note a link that
opens `.foldover/archive/<run>` in Finder closes the loop between "it archived 48
items" and "here they are" in one click, and needs no new agent support for a
local side.

Verdict for this pass: the blocking and important findings are all in open pull
requests and should be merged after their native checks. What remains here is
polish, and the largest single item is the archive prefix cursor behind the
search in pull request 196, which would let a search find a name without reading
the pages before it.

### SOL-20: Explain a change and let users test exclusions

usability — nice-to-have

What this is: A preview names the operation; skip rules are authored
as gitignore-style text.

Problem: A surprising change does not explain which observation or
rule caused it. Testing a new exclusion currently means another preview.

Suggested fix: Add Why this change? from reconciliation evidence, and a
read-only rule tester with representative matching/nonmatching paths.
Explain deletion versus missing/unreadable paths in plain language.

If skipped: Users troubleshoot by trial and error around potentially
destructive operations.

Acceptance: Explanations come from typed engine facts, not UI
guesses; rule testing uses exactly the engine's matcher and writes nothing.

Merged SOL26-22: Current source: `apple/Foldover/Views/PairEditorView.swift:258` accepts rules without a sample-path result. A Try a Path control should use the engine matcher. Effort: one day.

### F-15: Quiet hours for automatic syncs

novel, delightful, or quirky ideas worth trying — nice-to-have

The second pass proposed a schedule rule such as "never sync between 22:00 and 07:00" for battery use and NAS noise. This remains an optional extension of the existing Schedule/Environment machinery.

Suggested fix: offer explicit quiet hours without changing existing pair schedules.

### F-18: Measure larger content and resume hash buffers

performance problems, stuttering, and jank — nice-to-have

Historical evidence: `crates/foldover-fs/src/root/content.rs` uses a 64 KiB `HASH_BUFFER_BYTES`; Mimo also identified resume-prefix hashing through `io::copy`'s smaller buffer in `root/write/partial.rs:83`. Larger buffers may reduce system calls on huge files; the original review expected this path to remain I/O-bound.

Suggested fix: benchmark 256 KiB to 1 MiB buffers and reuse the content-hash buffer policy for resume prefixes only if measurements justify it.

### MIMO-07a: Guard preview count subtraction

bugs — nice-to-have

`apple/Foldover/ViewModels/PairSyncViewModel.swift:656-660` subtracts the number of returned changes or failures from unsigned totals. The scan-error count already has a zero floor; the remaining subtractions rely on internally consistent agent data.

Suggested fix: floor inconsistent totals at zero or use checked subtraction. Acceptance: malformed totals do not trap and valid hidden-row counts remain unchanged.

### MIMO-07c: Mixed outcomes notify only conflicts

usability — nice-to-have

Historical evidence: `apple/Foldover/Stores/SyncStore.swift` selects a conflict alert before reporting failures from the same finished run. The user can miss the failure count when both outcomes occur.

Suggested fix: include both conflicts and failures in the existing actionable notification. Acceptance: one mixed-result alert retains both counts.

### MIMO-07d: Sync while approval is pending re-prompts

usability — nice-to-have

Historical evidence: invoking Sync on a needs-approval result can run with zero approvals and return to the confirmation dialog. The exact current flow requires a native reproduction.

Suggested fix: route the command to the existing captured approval request, or disable it while that request is pending. Preserve exact review-token binding.

### MIMO-12b: A wedged IPC handler holds a connection permit

general issues — nice-to-have

`crates/foldover-ipc/src/local.rs:180-235` holds one of 32 connection permits across handler execution; non-permission accept errors end the serve loop. Historical exception-path hardening, not an observed outage.

Suggested fix: bound or cancel handler work appropriately and retry transient accept failures. Acceptance: a stalled handler and transient accept error do not permanently exhaust service.

### MIMO-12d: Long RPC work has no job handle

general issues — nice-to-have

`archive.putBackAll`, endpoint browsing and pairing are single RPCs. PR 166 increased their deadlines, but progress and cancellation still require job handles. Related: SOL26-11 covers whole-run restore specifically.

Suggested fix: use start/poll/cancel jobs when those surfaces next need progress, retaining the current slow-request budgets until then.

### MIMO-13b: Qualify additional local state filesystems

general issues — nice-to-have

Historical evidence: the Linux state allowlist in `crates/foldover-db/src/database.rs` excludes ZFS (0x2fc12fc1), F2FS (0xf2f52010) and bcachefs (0xca451a4e), potentially refusing legitimate local state storage.

Suggested fix: validate locking and durability guarantees on each filesystem before extending the allowlist; retain refusal of remote state locations.

### MIMO-13c: Unsafe crash-record permissions block startup

general issues — nice-to-have

`crates/foldover-db/src/database/starts.rs:73` skips malformed crash-history lines but propagates a failure from `private_file` for wrong ownership or mode.

Suggested fix: decide whether such a crash record can be treated as unreadable without weakening private-state validation. Acceptance: document and test safe startup behavior for malformed content and unsafe permissions.

### MIMO-13d: Keep-copy labels rely on trusted callers

general issues — nice-to-have

Historical evidence: local and SFTP conflict-copy naming interpolate a `from` label into a sibling name; `remote/serve.rs` applies `plain_label`, while current internal callers pass constants.

Suggested fix: validate labels at the common naming boundary if untrusted callers are added. Keep this classified as hardening, not a demonstrated path escape.

### MIMO-14b: Typed numeric limits differ from stepper bounds

usability — nice-to-have

`PairEditorView.swift` gives NumberField a declared range, but typed input bypasses the Stepper. Size and bandwidth setters clamp to safe arithmetic limits rather than the form's smaller upper bounds; the age setter already clamps to its declared maximum.

Suggested fix: apply the displayed size and bandwidth bounds consistently on commit, without changing stored values silently when the control is inactive.

### MIMO-17: The app icon predates the platform's icon style

visual issues and layout problems — nice-to-have

Evidence: `media-sources/icon.png` and `AppIcon.appiconset` ship a
full-bleed dark-navy square with no dark or tinted appearances; at
16 px the folded dart reads as a blob.

Suggested fix: Regenerate in the layered icon template with light, dark and
tinted variants; check legibility at 16 px and on light docks.

Acceptance: Screenshot the Dock in light, dark and tinted modes at
every icon size.

`apple/Foldover/Assets.xcassets/AppIcon.appiconset/Contents.json`, see MIMO-17.
Ten PNGs from 16 px to 1024 px and no `.icon`/Icon Composer asset, so the icon
cannot take the platform's current glass and shadow treatment and scales less
crisply than a vector one. The menu-bar arrows are vector SVG templates, so the
inconsistency is visible in the same asset catalog.

Suggested fix: author the icon as an Icon Composer `.icon` asset, retain asset
license/source provenance and keep the rasters only as a fallback.

### MIMO-18a: Scope Sync All while editing a pair

usability — nice-to-have

`App/Commands.swift` invokes the shared view model for Sync All while selected-pair commands follow the focused main-window value. `MainViewModel.canSyncAll` checks runnable pairs, not whether a pair editor is open.

Suggested fix: decide and test whether global Sync All should remain available during configuration; if it should withdraw, use the existing editor/focused-command policy consistently.

### MIMO-18b: Qualify the Help menu search

usability — nice-to-have

`App/Commands.swift:29` replaces the Help command group. The historical review claimed this removes standard menu-item search, but that native behavior has not been reproduced in this consolidation.

Suggested fix: verify the standard Help search on macOS; append the guide command only if replacement actually removes expected system behavior.

### MIMO-19: Destructive dialogs should name the folder

usability — nice-to-have

The removal dialog does this well ("Remove settings for X?"). "Archive
and Sync", "Put Back All?" and the approval message should name the
pair and its folders, so a reflex tap cannot hit the wrong pair.

Suggested fix: name the pair and actual endpoint folders in approval and whole-run restore dialogs, while preserving captured review context.

### SOL2-34: A remote-folder error can follow selection to another Mac

usability — nice-to-have

`apple/Foldover/Stores/MacToMacStore.swift:223-236` stores the result of
`offered(by:)` in one global error. The remote-folder picker cancels its view
task when the selected Mac changes, but a delayed failure from Mac A can still
publish after a successful request for Mac B. The picker then shows an error
for the wrong device.

Suggested fix: return a request-local folder result or generation-key errors
by device, and discard completion from any request that is no longer the
picker's current Mac. Test a delayed A failure followed by a successful B
selection.

### MIMO-20: A Status intent for Shortcuts

missing features — nice-to-have

`Intents/PairIntents.swift` offers Run, Pause and Preview. An intent
returning the pair's status line ("In sync", "3 conflicts need you")
completes the set and makes automations and Siri genuinely useful.

Suggested fix: add a read-only Status intent using the existing pair-state presentation boundary.

### MIMO-22: Optional notification when a long sync finishes

novel, delightful, or quirky ideas worth trying — nice-to-have

Notifications cover failures and conflicts only, deliberately
(PLAN.md §3.8). An opt-in "tell me when a sync that ran longer than N
minutes finishes" is the one missing alert class in sync tools
(related F-04 and SOL-09 for the notification plumbing).

Suggested fix: offer an opt-in long-run completion alert through the existing notification boundary, without changing silent-success defaults.

### MIMO-23: A run detail sheet

missing features — nice-to-have

The state keeps `run_item` (ADR 0026) and the inspector shows ten
one-line summaries; nothing shows what a past run actually changed.
A read-only change table per run, reusing the preview table's row
model, closes both this and "Older syncs are not listed." (related
SOL-08's run cursor and F-03's export).

Merged SOL26-15: Current source: `apple/Foldover/Views/PairInspector.swift:60-75` shows recent summaries. Searchable run details and local export remain unimplemented. Effort: one to two days.

Retain saved run-item evidence (ADR 0026), searchable outcomes and local export. SOL-08 separately owns complete archive-run paging; a history-detail view alone does not make older retained archive runs reachable.

Suggested fix: expose read-only saved run-item details in a searchable view
with local export over existing history data. Keep archive-run paging in
SOL-08 as a separate acceptance boundary.

### MIMO-24: An opt-in diagnostics bundle

missing features — nice-to-have

PLAN.md §9 promises "an opt-in diagnostics bundle helps support";
nothing implements it. A "Copy Status" gathering the agent version,
pair states, last errors and recent history into the pasteboard is the
small version.

Suggested fix: start with an explicit Copy Status action for versions, pair states, last errors and recent history; let the user inspect diagnostic data before sharing.

### SOL2-27: Define a measurable crash-free release gate

missing features — nice-to-have

`PLAN.md:731,768-770,796-799` requires more than 99.5 percent crash-free
sessions before 1.0, while the no-telemetry design defines neither a session
denominator nor a local crash count. Maintainers therefore cannot establish
that the release gate passed. The opt-in diagnostics bundle above can carry
evidence but does not by itself define or collect the metric.

Suggested fix: adopt a privacy-preserving, opt-in local session and crash
counter that users can inspect through diagnostics, or record an owner decision
that replaces the percentage with a measurable no-telemetry criterion. Define
the observation window and denominator before the beta is used as evidence.

### MUSE-05a: Allow inspector width adjustment

usability — nice-to-have

`MainView.swift` fixes the inspector at 280 points. Long paths and conflict details cannot request more width.

Suggested fix: allow a draggable or ideal width without reintroducing the native .inspector layout-loop crash; retain LayoutStressTests.

### MUSE-05b: Explain the empty detail while loading

usability — nice-to-have

`MainView.emptyDetail` uses Color.clear beside the sidebar spinner, leaving the content pane blank while the catalog loads.

Suggested fix: offer a compact loading explanation or progress view without adding layout feedback loops.

### MUSE-05c: Give the empty inspector an informative symbol

usability — nice-to-have

`PairInspector.swift` uses the sidebar.trailing toggle glyph for its no-selection placeholder.

Suggested fix: consider info.circle or slider.horizontal.3 when touching the placeholder; keep its accessible title.

### MUSE-05e: Qualify the setup badge under Increase Contrast

usability — nice-to-have

`SetupView` uses white text on the accent for its current-step badge. Contrast on user-selected accents has not been measured.

Suggested fix: check Increase Contrast and light/dark screenshots before changing the badge treatment.

### MUSE-05h: Offer focused CLI subcommand help

usability — nice-to-have

`crates/foldover-cli/src/main.rs` keeps one large USAGE string; subcommand --help works but prints unrelated command groups.

Suggested fix: show focused subcommand usage and retain literal help-looking paths or values. Acceptance: relevant flags stay discoverable without changing existing parsing.

### MUSE-05g: Show archive-run counts and bytes

missing features — nice-to-have

`apple/Foldover/ViewModels/ArchiveViewModel.swift:37-41`,
`apple/Foldover/Views/ArchiveView.swift:85-99`. The run list shows only a title.
A pair with thirty runs forces the user to open them one at a time to find the
sync that archived the file they want.

Suggested fix: show item count and total bytes per run. `ArchiveRun`
(`crates/foldover-proto/src/local/archive.rs:24-31`) needs two more fields, which
the agent can sum while it lists the run's folder.

### MUSE-06b: Offer a compact Undo completion toast

novel, delightful, or quirky ideas worth trying — nice-to-have

The window already registers Edit > Undo for its latest reversible sync; a temporary "Synced · Undo" receipt could teach that command.

Suggested fix: reuse the window model and existing Undo contract; avoid a second success surface when the current completion already offers the action.

### MUSE-06f: Watch a preview from the CLI

novel, delightful, or quirky ideas worth trying — nice-to-have

The fourth pass proposed `foldover watch ID`: periodically re-preview and show one-line diff counts over the existing CLI progress flow.

Suggested fix: make it an explicit read-only watch mode with cancellation and a bounded cadence.

### MUSE-06g: Offer named pair templates

novel, delightful, or quirky ideas worth trying — nice-to-have

Optional editor templates such as "Photos to NAS" could prefill mode, schedule and filters.

Suggested fix: preview the template's complete settings before saving and preserve explicit folder and safety choices.

### MUSE-06h: Filter pairs needing attention

novel, delightful, or quirky ideas worth trying — nice-to-have

The fourth pass proposed clicking an attention count to narrow the sidebar to pairs needing review.

Suggested fix: reuse current pair activity state and make clearing the filter obvious.

### MIMO-25b: Show local completion thumbnails

novel, delightful, or quirky ideas worth trying — nice-to-have

Optional thumbnails of a few copied images could add context to the existing completion imprint.

Suggested fix: limit thumbnails to a small local selection and avoid extra remote reads or idle work.

### MIMO-25c: Explain a dropped file with a dry run

novel, delightful, or quirky ideas worth trying — nice-to-have

The third pass proposed dropping one file on the detail to ask what the current pair would do with it.

Suggested fix: reuse engine reconciliation evidence for a read-only explanation; do not create a new pair or write the file.

### MIMO-25d: Offer a fuzzy pair switcher

novel, delightful, or quirky ideas worth trying — nice-to-have

A keyboard Open Quickly surface could find pairs first and archived runs later.

Suggested fix: reuse the catalog and retain ordinary sidebar navigation.

### MIMO-25e: Offer a pair-status desktop widget

novel, delightful, or quirky ideas worth trying — nice-to-have

A WidgetKit extension could show each pair's one-line status; this is a medium-sized optional platform surface.

Suggested fix: read cached agent status through the existing application boundary and state freshness explicitly.

### MIMO-25i: Show a copy-speed sparkline

novel, delightful, or quirky ideas worth trying — nice-to-have

The existing agent-owned copy-rate samples could drive a small inspector sparkline while a run copies.

Suggested fix: retain honest cumulative-rate semantics and distinguish stalls from missing samples.

### MIMO-25j: Use relative history dates with absolute detail

novel, delightful, or quirky ideas worth trying — nice-to-have

Recent history currently uses absolute dates; "2 min ago" with an absolute tooltip was proposed for quicker reading.

Suggested fix: update relative text at a bounded cadence and preserve exact timestamps for diagnostics.

### STEP-completion: Settle the menu-bar icon once on completion

novel, delightful, or quirky ideas worth trying — nice-to-have

`apple/Foldover/ViewModels/MenuBarAnimation.swift:41-64`. The working animation
already cycles four fold-arrow frames at 400 ms and respects Reduce Motion by not
running at all. A single two-frame "settle" when a run finishes would give the
menu bar the same completion beat the main window's completion card has, without
adding a permanent animation. Reuse `MotionStore.preference` so Reduce Motion
users get a static swap.

Suggested fix: reuse MotionStore and the existing frames for one completion transition; Reduce Motion gets a static swap and no idle timer.

### STEP-timeline: Show an archive-run timeline

novel, delightful, or quirky ideas worth trying — nice-to-have

`apple/Foldover/Views/ArchiveView.swift:85-99`. The run list is a plain list of
titles. Because every run already has a start time and, with the size fields
above, a byte total, a compact horizontal bar per run would turn the archive into
a readable history of how much the pair moved and when, the same story the
inspector's "Recent Syncs" tells in words.

Suggested fix: add a compact timeline only after run counts/bytes exist; retain accessible textual dates and counts.

### STEP-summary: Copy the live pair summary

novel, delightful, or quirky ideas worth trying — nice-to-have

`apple/Foldover/ViewModels/PairEditorViewModel.swift:332-339`,
`apple/Foldover/Views/PairEditorView.swift:363-369`. The editor rewrites
"Changes in either folder go to the other; a file deleted in one folder is
archived in the other. Syncs every 15 minutes." as settings change, which is the
clearest explanation of a sync pair anywhere in the product. Offering "Copy
Summary" on that sentence, and showing the same sentence in the completion
notification, would let a user paste an accurate description into a message to
whoever else uses the folders.

Suggested fix: offer Copy Summary over the existing plan text without changing its safety or schedule meaning.

### MIMO-26a: Cache pair ordering when catalog changes

performance problems, stuttering, and jank — nice-to-have

`MainViewModel.pairs` sorts with localizedStandardCompare on every access, several times per render.

Suggested fix: measure catalog-heavy rendering, then cache the sorted order by catalog revision if material.

### MIMO-26b: Avoid peer-frame front removal and payload copies

performance problems, stuttering, and jank — nice-to-have

`foldover-endpoint/src/link/stream.rs` removes a frame's first byte with remove(0) and copies outbound payloads when prepending a tag.

Suggested fix: measure large-stream allocation/copy cost and use slices or split buffers if the framing driver supports them.

### MIMO-26c: Batch SFTP restore-name availability checks

performance problems, stuttering, and jank — nice-to-have

Historical evidence: SFTP keep/restore sibling selection probes attributes one candidate at a time, up to 100 remote round trips. PR 208 improves observation and archive traversal but does not establish this naming path is batched.

Suggested fix: reuse a parent listing for candidate selection while still rechecking placement atomically or handling collisions.

### MIMO-26d: Avoid catalog-wide status work on icon animation ticks

performance problems, stuttering, and jank — nice-to-have

`apple/Foldover/App/MenuBarScene.swift` reads menuBarStatus while the working icon animates every 400 ms; that status walks the catalog.

Suggested fix: profile large catalogs and cache status only on its observable inputs if the tick work is material.

### MIMO-26e: Avoid duplicate history refreshes

performance problems, stuttering, and jank — nice-to-have

Historical evidence: SyncStore fetches history when a job ends and again when a changed-state check notices the same result.

Suggested fix: coalesce equivalent refreshes without suppressing history from unattended jobs.

### MIMO-26f: Avoid unnecessary equal-state publications

performance problems, stuttering, and jank — nice-to-have

Historical evidence: SyncStore assigns observed states during polling, which may invalidate consumers even when the meaningful state is unchanged.

Suggested fix: measure invalidation before adding equality suppression; preserve progress-counter updates despite WorkProgress's special equality contract.

### MIMO-26g: Keep peer-watch coalescing from occupying a worker

performance problems, stuttering, and jank — nice-to-have

`foldover-endpoint/src/remote/serve.rs` sleeps for CHANGE_QUIET after a watch burst before draining notifications.

Suggested fix: measure worker pressure from many watches and move coalescing to a nonblocking wait only if justified.

### MIMO-26h: Stream SFTP listing work sooner

performance problems, stuttering, and jank — nice-to-have

Historical evidence: SFTP observation buffers a tree listing before processing entries. PR 208 parallelizes content observations; first-result latency and peak listing memory still need measurement.

Suggested fix: profile large server trees and process bounded listing batches if latency or memory remains material.

### `AgentCapability` has three variants but the agent only ever emits `Sync`

general issues — nice-to-have

`crates/foldover-proto/src/local.rs:91-105`,
`crates/foldover-service/src/lib.rs:612-617`, `crates/foldover-ffi/src/lib.rs:116-122`,
`crates/foldover-cli/src/main.rs:377-384`. `health()` hardcodes
`AgentCapability::Sync` on every path, including crash-loop safe mode, where
`safe_mode` carries the real state instead. The FFI and the CLI each carry full
handling for `HealthOnly` and `PairConfiguration`, and the CLI's own test only
exercises `Sync`. A client asking "can this agent sync?" reads a field that
always answers yes, and two branches that look like supported states can never
be reached.

Suggested fix: pick one. Either emit `PairConfiguration`/`HealthOnly` where the
agent genuinely cannot sync and let clients branch on it, or delete the unused
variants and keep `safe_mode` as the single signal.

### `ChangeRowsCache` has no builder tag in its key

general issues — nice-to-have

`apple/Foldover/ViewModels/PairSyncViewModel.swift:104-131`. The cache is keyed
by pair plus `store.rowsRevision`. Its own comment warns that every builder
sharing the cache must be distinguishable by those fields, but nothing enforces
it, and only one builder exists today. A second builder over the same source —
bulk-selection rows, a filtered copy for export, would silently receive the
first builder's rows.

Suggested fix: add a builder identity to `RowsSource` so a collision cannot
compile.

Qualification: only one builder exists today; add identity only when a second builder actually shares the cache.

### `InfoButton`'s popover has no height limit

visual issues and layout problems — nice-to-have

`apple/Foldover/Views/InfoButton.swift:20-36`. The popover is `frame(width: 360)`
with `fixedSize(horizontal: false, vertical: true)`, so its height grows with the
note. The "Skip" and "Folders" notes run past 500 characters; at an accessibility
text size the popover can grow taller than the display and lose its content
off-screen.

Suggested fix: wrap the message in a `ScrollView` with `.frame(maxHeight: 360)`.

### STEP-pair-roots: Warn about identical or nested chosen folders

usability — nice-to-have

`apple/Foldover/ViewModels/PairEditorViewModel.swift:80` and `:130-143`. The
agent rejects identical and nested roots
(`crates/foldover-types/src/pair.rs:98`,
`crates/foldover-engine/src/configuration.rs:641`), but the editor has no local
check, so picking the same folder twice shows no warning and the failure only
appears after clicking Save Pair.

Suggested fix: add a `foldersWarning` that names the problem and include it in
`canSave`, mirroring `dailyWarning`.

### STEP-settings-poll: Qualify polling lifetime in Settings

performance problems, stuttering, and jank — nice-to-have

`apple/Foldover/Views/MacToMacView.swift:102`,
`apple/Foldover/Stores/MacToMacStore.swift:162-189`. `MacToMacView`'s `.task`
starts a 1 Hz loop that issues two IPC round trips per pass. The task belongs to
the Settings window, so the General tab keeps polling Mac-to-Mac state and
shared folders for as long as the window is open, two agent round trips and a
timer wakeup per second on a window that is usually idle.

Suggested fix: monitor only while the Mac-to-Mac tab is selected, and stretch
the interval to 2–3 s; pairing codes are not sub-second events.

Qualification: the task is attached to MacToMacView. Verify SwiftUI tab task lifetime natively before claiming the unselected General tab keeps it alive; the one-second polling cadence itself is source-confirmed.

### STEP-sync-all: Show aggregate progress for Sync All

usability — nice-to-have

`apple/Foldover/ViewModels/MainViewModel.swift`, `apple/Foldover/App/Commands.swift:160-162`,
`apple/Foldover/Views/MenuBarView.swift:34-36`. The command starts runnable pairs concurrently, but offers no aggregate outcome. Nothing says how many pairs
are running, how many are waiting, or when the last one finished, so a user who
clicks Sync All and switches away has no way to tell whether it is still working.

Suggested fix: carry a small aggregate state on `MainViewModel` ("Syncing 2 of
5") and show it beside the menu bar's status line. This falls out of the
concurrency fix in pull request 194.

### SOL26-18: Disclose keyboard steering and Quick Look

usability — nice-to-have

File and line: `apple/Foldover/Views/FoldSheet.swift:174-185,292`

Impact: Arrow/Return steering, Space Quick Look and Delete skipping exist, but the main help emphasizes folding and Command-S. New keyboard users must discover them.

Suggested fix: Add a compact shortcut legend or contextual help.

Effort: two hours

Historical fifth-pass detail: the Pair menu Quick Look command has no explicit Space shortcut. A contextual legend should expose Space without taking ordinary text-entry keys.

### STEP-setup: Make the guide's third step informational or completable

usability — nice-to-have

`apple/Foldover/ViewModels/MainViewModel.swift:667-672` and `:644`. Step 3 is
hardcoded `.upcoming`, and `showsSetup` requires `pairs.isEmpty`, so the guide
disappears the moment a pair exists. The row reads as unfinished work that was
never finished.

Suggested fix: either drop the third step, or mark it informational with no
progress badge.

### STEP-archive-selection: Avoid repeated archive selection lookups

performance problems, stuttering, and jank — nice-to-have

File and line: `apple/Foldover/ViewModels/ArchiveViewModel.swift:139-141`

Impact: Mapping selected rows back to items costs O(selection × loaded items) on the main actor. Affects large recovery selections; no frame-time measurement. Selecting a full 1,000-item page performs about one million path comparisons.

Suggested fix: Build one dictionary or selected-ID set per operation.

Effort: one hour

Fifth-pass evidence: ArchiveViewModel.putBack uses one linear store.items.first lookup per selected row. This is O(selection × loaded items); a native pause has not been measured, so retain it as a profiling/implementation proposal.

### SOL26-10: Held reviews retain two item representations

performance problems, stuttering, and jank — nice-to-have

File and line: `crates/foldover-service/src/sync.rs:89`

Impact: Wire items and a cloned full preview retain duplicate paths/actions for large reviewed plans. Heap impact is not measured.

Suggested fix: Measure retained heap and share one representation if material.

Effort: half a day

### SOL26-11: Whole-run recovery has no progress or cancellation

missing features — nice-to-have

File and line: `crates/foldover-engine/src/archive.rs:141`

Impact: Large restores reserve the pair until all item operations finish, without user-visible counters or cancellation. PR 211 bounds listing memory to 1,000-item pages, but restore remains serial inside one synchronous request and its failure report can still grow without a bound.

Suggested fix: Expose restore-all as a cancellable job with counters and bounded failure detail.

Effort: one day

### SOL26-17: Faint waiting text ignores Increase Contrast

visual issues and layout problems — nice-to-have

File and line: `apple/Foldover/Views/FoldLineView.swift:140,208`

Impact: Waiting names use opacity 0.6 while Increase Contrast handling only adjusts arriving text. Low-vision users receive inconsistent emphasis. Contrast has not been measured.

Suggested fix: Apply the contrast policy to waiting and old-name text; check light/dark screenshots.

Effort: two hours

### SOL2-37: An unavailable fold remains enabled to VoiceOver

usability — nice-to-have

At `apple/Foldover/Views/FoldSheet.swift:821-831`, a fold that cannot run is
only dimmed. VoiceOver still announces an enabled Sync action and can invoke
it, after which the guard silently does nothing. This gives keyboard and
assistive-technology users a different availability contract from the visual
state.

Suggested fix: expose the disabled accessibility state whenever `canFold` is
false and omit the action in that state, while retaining the explanatory hint.
Add an accessibility regression for both available and unavailable folds.

### SOL26-19: Onboarding copy obscures automatic first runs

usability — nice-to-have

File and line: `apple/Foldover/ViewModels/MainViewModel.swift:694; apple/Foldover/Views/PairEditorView.swift:53`

Impact: The guide says nothing changes until you sync, while the editor explains schedules can start after saving. First-time users get inconsistent guidance.

Suggested fix: Explain manual and automatic starts together and point to required approval.

Effort: one hour

### SOL26-21: Copy a local sync receipt

novel, delightful, or quirky ideas worth trying — nice-to-have

File and line: `apple/Foldover/Views/FoldParts.swift:89`

Impact: The completion imprint already has outcome counts. A plain-text receipt would help users share or retain a troubleshooting record without telemetry.

Suggested fix: Offer Copy Receipt with pair name, counts, duration and failures; omit paths by default.

Effort: half a day
