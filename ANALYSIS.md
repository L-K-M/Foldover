# Foldover improvement backlog

What this repo does: Foldover previews and syncs two folders through a
single Rust agent, with a native macOS interface. Backup is additive;
Mirror also removes target-only content; Sync reconciles both sides.
Archives provide short-term recovery on each endpoint.

Reviewed 2026-10-23, starting at `8fb2405`. Assumed load: one person,
several overlapping jobs on disjoint roots, 100k to 1M tracked files,
and occasional multi-gigabyte files over slow or interrupted links.
The initial review was written in `Sol.md` before implementation; a
second independent pass by SWE-2 Max was written in `Fusion.md`; a
third independent pass is merged below from `Mimo.md` (SWE-2 Max
scans with the reviewer's own verification, ids `MIMO-`). A fourth
pass (`Muse.md`, ids `MUSE-`) added engine/UI findings and five
small UI PRs; its unique material is merged in place below (ids
`MUSE-`). Source references describe that baseline unless a PR status
says otherwise. The consolidation table records retained and superseded
work; the remaining entries describe unresolved work and qualification.

## Start here

Preserve the single-writer engine, review-token approvals, endpoint-local
archives and existing layer boundaries. Reproduce a reported bug before
fixing it. Keep visual proposals separate from measured defects.

| Priority | Work | Acceptance boundary |
|---|---|---|
| P1 | SOL-21: cancel stalled peer streams | Stalled peer, slow healthy peer and idle watch |
| P1 | MIMO-01–04: correctness the sync depends on | Missed FSEvents changes, stale-plan skips, colliding run names, newline skip rules |
| P1 | MUSE-01: atomic lease renewal | Two processes: renewer loses cleanly after a takeover |
| P1 | SOL-26: honor the selected SFTP parent | The saved root equals the highlighted folder |
| P1 | SOL-03: reject stale archive replies | Delayed success, error and cleanup cannot change a newer listing |
| P1 | SOL-04: search the complete preview | Find late matches without downloading earlier pages |
| P2 | SOL-23: index protected ancestors | Same safety decisions, bounded work with many failures |
| P2 | SOL-24 and SOL-08: complete, bounded archive browsing | Old runs and late items remain reachable without full rescans |
| P2 | SOL-05 and SOL-06: visible progress and responsive previews | Large transfers advance; search avoids repeated formatting |
| P2 | MUSE-02–04: progress equality, CLI default verb, archive prune kinds | Tautological Equatable is explicit; bare `sync` is safe by default; only directories prune |
| P2 | MIMO-08–11: sync speed and liveness | Batched fsyncs, SFTP scans that skip unchanged content, choices without full re-previews, pushed status |
| P2 | SOL-09, SOL-10 and SOL-16: visible, actionable attention | Background failures, restore causes and freshness are explicit |
| P2 | F-01: `pairs set` partial-update convenience | One option toggles without re-supplying pair identity |
| P3 | SOL-13–15 and SOL-17–20: product and visual polish | Validate with native light/dark, keyboard and accessibility checks |
| P3 | MUSE-05: small UI edges (inspector width, empty states, focused subcommand usage still open) | Each edge fixed where it sits; no layout regressions in LayoutStressTests |
| P3 | MIMO-15–27: interface additions and hardening | See entries; MIMO-12/13 are exception-path hardening |
| P3 | F-02–F-07: CLI readability, exports and small notifications | See entries |

## PR consolidation decisions (24 October)

Reviewed all 26 open PRs against `9b9eeab`, including connected code,
tests, CI failures and review threads. The consolidation branch is
`review/consolidate-open-prs`; its combined checks qualify the result.
Qualification covers Linux `cargo xtask ci`, dependency and shell checks,
tracked Markdown/Mermaid checks, the native Xcode 27 build/test and UI tour.
Revision-specific results are recorded on
[PR 181](https://github.com/L-K-M/Foldover/pull/181).
The original four passes above remain the source of the unresolved backlog.

| PR | Decision | Retained work and corrections |
|---|---|---|
| [140](https://github.com/L-K-M/Foldover/pull/140) | Close as shipped | `git cherry` verifies all seven patches already exist in main under different commit IDs |
| [146](https://github.com/L-K-M/Foldover/pull/146) | Merge | Action release SHA independently verified; six immutable pins |
| [147](https://github.com/L-K-M/Foldover/pull/147) | Merge | libc patch release and registry checksum verified |
| [157](https://github.com/L-K-M/Foldover/pull/157) | Combine and fix | Help exits 0; literal help-looking names, paths and values remain valid |
| [158](https://github.com/L-K-M/Foldover/pull/158) | Combine and fix | Shared polling preserves progress through plain runs and preview listings |
| [159](https://github.com/L-K-M/Foldover/pull/159) | Combine | Conflict exit 4, partial restore exit 1, real-agent regressions |
| [160](https://github.com/L-K-M/Foldover/pull/160) | Combine | Complete token-bound paging and kind filters; 125-item process test |
| [161](https://github.com/L-K-M/Foldover/pull/161) | Combine and fix | Four running pairs; slot waits inside tasks; early results and aggregate exits tested |
| [162](https://github.com/L-K-M/Foldover/pull/162) | Merge | Retryable reads preserve uncertain-commit warnings for mutations |
| [163](https://github.com/L-K-M/Foldover/pull/163) | Merge | Count Unicode destinations once; preserve ambiguity and preparation budget |
| [164](https://github.com/L-K-M/Foldover/pull/164) | Combine | Agent owns the copy clock; rate survives proto, FFI and app boundaries |
| [165](https://github.com/L-K-M/Foldover/pull/165) | Combine | Restore reservation through reload, busy feedback and failure recovery tests |
| [166](https://github.com/L-K-M/Foldover/pull/166) | Fix and merge | Slow budgets and poisoned connections; include remote archive reads |
| [167](https://github.com/L-K-M/Foldover/pull/167) | Merge | Named progress indicators retained through menu-bar integration |
| [168](https://github.com/L-K-M/Foldover/pull/168) | Fix and combine | Stop routing, bounded rows and larger-font tests; stable button targets and conservative display cap |
| [169](https://github.com/L-K-M/Foldover/pull/169) | Supersede with 164 | Keep unit promotion, duration formatting and tests; replace client clock with agent clock |
| [170](https://github.com/L-K-M/Foldover/pull/170) | Supersede with 168 | Keep bounded selectable errors; 168 supplies scaled rows and display-aware scrolling |
| [171](https://github.com/L-K-M/Foldover/pull/171) | Fix and merge | Freshness updates over time; supply real history in its failing test fixture |
| [172](https://github.com/L-K-M/Foldover/pull/172) | Merge | Relative archive dates retain clock times and boundary tests |
| [173](https://github.com/L-K-M/Foldover/pull/173) | Merge | Keep aborted first runs gated; correct the identity-commit contract |
| [174](https://github.com/L-K-M/Foldover/pull/174) | Merge | Re-subscribe changed roots; retain real notification regression |
| [175](https://github.com/L-K-M/Foldover/pull/175) | Merge | Bound each peer counter report; retain unit and link-level regressions |
| [176](https://github.com/L-K-M/Foldover/pull/176) | Merge | Pair commands follow the settled health answer during probes |
| [177](https://github.com/L-K-M/Foldover/pull/177) | Supersede with 165 | Keep reservation rationale and the held-list assertion in 165's stronger tests |
| [178](https://github.com/L-K-M/Foldover/pull/178) | Merge | Share rows across renders; state/page revisions invalidate the cache |
| [179](https://github.com/L-K-M/Foldover/pull/179) | Supersede with 168 | Its bounded list is covered by 168, including compact short catalogs |

The review found three red native checks: 168's scroll test, 169's
actor-isolated formatting calls, and 171's empty history fixture. The
scroll screenshots showed the target row on screen: its combined label
is a button, while the test queried a static text. The consolidation
corrects those paths. CLI regressions reproduced delayed
Sync All output, swallowed progress and incorrect aggregate conflict
exits before their fixes. Help-value and archive-budget tests also failed
before their fixes.

Remaining qualification: large-preview profiling, physical mixed-display
layout and hands-on VoiceOver. Peer progress is bounded per report;
counted callbacks or a per-observation budget would also bound CPU
amplification across a hostile stream. Per-file bytes in flight and stale
archive replies remain separate work below.

## Must fix

### SOL-03: Old archive requests can change the current browser

- **What this is:** Archive browsing is asynchronous; people can switch
  side or run before a request finishes.
- **Problem:** `ArchiveStore.swift:123-147, 210-230` guards some successes
  but applies old errors and loading cleanup unconditionally. Returning
  to the same side also defeats identity-only guards. Whole-run restore
  checks context before an awaited reload, then publishes its summary
  without rechecking afterward.
- **Fix:** Use listing/request generations for success, failure and
  cleanup. Publish restoration feedback only into its original context.
- **If skipped:** A slow failure from Folder A can obscure a successful
  Folder B listing, or an old completion can describe the wrong run.
- **Acceptance:** Controlled out-of-order responses, including A to B to A,
  a stale failure, and navigation during the post-restore reload.

## Should fix

### SOL-04: Searching a preview searches only downloaded rows

- **What this is:** The toolbar offers path search over a preview.
- **Problem:** `PairSyncViewModel.swift:386-449` filters locally; the
  protocol's first response contains at most 100 items
  (`crates/foldover-proto/src/local/sync.rs:14-16`). A matching path later
  in a large plan can yield an empty search until Show More is clicked
  repeatedly. Existing pagination is useful, but it does not make search
  complete.
- **Fix:** Add path filtering to token-bound server paging. Say how many
  results are loaded and retain a clear search scope in the interim.
- **If skipped:** Users can mistake an unseen destructive change for a
  change that does not exist.
- **Acceptance:** Find a match beyond row 10,000 without downloading the
  preceding rows; stale review tokens still fail closed.
- **Related shipped work:** PR 160 (`sync ID --changes`) pages the full
  preview over the CLI; server-side filtered search remains open.
- **Mimo addendum:** until search reaches the whole plan, the empty
  state should not claim "No Matching Changes" while unloaded pages
  remain; say the search covers the changes loaded so far.

### SOL-05: Large-file progress looks frozen

- **What this is:** Progress counts committed operations and their bytes.
- **Problem:** `crates/foldover-engine/src/run/copies.rs:166-175` reports
  copied bytes when a whole file lands. `PairSyncViewModel.swift:305-330`
  displays that count. One 20 GB transfer can show no byte progress for
  minutes; no throughput or reliable remaining-time estimate is exposed.
- **Fix:** Report bytes in flight through the endpoint abstraction,
  separately from committed bytes. Throttle publication, show the current
  file, and estimate remaining time only after enough stable samples.
- **If skipped:** Normal transfers resemble hangs and invite cancellation.
- **Acceptance:** A throttled large file advances while being copied;
  retries do not double-count, and cancellation remains responsive.
  Add a percentage caption and an explicit accessibility value to the
  existing progress bar as a small interim improvement.
- **Related shipped work:** PR 164 adds the cumulative copy rate
  (`SyncProgress.rate`) and an `N MB/s · about T left` status segment,
  hidden while the job waits on another Mac. In-flight per-file byte
  progress remains open. The consolidation retains 169's unit promotion,
  duration formatting and tests over 164's agent-owned clock.

### SOL-06: Profile cached preview rendering at scale

- **Implemented:** PR 178 caches rows across renders by store revision,
  including state, pages and kind filters. Search filters cached rows.
- **Acceptance:** Profile 10k, 100k and 200k loaded rows on macOS; typing
  and selection must avoid repeated full presentation rebuilds and meet
  PLAN.md's interaction budgets.

### SOL-07: Pair-local copy limits do not protect a shared disk

- **What this is:** Copies have sensible concurrency within one pair.
- **Problem:** The documented limit is per pair, not per physical device
  (`docs/guide/limitations.md:97-108`). Several independent pairs on one
  spinning disk or NAS multiply workers and can turn sequential I/O into
  seeks and queueing. This is a known limitation.
- **Fix:** Budget workers by held volume/device identity, with a
  conservative fallback for unknown devices. Keep pair bandwidth limits
  separate and cancellation-aware.
- **If skipped:** Sync All can make each job slower and the Mac less
  responsive even though each pair stays within its own limit.
- **Acceptance:** Two HDD-backed pairs share the disk's worker count;
  independent disks still run concurrently. Benchmark total completion
  time and foreground latency.
- **Related shipped work:** PR 161 (`sync --all`) already caps CLI fan-out
  at four concurrent pair jobs; the per-device worker budgeting it
  approximates remains engine work.

### SOL-08: Retained archive runs eventually disappear from browsing

- **What this is:** Endpoint archives retain versions for 30 days.
- **Problem:** `crates/foldover-service/src/archive.rs:27-42` truncates
  runs to `ARCHIVE_RUN_LIMIT` (1000) and exposes only `more`; there is no
  run cursor. Frequently changing pairs can retain older versions that
  the app cannot reach. The UI admits older runs are omitted.
- **Fix:** Cursor-page run history, preserving newest-first ordering;
  add date narrowing without loading every run into the app.
- **If skipped:** Users cannot browse all versions that retention promises
  are still present.
- **Acceptance:** Reach a version older than the first 1000 runs through
  both app and CLI; additions during paging must not duplicate rows.

### SOL-09: Background failures need an app-independent signal

- **What this is:** Sync keeps running when the app quits.
- **Problem:** Failure notifications are delivered by the app; with it
  closed, stopped automatic runs do not alert anyone
  (`docs/guide/limitations.md:38-41`). This is documented, but undermines
  the background-agent workflow.
- **Fix:** Persist attention events in the agent and deliver them through
  the existing native notification boundary with explicit permission.
  Coalesce repeats and preserve events until acknowledged.
- **If skipped:** A scheduled backup can remain stopped unnoticed.
- **Acceptance:** Quit the app, cause a scheduled failure, and receive one
  actionable alert. Restarting must not duplicate an acknowledged alert.

### SOL-10: Recovery needs error details, not only a failure count

- **What this is:** The service already returns individual restore
  failures with paths and messages.
- **Problem:** `CoreArchiveClient.putBackAll` reduces the response to
  `RunRestore.failures: UInt64` (`ArchiveStore.swift:33-39, 70-72`). Users
  learn that files stayed archived but cannot see why or retry just those
  failures from that result.
- **Fix:** Preserve the bounded failure list and total count through the
  store. Show failed items with actionable causes and retry selection.
- **If skipped:** Permission, disconnected-folder and name-collision
  problems all look like the same unexplained count.
- **Acceptance:** A mixed restore names each returned failure, retains
  the total when truncated, and never labels a partial result successful.

### SOL-11: Qualify real storage and UI performance before broad trust

- **What this is:** The project has simulations, filesystem image tests
  and five Rust microbenchmarks.
- **Problem:** They do not establish real NAS throughput, end-to-end
  million-file memory, 200k-row UI smoothness, agent idle power, or write
  fidelity on physical foreign filesystems. These gaps are acknowledged
  in `docs/guide/limitations.md` and `docs/hardware-validation.md`.
- **Fix:** Execute the existing hardware checklist and add retained
  measurements for the missing PLAN.md section 5.3 budgets. Include cold
  and warm runs, sleep/wake, low free space and disconnected volumes.
- **If skipped:** Passing synthetic tests can be mistaken for verified
  performance and recovery on a user's actual disks.
- **Acceptance:** Record hardware, dataset, build SHA, measurements and
  failures. The owner retains release and hands-on VoiceOver decisions.

### F-01: `pairs update` re-asks for everything to change one option

- **What this is:** Updating a pair requires re-supplying NAME LEFT RIGHT
  MODE positionally even to toggle `--schedule`.
- **Problem:** `crates/foldover-cli/src/main.rs` `pairs update` treats
  roots as required positionals; they print as JSON byte arrays in `pairs
  show` output, so a one-flag change means copying opaque bytes.
- **Fix:** Add `pairs set ID REVISION [OPTION VALUE]...` that fetches the
  pair, applies named option overrides and calls `update_pair`; reuse the
  existing option parser.
- **Acceptance:** `pairs set ID R --schedule changes` keeps every other
  option; a stale revision is still refused.

### F-02: CLI JSON output prints paths as byte arrays

- **What this is:** Every item line prints `"path":[104,101,…]` — exact
  for any encoding, unreadable for people.
- **Fix:** A `--text` flag rendering UTF-8 paths as strings (arrays stay
  for non-UTF-8) across preview items, archive items and history output.
  Medium surface; cosmetic gain.
- **Acceptance:** `sync ID --changes --text` prints quoted paths; a
  non-UTF-8 name still round-trips through the array form.

### F-03: Export or copy the change list

- **What this is:** Preview results cannot leave the app: no Copy for
  multi-selection, no CSV/JSON export.
- **Fix:** A "Copy Paths" context command and/or File › Export; small
  Swift-side add on the same store boundary.
- **Acceptance:** A filtered selection copies its paths; export writes a
  parseable file the UI tour can screenshot.
- **Mimo addendum:** the change table is single-selection today
  (`Table(rows, selection: ChangeRow.ID?)` in `PairDetailView.swift`);
  multi-select with bulk actions is MIMO-27 and should land with or
  before Copy Paths.

### F-04: Notify when a run recovers leftovers

- **What this is:** `recovered > 0` on a finished run means a previous
  run was interrupted; alerts currently cover needsApproval/failed only.
- **Fix:** One coalesced notification for recovered leftovers; opt-in
  under the existing permission flow.
- **Acceptance:** An interrupted rerun produces exactly one alert; none
  when the app already foregrounds the pair.

### F-05: Menu bar shows status but never when

- **What this is:** The extra shows "In sync" style text but not *when* —
  Time Machine's key glanceable. History is already fetched.
- **Fix:** "Synced 12 min ago" under the pair name; complements the
  deeper trust summary in SOL-16.
- **Acceptance:** Fixture pairs render relative times; no extra polling
  beyond the existing history refresh.

### F-06: Linux service packaging

- **What this is:** The engine and CLI already run headless on Linux
  (verified end-to-end: pair add → preview → approve → sync).
- **Fix:** A `packaging/systemd` user unit for `foldoverd`; GTK4 app
  remains the planned larger piece.
- **Acceptance:** `systemctl --user status foldoverd` runs on a clean
  Linux host; `foldover status` answers over the default socket.

### F-07: NAS pre-flight capability probing

- **What this is:** `pairs check` warns on SFTP roots; the limitations
  doc leaves "per-volume capability probing" unimplemented.
- **Fix:** Probe a scratch file — create, statfs, xattr attempt — to
  predict xattr/permission/records support before the first sync, and
  surface the result in the pair editor.
- **Acceptance:** A capability-weak server reports predicted gaps before
  a sync, not as failures during one.

### F-08 through F-14: shipped this session

`--help`/`-h` exit 0 on both binaries (#157); `sync ID --progress`
streaming (#158); `sync ID --changes [KIND]` preview paging (#160);
`sync --all` capped concurrent runs with aggregate exit (#161); read-vs-
write FFI timeout messages (#162); copy rate + ETA in proto, FFI and app
(#164); slow-request IPC budget + poisoned-connection hardening (#166).

## Nice to have: interface and product improvements

### SOL-12: Qualify the menu bar on physical displays

- **Implemented:** The consolidation retains 168's Stop routing and
  scaled, scrolling rows, 170's bounded selectable errors, and a list
  budget based on the smallest attached display. Default and large-text
  UI tests exercise a 44-pair fixture.
- **Acceptance:** Verify short and mixed-height displays, keyboard
  access and VoiceOver with the popover on each display.

### SOL-13: A conflict inspector that compares both versions

- **What this is:** Current conflict choices identify Folder A and B but
  show little evidence for choosing one.
- **Problem:** `PairInspector.swift:24-38` shows one path and action;
  `ChangeRowActions` offers keep-side commands without side-by-side
  sizes, dates or content previews.
- **Fix:** Add two compact version cards using actual endpoint names,
  size and modification time. Keep Both remains the safe visible choice;
  text/image comparison can follow through the same store boundary.
- **If skipped:** Users leave the app to investigate each conflict.
- **Acceptance:** Missing and remote versions are explicit; dates never
  imply which version is correct; every choice remains review-token-bound.

### SOL-14: Quiet theming with a consistent semantic palette

- **What this is:** Native controls already inherit the system appearance.
- **Problem:** Warning, failure and activity styling is repeated across
  views. Existing light/dark tours do not establish increased-contrast
  behavior or a shared visual contract for future components.
- **Fix:** Centralize semantic tints and spacing for touched components.
  Support System, Light and Dark only if an in-app preference is useful;
  preserve system accent and Increase Contrast. Use restrained warmth in
  empty states, not decorative backgrounds behind file tables.
- **If skipped:** Future additions drift in density, color and emphasis.
- **Acceptance:** Screenshot light/dark and increased-contrast states at
  minimum widths; status remains understandable without color.
- **Fusion addendum:** per-pair color/tag (Finder-style) in sidebar and
  menu bar, a monogram glyph from pair initials, a Reduce-Motion-aware
  pulse on the status dot while connecting, `.badgeProminence` on the
  numeric conflict badge, and deletion counts in the approval button or
  dialog title all fit the same palette work.
- **Mimo addendum:** tint the sync modes themselves (two-way, mirror,
  backup) so a screenshot is readable at a glance; adopt the icon's
  blue as the default in-app accent (still user-overridable); and keep
  one status-symbol vocabulary (spinner, warning, waiting, idle) shared
  by sidebar, detail, menu bar and notifications — today each surface
  names its own glyphs.

### SOL-15: Make the editor's first three decisions obvious

- **What this is:** The editor exposes folders, mode, schedule, rules,
  metadata and advanced options in one form.
- **Problem:** First-time users must scan specialist controls before
  saving; its minimum width is 800 points (`PairEditorView.swift:20-33`).
- **Fix:** Keep Name, Folders and Direction visible. Collapse optional
  rules and advanced controls while surfacing active nondefaults and
  warnings. Preserve the live plan and the save-does-not-sync rule.
- **If skipped:** Pair creation feels more technical than the core task.
- **Acceptance:** A new user creates a safe local pair in under three
  minutes; keyboard navigation and edited nondefaults remain discoverable.

### SOL-16: A trust summary with honest freshness

- **What this is:** Sidebar status says Ready, In sync or Needs Attention.
- **Problem:** These labels do not distinguish a recent successful check
  from one performed yesterday or show when a held pair will run next.
- **Fix:** Show Last checked, Last successful sync and the next scheduled
  opportunity, with explicit Waiting for disk/network/review states.
  Never call a disconnected folder current.
- **If skipped:** A calm icon can convey more confidence than the evidence.
- **Acceptance:** Frozen-clock tests cover stale, paused, offline and
  attention states; relative dates update without frequent global redraws.
  A Dock badge can expose pending review while the main window is closed.
- **Fusion addendum:** the menu-bar variant is F-05; a "time travel"
  one-liner ("Everything in sync · last checked 10:42 · next ~10:52")
  and a sync forecast ("≈ 3.2 GB to copy — about 4 min at recent
  speeds", using the rate plumbing shipped in PR 164) are the same data
  shown earlier and cheaper.

### SOL-17: A named safety rehearsal for a first pair

- **What this is:** The initial preview already blocks unsafe first runs.
- **Problem:** People still need to understand what archive and Undo
  actually recover before trusting them.
- **Fix:** Offer an opt-in rehearsal with disposable sample folders:
  create, edit, delete, preview, sync and restore one sample. Label it
  clearly and never use a user's selected roots.
- **If skipped:** Recovery remains something first learned under stress.
- **Acceptance:** No real pair configuration is changed; all samples are
  removable and the rehearsal can be skipped permanently.
- **Fusion addendum:** a `foldover sim` CLI variant — run a pair in a
  scratch dir — is nearly free with `--socket` +
  `StateLocation::Directory` (both already work) and demonstrates the
  product without touching real data.

### SOL-18: Give versions an expiration receipt

- **What this is:** Archives have a fixed 30-day retention policy.
- **Problem:** The browser shows a run's time but not when its recoverable
  versions expire.
- **Fix:** Show a compact expiration date and explain that versions live
  on that endpoint. Offer a non-destructive Export Copy action before
  expiry; do not change retention implicitly.
- **If skipped:** A user can postpone recovery past the pruning date.
- **Acceptance:** Expiry matches the engine's cutoff; exporting preserves
  archived originals, metadata and occupied destination files.
  Add read-only Quick Look and Reveal for local archive items first, so
  users can inspect a version before restoring it.
- **Fusion addendum:** a symlink or `.fileprovider` view of
  `.foldover/archive` would give power users Finder-native recovery
  without opening Foldover.

### SOL-19: A restrained Foldover signature moment

- **What this is:** The app already animates menu-bar activity and honors
  Reduce Motion.
- **Problem:** Completion is largely textual; the product name has little
  visual expression outside the icon.
- **Fix:** On a clean interactive completion, let the two folder chips
  briefly settle into a folded-paper checkmark, then return to a quiet
  dated receipt. No confetti, sounds, modal success or perpetual motion.
- **If skipped:** No functional loss; this is a small identity improvement.
- **Acceptance:** No animation for unattended runs, partial success or
  conflicts; Reduce Motion gets a static transition; no extra idle timer.
- **Fusion addendum:** an opt-in completion chime/haptic on syncs with
  >0 copies is the same moment for the opposite sense.

### SOL-20: Explain a change and let users test exclusions

- **What this is:** A preview names the operation; skip rules are authored
  as gitignore-style text.
- **Problem:** A surprising change does not explain which observation or
  rule caused it. Testing a new exclusion currently means another preview.
- **Fix:** Add Why this change? from reconciliation evidence, and a
  read-only rule tester with representative matching/nonmatching paths.
  Explain deletion versus missing/unreadable paths in plain language.
- **If skipped:** Users troubleshoot by trial and error around potentially
  destructive operations.
- **Acceptance:** Explanations come from typed engine facts, not UI
  guesses; rule testing uses exactly the engine's matcher and writes nothing.

### F-15 to F-18: additional product ideas from the second pass

- **F-15 Quiet hours:** "never sync between 22:00–07:00" — a schedule
  complement for battery and NAS noise; fits the existing
  `Schedule`/`Environment` machinery.
- **F-16 `sync ID --report out.html`:** render a preview/result as a
  static page for emailing admins — small generator over the existing
  typed result.
- **F-17 Change-table width:** let the Item column take max width before
  Change; the 24 px icon column plus three text columns squeeze paths.
- **F-18 Hash buffer:** `HASH_BUFFER_BYTES` 64 KiB → 256 KiB–1 MiB to cut
  syscall count on huge files (`crates/foldover-fs/src/root/content.rs`).
  Low priority; usually I/O-bound.
  **Mimo addendum:** `root/write/partial.rs:83` hashes resume prefixes
  through `io::copy`'s 8 KiB buffer instead of this loop — route it
  through `HASH_BUFFER_BYTES` too.

## Additional engine and endpoint findings

These findings came from independent read-only passes and were checked
against the connected source. They take priority over decorative work.

### SOL-21: A live peer can keep a stalled stream waiting forever

- **Evidence:** `crates/foldover-endpoint/src/link/stream.rs:50-94`
  waits for a frame or link shutdown. Unlike the ordinary request path,
  it has no stall deadline. Keepalives can keep a non-answering peer live.
- **Impact:** A connected peer that stops answering can hold a sync and
  its blocking worker indefinitely; cancellation is checked between
  frames, not during the blocked frame read.
- **Task:** Add cancellation-aware frame waits and a stall policy.
  Preserve legitimate long verification and watch streams with explicit
  progress/heartbeat semantics rather than one short timeout everywhere.
- **Acceptance:** A paired host accepting a request but never replying
  must release the caller on cancel and on the stall deadline. Slow
  healthy transfers and idle watches must remain valid.

### SOL-23: Many failed or unavailable paths multiply per-row work

- **Evidence:** `run.rs:1574-1576` linearly searches failed ancestors per
  operation; `plan.rs:317-331` searches all skip roots per row;
  `run/undo.rs:237-278` repeatedly normalizes unavailable and unstable
  paths for every journal entry. Endpoint observation filtering has the
  same entries-times-unavailable shape in `local.rs`, `sftp.rs` and
  `remote.rs`.
- **Impact:** Thousands of unreadable paths in a 100k-file tree can turn
  degraded but otherwise safe syncs or Undo into prolonged CPU work.
- **Task:** Precompute canonical protected-root indexes once per phase.
  Test the path's own ancestors against the index, or maintain a minimal
  prefix-free root set. A single greatest lexical predecessor is unsafe:
  an unrelated sibling can hide an earlier matching ancestor.
- **Acceptance:** Compare indexed decisions against the existing linear
  reference on generated trees, including `a`, `a-`, `a/z`, deep paths,
  Unicode aliases, unreadable parents and readable siblings. Benchmark
  100k entries with thousands of protected roots and cancellation.

### SOL-24: Every archive page walks the whole archived tree

- **Evidence:** `crates/foldover-endpoint/src/local/archive.rs:190-229`
  and `sftp.rs:1159-1194` traverse and collect the run, then sort and
  truncate. The cursor reduces returned rows, not listing work.
- **Impact:** Browsing a 100k-file archive in 100-row pages repeats a
  full tree walk for every page, especially costly over SFTP.
- **Task:** Implement ordered, bounded traversal with subtree cursor
  pruning, or retain a bounded token-bound listing index. Preserve raw
  path order, symlink confinement and errors for directories actually read.
- **Acceptance:** Pages concatenate to the same exact ordering; count
  filesystem/server listing calls to prove later pages avoid full rescans.

### SOL-25: Waiting for an SFTP session cannot observe cancellation

- **Evidence:** `crates/foldover-endpoint/src/sftp/sessions.rs:123-127`
  uses an unbounded condition-variable wait when the pool is exhausted.
- **Impact:** An operation can wait behind a multi-hour transfer after
  Stop was requested; per-request network timeouts do not bound pool wait.
- **Task:** Pass the operation's checkpoint through session acquisition
  and use bounded waits that poll it. Preserve connection limits and
  return borrowed sessions reliably on failure.
- **Acceptance:** Occupy every session, request another operation, cancel
  it, and prove prompt completion without opening an extra connection.

### SOL-26: The SFTP chooser ignores a selected enclosing folder

- **Evidence:** `apple/Foldover/Views/PairEditorView.swift:586-593`
  accepts a highlighted child only when it is in `folders`, but the
  Enclosing Folder row is represented separately by `parent`.
- **Impact:** Highlighting Enclosing Folder and choosing Use This Folder
  silently selects the currently browsed folder instead.
- **Task:** Validate highlighted selection against both parent and child
  entries before committing the locator. Keep the visible path and
  effective chosen root consistent.
- **Acceptance:** Tests for parent, child, no selection and stale
  selection; a UI tour verifies the saved root bytes match the selection.

### SOL-27: Deleted pairs keep their loaded preview state in memory

- **Evidence:** `MainViewModel.removePair` removes configuration but
  `SyncStore` keeps its per-pair states, listings, histories and searches.
- **Impact:** Repeated create/review/remove cycles retain potentially
  large listings throughout a long-running menu-bar app session. Random
  pair IDs make ID reuse an unsuitable basis for claiming a separate bug.
- **Task:** Prune per-pair data against each authoritative catalog refresh.
  Invalidate outstanding read/paging tickets so removed state cannot be
  reintroduced by a delayed response.
- **Mimo addendum:** the agent leaks the same way: `foldover-service/
  src/sync.rs:169` keeps `jobs`, `skips` and `mutating` entries after
  `PAIR_REMOVE`, so `status()` and `items()` keep answering for a
  deleted id. Prune both sides. `MainViewModel.changeSelections` and
  the `SyncStore` search/decline maps have the same shape.
- **Acceptance:** Remove a pair with loaded pages and an outstanding
  request; its state stays forgotten after that request completes.

## Fourth review additions (Muse.md)

Unique findings of the fourth pass. Material that only restates SOL-,
MIMO- or F- entries, or that ships in the table above, is not repeated.
PRIORITY table rows: MUSE-01 (P1), MUSE-02 and MUSE-03 (P2).

### MUSE-01: Lease renewal is a non-atomic read-then-replace

- **Evidence:** `crates/foldover-endpoint/src/lease.rs:287-301`
  (`HeldLease::renew`) reads `.foldover/lock`, checks the holder, then
  `replace_lock_file` without an exclusive claim. Takeover uses the
  exclusive-create protocol (ADR 0032); the steady-state beat does not.
- **Impact:** A reclaimer that wins between the check and the write can
  be overwritten by the old holder for up to one beat, briefly allowing
  two writers. The window is small but the lease is the only mutual
  exclusion on a shared folder.
- **Task:** Renew through the same exclusive-create claim path as
  takeover, or re-read after write and surface `LeaseError::Lost` on
  mismatch. Record the decision in ADR 0032 (amendment or "accepted
  risk with bound").
- **Acceptance:** Two processes: one holds and renews while the other
  reclaims; the loser observes `Lost` and never writes after takeover.

### MUSE-02: WorkProgress equality is a tautology

- **Evidence:** `apple/Foldover/Stores/SyncStore.swift` `WorkProgress.==
  always returns true`, with a comment that equal-looking working states
  must not look like new results.
- **Impact:** Harmless under `@Observable` rendering today, but any
  future `onChange(of:)` on a state embedding progress will never fire.
  The comment documents intent; the type still lies.
- **Task:** Prefer explicit progress events or a dedicated equality
  that ignores counters but compares `waitingFor`/job identity; document
  whichever contract stays.
- **Acceptance:** A regression test shows a job-level transition (idle →
  working, waiting → copying) is observable and a pure counter tick is
  not.

### MUSE-03: `foldover sync ID` applies immediately by default

- **Evidence:** `crates/foldover-cli/src/main.rs` `sync_command` with
  no flags builds `SyncCommand::Run { left: 0, right: 0, … }`. The
  README's app story is preview-first; the CLI's bare verb is a full run.
- **Impact:** A user following "sync ID" muscle memory can write without
  seeing the plan. First-run gates still stop new pairs; existing pairs
  do not.
- **Task:** Either make no flags mean preview + exit 3 with a rerun hint,
  or require an explicit `--run`/`--apply` for execution. Align README
  and usage text either way.
- **Acceptance:** Documented behavior matches the binary; a regression
  test pins the chosen default.

### MUSE-04: Archive pruning accepts timestamp-named files

- **Evidence:** `crates/foldover-endpoint/src/local/archive.rs:272-305`
  collects any top-level entry whose name parses as a run timestamp and
  `remove_tree`s it; `archived_runs` filters to directories.
- **Impact:** A stray file named like a run folder would be deleted by
  retention. Low likelihood (Foldover only creates directories), real
  consequence (pruning is the only path that deletes data, per
  limitations).
- **Task:** Require `NodeKind::Directory` before scheduling removal.
- **Acceptance:** A file whose name matches the run format survives
  pruning; timestamp directories still expire.

### MUSE-05: Small UI edges from the fourth pass

- **Fixed inspector width:** `MainView.swift` pins the inspector pane at
  280 pt with a decorative divider; long paths and conflict details
  cannot widen it. Let the pane drag or grow to an ideal width without
  reintroducing the `.inspector` column crash (see `SplitColumn` notes).
- **Empty detail while loading is `Color.clear`** (`MainView.swift`
  emptyDetail): a blank pane beside a sidebar spinner; a lightweight
  `ProgressView` would say what is happening.
- **Inspector empty icon is `sidebar.trailing`** (the toggle glyph);
  prefer `info.circle` or `slider.horizontal.3`.
- **Change-table symbol column header is `""`:** per-row action labels
  exist; a column-level accessibility name would help explorers.
- **SetupView current-step badge** hardcodes white text on accent;
  check Increase Contrast.
- **Status bar and placeholder can both offer actions** after a finished
  sync when the table is empty (`PairDetailView` `placeholderSpeaks`);
  pick one primary surface.
- **Archive run list has no item counts.** `ArchiveRun` only carries
  name + `started_ms`; counts need a wire field or a selected-run
  summary line (the selected run's loaded page can state its size now).
- **CLI help is one giant USAGE string** (`main.rs` const `USAGE`):
  `foldover sync --help` now works, but shows every command. Focused
  subcommand usage and epilogues remain optional work.
- **Menu-bar "q" shortcut** is already MIMO-18; noted again because
  the fourth pass hit it while bounding the popover.

### MUSE-06: Delight ideas not already listed elsewhere

Consolidated; cross-references show where a named SOL/MIMO/F entry
already owns the work. Shovel-ready small ideas:

- Completion breath: one Reduce-Motion-aware pulse of the pair status
  icon on a clean interactive completion — part of SOL-19's signature
  moment; do not add a second animation system.
- Undo toast after a window sync ("Synced · Undo", auto-dismiss):
  teaches the existing Edit › Undo; pure Swift on the window model.
- Menu-bar progress ring replacing the spinner — only after SOL-05's
  in-flight bytes exist; otherwise it repeats PR 164's numbers.
- Conflict compare: two-pane local Quick Look for both-changed pairs —
  belongs with SOL-13's version cards.
- First-sync receipt: "First sync done — N files now protected" on the
  completion card; pairs with SOL-17's rehearsal.
- `foldover watch ID`: re-preview on a timer, one-line diff counts;
  fits the CLI after PR 158's `--progress`.
- Pair templates ("Photos → NAS" preset of mode/schedule/filters) in
  the editor's Choose menu.
- Pair health filter: clicking "N need attention" in the sidebar
  narrows the list to attention rows.

## Third review additions (Mimo.md)

Unimplemented findings of the third pass. Entries delivered as open PRs
are in the table above instead. Style and priority rules hold: reproduce
before fixing, keep the review-token boundaries, and keep visual
proposals separate from measured defects.

### MIMO-01: The FSEvents history sentinel can swallow a change

- **Evidence:** `crates/foldover-watch/src/history/fsevents.rs:217`.
  macOS ORs `HistoryDone` onto the last replayed event, which can
  itself be a real change; the early `Ok(false)` skips `relevant(&path)`
  for that event and everything queued behind it in the batch.
- **Impact:** A change riding with the replay sentinel is not noticed
  until the periodic verification run — the pair looks current while a
  real edit waits an hour. macOS-only; reported by scan, not reproduced.
- **Task:** Evaluate `relevant(&path)` before honoring the sentinel, and
  drain already-queued post-replay events before returning.
- **Acceptance:** Force a replay whose last event is a user change and
  observe it reported; a sentinel alone still reports nothing.

### MIMO-02: A skip from a stale plan survives a pair edit

- **Evidence:** `crates/foldover-service/src/sync.rs:292`. `items()`
  (`:373`) and `keep_both()` (`:263`) check `reviewed.revision` against
  the pair; `skip()` does not.
- **Impact:** Skip Once taken from a preview survives `PAIR_UPDATE` and
  applies if the new folders hold matching versions at that path.
- **Task:** Validate the revision in `choice()`, or at least in `skip`.
- **Acceptance:** Skip a row, edit the pair, present the same path with
  equal versions; the stale skip must be refused.

### MIMO-03: Two runs can share one archive folder

- **Evidence:** Run names are millisecond timestamps
  (`foldover-engine/src/run.rs:1106`, `foldover-endpoint/src/local/
  archive.rs:73`, `sftp.rs` same helper).
- **Impact:** A clock rollback or a restart inside one millisecond
  merges two runs into one archive folder; undo of the later run can
  put the earlier run's file back at a colliding path.
- **Task:** Make run names collision-proof (append the lease holder id
  or a short unique suffix); keep names sortable by time.
- **Acceptance:** Force the same name twice; the second run archives
  beside the first, and undo of either restores only its own entries.

### MIMO-04: A skip rule with a newline silently skips other files

- **Evidence:** `apple/Foldover/Stores/PairStore.swift:467`
  (`skipRule(for:)`) escapes spaces and glob characters but not control
  characters; a path containing `\n` yields a two-line gitignore rule.
- **Impact:** The second line silently excludes unrelated paths from
  every future sync of the pair.
- **Task:** Reject control characters like the non-UTF-8 case (an error
  the Always Skip flow already displays), or escape them.
- **Acceptance:** Always Skip on a path with a newline either refuses
  with its message or produces one inert rule.

### MIMO-05: One failed run disables scheduled sync until a manual run

- **Evidence:** `crates/foldover-service/src/sync.rs:563` treats
  `Failed` like `NeedsApproval`/`Undone` when claiming `Work::Scheduled`.
- **Impact:** A transient failure (network blip, timeout) stops
  unattended sync with no recovery except a person syncing by hand;
  ADR 0022's intent is to stop for *review*, not for retryable errors.
- **Task:** Retry scheduled work after `Failed`, or clear `Failed` after
  a cooldown; keep the block for states that genuinely await a person.
  Decide and document, since the current behavior may be deliberate.
- **Acceptance:** A scheduled run that fails once runs again on its next
  cadence; `NeedsApproval` and `Undone` still wait.

### MIMO-06: Undo can panic on edge-case and corrupt entries

- **Evidence:** `crates/foldover-engine/src/run/undo.rs:243,256,271`
  `expect("a name key is a valid path")` after `name_key`, which
  lowercases and can grow a path past `SyncPath`'s byte limit (e.g.
  `İ`), and on malformed stored rows.
- **Impact:** Undo of a run touching such names panics the job; the
  `catch_unwind` keeps the agent alive but the undo fails. Undo's
  reporting also counts a folder restored `Beside` as unrestored
  (`undo.rs:171`, `205-217`).
- **Task:** Return the existing soft errors instead of panicking; count
  `Placed::Beside` restores as refilling their parent.
- **Acceptance:** Journal entries near the name limit and with bad bytes
  produce skipped rows in `not_undone`, never a panic.

### MIMO-07: Small app-side correctness edges

Four independent, small fixes in the SwiftUI layer.

- **Counts can trap:** `PairSyncViewModel.swift:483` (`hiddenRowsText`)
  and two siblings subtract `UInt64` counts the agent reports from
  counts the listing carries; use `subtractingReportingOverflow` or a
  zero floor.
- **A completion card offers Undo that can never enable:**
  `PairSyncViewModel.swift:456` ignores the `records == .both` rule
  `undoableResult` applies, so an archive-less Backup target shows a
  dead button.
- **Mixed outcomes notify only the conflicts:** `SyncStore.swift:518`
  alerts conflicts and drops the failure count of the same run.
- **Sync while an approval is pending just re-prompts:**
  `PairSyncViewModel.swift:520` runs with zero approvals, bouncing to
  the dialog; disable the command or present the dialog instead.

### MIMO-08: Per-file fsyncs dominate many-small-file runs

- **Evidence:** `crates/foldover-fs/src/root/write.rs` syncs the file
  and one or two directories per placement and per removal, at both
  durability levels (sites listed in the ADR 0019 area).
- **Impact:** A run over N files in one folder costs ~2N+ fsyncs; on
  spinning disks and shares this, not copying, is the wall time.
- **Task:** Batch directory syncs per run or per commit batch under
  `Durability::Standard`; keep per-op syncs under `Full`. Respect
  ADR 0019's durability contract and staged-write recovery.
- **Acceptance:** Equal safety under injected crashes at batch
  boundaries; benchmark 10k small files before and after.

### MIMO-09: Every SFTP scan re-downloads all file content

- **Evidence:** `crates/foldover-endpoint/src/sftp.rs:480`
  (`observe_reporting`) ignores its `previous` snapshot and reads and
  hashes every file on every scan; deliberate, since stat cannot prove
  unchanged.
- **Impact:** Periodic verification on a 100 GB SFTP root moves 100 GB;
  the documented shallow checks stop being shallow.
- **Task:** A stat-fingerprint fast path (size, mtime second, mode
  unchanged against the stored snapshot) that skips reads for matching
  entries, with an explicit weaker-guarantee note and a periodic
  full-hash pass that keeps the strong guarantee.
- **Acceptance:** A second scan of an unchanged tree transfers listings
  only; an edit that preserves size and mtime is still caught by the
  periodic hash pass.

### MIMO-10: Each review choice re-runs the whole preview

- **Evidence:** `apple/Foldover/Stores/SyncStore.swift` `choose` follows
  every Skip Once / Keep A / Keep B / Keep Both with `preview(id)`.
- **Impact:** Ten decisions on a 100k-entry plan cost ten full rescans,
  each with only "Resolving…" to look at; reviewing a plan feels slow in
  exactly the moment the product promises responsiveness.
- **Task:** Either batch choices (multi-select, MIMO-27) and preview
  once, or refresh incrementally and show the re-scan's counters.
- **Acceptance:** Resolving several conflicts on a large plan performs
  at most one preview per batch and shows progress while it runs.

### MIMO-11: Status is polled; the agent's events are unused

- **Evidence:** The agent publishes `scanProgress`, `opDone`,
  `runFinished` and friends (PLAN.md §4.3), but the UniFFI surface is
  request/response: `SyncStore` polls working pairs every 500 ms,
  `MainViewModel.monitor` reads every pair every 15 s, `MacToMacStore`
  polls two RPCs per second while its tab is open, and jobs the agent
  starts (scheduled, on-change) update only on the 15 s tick
  (`SyncStore.swift:598`), so their progress looks frozen between ticks.
- **Task:** Either carry the event stream through UniFFI (callback
  interface, batched) or make polling adaptive: fast for app-started
  jobs, slower for agent-started ones, backoff when idle.
- **Acceptance:** An agent-started sync updates the app at sub-second
  cadence while it copies; an idle app polls no pair more than once a
  minute.

### MIMO-12: IPC exception paths

- **Oversized responses kill the connection:**
  `foldover-ipc/src/local.rs:185` fails inside `write_value`; the
  client reads "Agent closed before its response" instead of a JSON-RPC
  error. Answer with a proper error reply.
- **One wedged handler can exhaust IPC:** `local.rs:154` holds one of
  32 connection permits across handler execution with no deadline, and
  any `accept` error but permission-denied ends `serve` permanently.
  Bound handler execution and retry transient accept errors.
- **A transient identity failure is cached forever:**
  `foldover-net/src/installed.rs:61` pins `None` on the first probe
  failure, disabling privileged IPC until restart. Cache successes only.
- **Long work as jobs:** `archive.putBackAll`, `endpoint.browse` and
  pairing are single RPCs whose deadlines were fixed in PR 166; giving
  them start/poll job handles (as `run.*` has) would add progress and
  cancel. Larger than the three fixes above; do it when the archive and
  browse surfaces are next touched.

### MIMO-13: Platform hardening

- **`pairing=0` is accepted in mDNS sightings:**
  `foldover-net/src/discovery.rs:267` validates the service port but
  not the pairing port. Reject zero.
- **Local-state allowlist omits filesystems:** `foldover-db/src/
  database.rs:1053` misses ZFS (`0x2fc12fc1`), F2FS (`0xf2f52010`) and
  bcachefs (`0xca451a4e`), refusing legitimate local state storage.
- **A permission-corrupted crash record blocks startup:**
  `foldover-db/src/database/starts.rs:73` hard-fails on wrong uid/mode
  where malformed content is skipped; treat both as unreadable.
- **Keep-copy labels are unchecked off the peer path:**
  `foldover-endpoint/src/local/conflict.rs:33` and `sftp.rs:1251` format
  the `from` label into a sibling name; only `remote/serve.rs:270`
  applies `plain_label`. Current callers pass constants — hardening.
- **Lease writes are uncapped:** `remote/serve.rs:423` writes
  peer-supplied lease content verbatim while reads cap at 4096 bytes.
  Cap before touching disk.

### MIMO-14: Input edges in the app

- **`smb://` URLs from unvalidated hosts:**
  `apple/Foldover/ViewModels/PairEditorViewModel.swift:637` interpolates
  a network-supplied host into a mount URL; `:` or `@` can inject
  credentials into the URL Finder opens. Validate the host shape.
- **`NumberField` ignores its range on typed input**
  (`PairEditorView.swift:424`): clamp on commit, not only in the stepper.
- **Blank skip-rule lines persist as empty rules**
  (`PairEditorViewModel.swift:137` splits without dropping empties).
- **Update checker edges** (`FoldoverSystem/UpdateChecker.swift:134,167`):
  force-unwrapped feed URL literals, and an `async let` left unawaited
  when the feed parse throws.

### MIMO-15: Layout floors that do not fit together

- **Evidence:** `apple/Foldover/Views/MainView.swift:24-34`:
  `windowMinWidth` 820 is smaller than the sidebar maximum 340 plus the
  detail minimum 320 plus the inspector 280 = 940. Settings is fixed at
  520 wide (`SettingsView.swift:97`), the pair editor at `minWidth: 800`
  (`PairEditorView.swift:33`), the SFTP picker at 500x520.
- **Impact:** A wide sidebar at minimum window width squeezes the detail
  under its own floor and clips the inspector; fixed panes crowd scaled
  text on 1280x800 displays.
- **Task:** Raise the window minimum or let the sidebar yield first;
  prefer min/ideal/max over fixed widths (related SOL-15).
- **Acceptance:** `LayoutStressTests` covers the wide-sidebar case too;
  every pane stays above its floor at the window minimum.

### MIMO-16: The detail's status card offers no way to run

- **Evidence:** `apple/Foldover/Views/PairDetailView.swift:150` shows
  action buttons only while working and after completion; a freshly
  previewed plan's only run buttons are in the toolbar and menus.
- **Task:** Put the primary action (Sync Now) in the card beside the
  plan summary; keep the plan sentence's three-line clamp but add an
  expansion or selection affordance (it truncates with a tooltip only),
  and auto-load change pages to a stated cap instead of a small
  "Show More" caption ("and 42 more" is easy to miss).
- **Acceptance:** Preview, then run without leaving the card; long plan
  sentences and large plans state their truncation.

### MIMO-17: The app icon predates the platform's icon style

- **Evidence:** `media-sources/icon.png` and `AppIcon.appiconset` ship a
  full-bleed dark-navy square with no dark or tinted appearances; at
  16 px the folded dart reads as a blob.
- **Task:** Regenerate in the layered icon template with light, dark and
  tinted variants; check legibility at 16 px and on light docks.
- **Acceptance:** Screenshot the Dock in light, dark and tinted modes at
  every icon size.

### MIMO-18: Command surface inconsistencies

- **Sync All stays enabled while the pair editor is open**
  (`App/Commands.swift:160`) although other pair commands withdraw
  through the focused-scene check.
- **The Help menu is replaced away** (`App/Commands.swift:29`), which
  removes the standard menu-item search people use to find commands.
  Append instead of replacing.
- **A bare "q" quits the app** from the menu-bar popover
  (`MenuBarView.swift:44` `keyboardShortcut("q")`); use ⌘Q semantics or
  drop the shortcut.

### MIMO-19: Destructive dialogs should name the folder

The removal dialog does this well ("Remove settings for X?"). "Archive
and Sync", "Put Back All?" and the approval message should name the
pair and its folders, so a reflex tap cannot hit the wrong pair.

### MIMO-20: A Status intent for Shortcuts

`Intents/PairIntents.swift` offers Run, Pause and Preview. An intent
returning the pair's status line ("In sync", "3 conflicts need you")
completes the set and makes automations and Siri genuinely useful.

### MIMO-21: Pause one pair

`PauseLength` pauses automatic syncs globally. A per-pair pause (a
"hold" on one noisy pair without stopping the rest) fits the existing
`Environment`/pause machinery and the schedule model.

### MIMO-22: Optional notification when a long sync finishes

Notifications cover failures and conflicts only, deliberately
(PLAN.md §3.8). An opt-in "tell me when a sync that ran longer than N
minutes finishes" is the one missing alert class in sync tools
(related F-04 and SOL-09 for the notification plumbing).

### MIMO-23: A run detail sheet

The state keeps `run_item` (ADR 0026) and the inspector shows ten
one-line summaries; nothing shows what a past run actually changed.
A read-only change table per run, reusing the preview table's row
model, closes both this and "Older syncs are not listed." (related
SOL-08's run cursor and F-03's export).

### MIMO-24: An opt-in diagnostics bundle

PLAN.md §9 promises "an opt-in diagnostics bundle helps support";
nothing implements it. A "Copy Status" gathering the agent version,
pair states, last errors and recent history into the pasteboard is the
small version.

### MIMO-25: Delight and speed ideas

Small, self-contained, each optional.

- **The menu-bar arrow fills as a run progresses** — the animated icon
  (PR 152) already moves while working; make it carry the byte
  progress too (related SOL-05).
- **Completion card thumbnails** — three Quick Look thumbnails of
  copied images beside "142 items copied", local files only (related
  SOL-19's signature moment).
- **Drop a file on the detail to ask "what would this do?"** — a
  one-path dry run in a popover; a trust builder on the preview promise.
- **A fuzzy pair switcher (⌘P)** — pairs first, archived runs later,
  like Xcode's Open Quickly.
- **A desktop widget** with each pair's one-line status (WidgetKit
  extension; medium size).
- **Keyboard-first conflict triage** — J/K to move, 1/2/B to resolve,
  Space to Quick Look, auto-advance (related SOL-13's comparison cards).
- **An "in step for 12 days" streak** per pair, from run history
  (related SOL-16 and F-05's freshness work).
- **A sync speed sparkline** in the inspector while copying, from the
  rate samples PR 164 already carries.
- **Relative times in the history list** ("2 min ago", absolute in the
  tooltip).

### MIMO-26: Small hot-path notes

Verified shape, small each; batch them when touching the files.

- `MainViewModel.swift:246` sorts `pairs` with `localizedStandardCompare`
  on every access and is read several times per render; cache the order.
- `foldover-endpoint/src/link/stream.rs:199,228` memmoves each received
  frame with `remove(0)` and copies each outbound payload to prepend its
  tag; slice instead.
- `foldover-endpoint/src/sftp.rs:1217,1253` probe free sibling names
  one remote `attributes()` at a time (up to 100 round trips); read the
  parent once.
- `apple/Foldover/App/MenuBarScene.swift:28` recomputes `menuBarStatus`
  (O(pairs)) on every 400 ms animation frame; cache it.
- `SyncStore.swift:802` fetches history when a job ends and again on
  the next changed-state tick.
- `SyncStore.swift:824` writes `states[id]` unconditionally, so equal
  states still invalidate Observation on every poll tick.
- `foldover-endpoint/src/remote/serve.rs:463` sleeps 500 ms on a
  serving thread per change burst; `sftp.rs:491` buffers a whole tree
  listing before processing any of it.

### MIMO-27: Multi-select and bulk actions in the change table

- **Evidence:** The table selects one row (`PairDetailView.swift`
  `Table(rows, selection: ChangeRow.ID?)`) while its context-menu
  closure already takes a set and then acts on `rows.first`; the archive
  window selects sets happily. F-03's export assumes a multi-selection
  that does not exist yet.
- **Task:** Set-valued selection; Skip Once / Always Skip / Keep
  (conflicts) / Copy Paths over the selection, in one request batch per
  action (which also halves MIMO-10's re-preview churn).
- **Acceptance:** Select several rows across kinds and act once; the
  inspector reports the selection size; each action remains
  review-token-bound.

## Confirmed non-bugs from the second pass

Verified against the source so they are not re-reported:

- **`archive_items` empty-page break (CLI):** both endpoints collect the
  whole filtered listing before truncating, so `more` can never be true
  with an empty page; the `after.is_none()` break is dead-code defense.
- **`leave_safe_mode` maps timeouts via `AgentError::from`:** correct —
  resuming is a mutation; the committed warning belongs there.
- **CLI `sync` polls forever:** intentional — syncs legitimately run for
  hours; `--cancel` is the escape and `--progress` shows liveness.
- **Cumulative-average copy rate:** `copied / elapsed` lags on bursty
  runs and after stalls — deliberate honest smoothing, not a bug. An
  EWMA/instantaneous rate is a refinement if the display ever feels slow.
- **`deny_unknown_fields` on `SyncProgress`:** mixed-version client/agent
  pairs reject each other on the new `rate` field; acceptable because
  app and agent ship versioned together and the `core_version` handshake
  already enforces it. Revisit only if the proto serves third parties.
- **Content-filter note:** one engine/filesystem review pass was blocked
  by a provider content filter and a restart returned only low-severity
  style notes; the engine's own simulation and property tests remain the
  authority on that area.

## Original verification record (23 October)

- Source review covered the architecture, service/IPC/CLI paths, engine
  and storage safety paths, endpoints, Swift stores and views, tests,
  limitations, release workflow and performance budgets.
- This was a risk-based review, not an independent audit of every native
  driver binding, signing/cryptographic primitive, generated binding or
  long-running simulation scenario.
- The baseline Linux `cargo xtask ci` passed. All five existing
  performance budgets passed: scan 10k files 620.8 ms, hash 64 MiB
  17.7 ms, plan 100k entries 114.6 ms, DB commit 100k 254.5 ms, and
  DB read 100k 76.3 ms. These are host measurements, not NAS or UI proof.
- The local Markdown checker also traverses pre-existing ignored output
  directories; its reported stale-artifact problems are not source findings.
- Native appearance, frame rate, VoiceOver and physical-device behavior
  were not verified on this Linux host.
- No confirmed data-loss flaw was established in the inspected journal
  and staging paths. SFTP Full-durability restoration is already refused
  by `PairArchive::put_back`; the lower-level omission is not a reachable
  app defect and is not listed as one.
- PRs 159 and 163 completed two GLM rounds without important findings.
  Useful minor feedback was addressed. The remaining CLI singular/plural
  wording nit was deferred under the stopping rule. A suggestion to
  document legitimately missing Unicode destinations was rejected:
  both count and lookup loops use the same immutable candidate vector,
  including unavailable aliases, so that explanation would be false.
- PR 165 also completed two rounds without important product findings.
  Its remaining test-helper cancellation/cleanup nits are deferred;
  the rendezvous already has a three-second deadline. Its independent
  English status-copy assertion stays, consistent with the English-only
  product decision.
- PR 168 completed two rounds without confirmed important findings.
  Large-text viewport feedback was addressed. Its remaining multi-display
  sizing suggestion awaits native verification; speculative non-run
  fixture support and a one-use row factory were deferred.
- Second-pass record (PRs 157, 158, 160, 161, 162, 164, 166): every Rust
  change was verified locally (unit tests, clippy, fmt; real-agent runs
  for the CLI features). Two review findings were rejected with reasons
  in-thread: `--changes` "bypassing the configuration queue" (the pair
  fetch is itself the queue wait, and dispatch was reordered anyway) and
  "the apply clock starts at plan time" (`planned()` fires inside
  `execute()`, after approval, immediately before the copy loop). One
  macOS-only flake (`a_folder_another_mac_shares_syncs_through_it`,
  metadata scan raced a file op in the simulated remote) failed once and
  passed on rerun; a second flake hit `test-agent-registration.py`
  (`Updated agent did not start` under launchd). Both are worth a
  stabilization look if they recur; neither touched shipped code.
- Deferred minor suggestions: `AgentError::write` alias and EWMA rate
  refinement. The consolidation adds a typed progress mode and CLI process
  regressions for streaming, paging and aggregate exits.
- Third-pass record (Mimo.md): each Rust PR reproduced its regression
  before the fix — PR 173's identity assertion on an aborted first run,
  PR 174's notification-level watcher test, PR 175's step bound (the
  unclamped loop reported 2,000,010 steps and failed) — and passed the
  local `cargo xtask ci`. The Swift PRs (176–179) were parse-checked
  with a local toolchain; their Swift Testing regressions run in the
  queued macOS jobs, so native test passage is unverified here.
- Third-pass scan findings were verified against the source before
  listing: MIMO-01 is scan-derived and macOS-only, and the fsync and
  SFTP transfer-volume claims in MIMO-08/09 are read from the source's
  structure, not measured. SOL-06's macOS profiling acceptance and
  SOL-12's native checks remain open with their PRs.
- The consolidation resolves the #158/#161 merge-order dependency through
  shared polling, retaining plain-run progress output.
- Fourth-pass (Muse) record: five Swift-only PRs (167, 169–172) were
  developed on Linux with helper-model review passes; every important
  GLM finding on their latest revisions was applied, and the last review
  of each reported zero actionable suggestions. Tour/macos jobs were
  green or still queued at the time of this merge, except where a PR's
  own table row says otherwise. Engine claims MUSE-01 and MUSE-04 were
  read from source and not reproduced under two-process tests yet;
  MUSE-03's default-verb behavior was read from `sync_command`, not
  executed against a live agent in this pass. Native layout, VoiceOver
  and Increase Contrast were not exercised here. PRs 169 and 170
  duplicated PR 164 and PR 179/168 respectively; their final disposition
  is recorded in the consolidation table above.
- Markdown links/structure and Mermaid checks passed for this document.
  The final revision's native CI and any remaining queued Linux jobs are
  outstanding, not successful or waived.

Verdict: merge the reviewed, bounded improvements after native checks;
next address stalled peer cancellation, wrong-folder selection and stale
recovery state. Then complete large-preview search and progress.

## Fifth review pass (28 October 2026)

A fresh independent pass over the tree at `5019fc1`, written locally as
`Step.md` and never committed. Every blocking and important finding from it
has an open pull request, so none of them is listed as backlog below; the
record of what was found and where it went is kept here so the pass is not
lost. The remaining entries are the pass's nice-to-haves, in the schema used
above, ordered by focus area. Findings that duplicate an entry already in this
document are folded into that entry and cross-referenced rather than repeated.

### Implemented in open pull requests

| Finding | Severity | Pull request |
|---|---|---|
| Three tests fail on filesystems with coarse `st_ctime`, so the Linux gate is red or flaky | blocking | 191 |
| `foldover sync ID --cancel` exits 1 when the cancel succeeded | important | 192 |
| A slow first agent status becomes "not responding, choose Turn On" | important | 193 |
| Sync All in the app runs pairs one at a time | important | 194 |
| The app polls pair statuses one at a time | important | 195 |
| The archive browser has no search | important | 196 |
| The editor stores blank lines as filter rules; `skipRule` builds `/` for an empty path | nice-to-have | 197 |
| The change table's context menu acts on one row of a multi-selection | nice-to-have | 198 |

Verification record for the pass: the Rust-side findings were reproduced
before they were fixed — the three failing tests, the `--cancel` and
`--changes` exit codes against a real `foldoverd` on a private socket, and the
GLM reviewer's follow-ups on 191 and 192. The Swift-side findings could not be
executed here: the host has no Xcode and its Swift toolchain cannot start
(`swiftc: error while loading shared libraries: libncurses.so.6`), so they were
reviewed by hand and their Swift Testing regressions run in the queued macOS
jobs. PR 194's first revision broke the macOS build with a stray closing brace
and was fixed in a follow-up commit; that is the class of error the native jobs
exist to catch.

### General issues — nice-to-have

#### `AgentCapability` has three variants but the agent only ever emits `Sync`

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

#### `ChangeRowsCache` has no builder tag in its key

general issues — nice-to-have

`apple/Foldover/ViewModels/PairSyncViewModel.swift:104-131`. The cache is keyed
by pair plus `store.rowsRevision`. Its own comment warns that every builder
sharing the cache must be distinguishable by those fields, but nothing enforces
it, and only one builder exists today. A second builder over the same source —
bulk-selection rows, a filtered copy for export — would silently receive the
first builder's rows.

Suggested fix: add a builder identity to `RowsSource` so a collision cannot
compile.

#### Two unreachable UTF-8 fallbacks

general issues — nice-to-have

`apple/Foldover/ViewModels/PairSyncViewModel.swift:914-917` and `:675-696`.
`String(data:encoding:.utf8) ?? String(decoding:as:)` can never take the
fallback, because the second initializer substitutes replacement characters
rather than failing. The code reads as if invalid UTF-8 is handled when it is
not; `PairEditorViewModel.pathText` has the correct form with an explicit
message.

Suggested fix: drop the dead fallback, or use the editor's form and say the path
contains non-UTF-8 bytes.

#### `InfoButton`'s popover has no height limit

general issues — nice-to-have

`apple/Foldover/Views/InfoButton.swift:20-36`. The popover is `frame(width: 360)`
with `fixedSize(horizontal: false, vertical: true)`, so its height grows with the
note. The "Skip" and "Folders" notes run past 500 characters; at an accessibility
text size the popover can grow taller than the display and lose its content
off-screen.

Suggested fix: wrap the message in a `ScrollView` with `.frame(maxHeight: 360)`.

### Performance problems, stuttering, and jank — nice-to-have

#### Put-back of many items is quadratic on the main thread

performance problems, stuttering, and jank — nice-to-have

`apple/Foldover/ViewModels/ArchiveViewModel.swift:121-124`.
`rows.compactMap { row in store.items.first { $0.path == row.id } }` is
O(rows × items). Selecting the whole 1,000-item page does a million comparisons
before the request is even sent, stalling the main thread.

Suggested fix: build a `[Data: ArchivedItem]` dictionary once per page and look
up by row id.

#### Settings polls the agent twice a second while it is open

performance problems, stuttering, and jank — nice-to-have

`apple/Foldover/Views/MacToMacView.swift:102`,
`apple/Foldover/Stores/MacToMacStore.swift:162-189`. `MacToMacView`'s `.task`
starts a 1 Hz loop that issues two IPC round trips per pass. The task belongs to
the Settings window, so the General tab keeps polling Mac-to-Mac state and
shared folders for as long as the window is open — two agent round trips and a
timer wakeup per second on a window that is usually idle.

Suggested fix: monitor only while the Mac-to-Mac tab is selected, and stretch
the interval to 2–3 s; pairing codes are not sub-second events.

#### The change table re-filters every loaded row on each keystroke

performance problems, stuttering, and jank — nice-to-have

`apple/Foldover/ViewModels/PairSyncViewModel.swift:556-560`. `listedRows.filter`
runs over every downloaded page on each keystroke. The row build is cached, but
the filter is not, so a 1,000-row listing costs 1,000 case-insensitive substring
searches per character typed.

Suggested fix: cache the filtered result against `(searchText, rowsRevision)` the
way `ChangeRowsCache` caches the build, or debounce the search by about 100 ms.

### Missing features — nice-to-have

#### Archived items cannot be looked at before they are put back

missing features — nice-to-have

`apple/Foldover/Views/ArchiveView.swift:165-170`. The only command is "Put Back".
A user recovering a file cannot preview it, confirm it is the version they want,
or see where it came from. For a local side the archived copy is reachable at
`.foldover/archive/<run>/<path>`, so Quick Look and "Show in Finder" are both
implementable without new agent support.

Suggested fix: offer Quick Look and "Show in Finder" for rows on a local side,
built from the pair's root plus the archive path.

#### Archive runs carry no size, so the right one is found by opening each

missing features — nice-to-have

`apple/Foldover/ViewModels/ArchiveViewModel.swift:37-41`,
`apple/Foldover/Views/ArchiveView.swift:85-99`. The run list shows only a title.
A pair with thirty runs forces the user to open them one at a time to find the
sync that archived the file they want.

Suggested fix: show item count and total bytes per run. `ArchiveRun`
(`crates/foldover-proto/src/local/archive.rs:24-31`) needs two more fields, which
the agent can sum while it lists the run's folder.

#### The editor accepts the same folder on both sides until Save

missing features — nice-to-have

`apple/Foldover/ViewModels/PairEditorViewModel.swift:80` and `:130-143`. The
agent rejects identical and nested roots
(`crates/foldover-types/src/pair.rs:98`,
`crates/foldover-engine/src/configuration.rs:641`), but the editor has no local
check, so picking the same folder twice shows no warning and the failure only
appears after clicking Save Pair.

Suggested fix: add a `foldersWarning` that names the problem and include it in
`canSave`, mirroring `dailyWarning`.

#### Older archive runs stay unreachable

missing features — nice-to-have

Folded into SOL-08 above. `ARCHIVE_RUN_LIMIT` is 1000 with a `more` flag but no
cursor in `ArchiveKey`, so a pair that syncs often enough exhausts it; the app
surfaces it as "Older syncs are not listed."
(`apple/Foldover/Views/ArchiveView.swift:90`).

### Visual issues and layout problems — nice-to-have

#### Attention is red in the menu bar and orange everywhere else

visual issues and layout problems — nice-to-have

`apple/Foldover/Assets.xcassets/MenuBarArrowAttention.imageset/arrow.svg`
hardcodes `fill="#ff3b30"`, while `.orange` is used in
`apple/Foldover/Views/PairSidebar.swift:125` and `:143`,
`apple/Foldover/Views/PairDetailView.swift:137` and `:335`, and
`apple/Foldover/Views/MenuBarView.swift:127`. The same semantic state reads as an
alarm in the menu bar and as a caution in every window, and the palette is not
stated anywhere so the two cannot be reconciled by inspection.

Suggested fix: recolour the attention asset to the system orange and record the
semantic palette in one place.

#### The app icon is a set of rasters with no macOS 26 vector asset

visual issues and layout problems — nice-to-have

`apple/Foldover/Assets.xcassets/AppIcon.appiconset/Contents.json` — see MIMO-17.
Ten PNGs from 16 px to 1024 px and no `.icon`/Icon Composer asset, so the icon
cannot take the platform's current glass and shadow treatment and scales less
crisply than a vector one. The menu-bar arrows are vector SVG templates, so the
inconsistency is visible in the same asset catalog.

Suggested fix: author the icon as an Icon Composer `.icon` asset and keep the
rasters only as a fallback.

### Aesthetics and theming — nice-to-have

#### The semantic palette is scattered across nine files

aesthetics and theming — nice-to-have

`apple/Foldover/Views/PairDetailView.swift:331-338`,
`apple/Foldover/Views/PairSidebar.swift:140-154` and `:208-215`,
`apple/Foldover/Views/MenuBarView.swift:127`,
`apple/Foldover/Views/ArchiveView.swift:202` and the menu-bar asset above. Orange
means attention in five places, red means failure in seven, green means connected
in one, and the menu bar uses a hardcoded `#ff3b30`. There is no single
definition to check a new surface against, so drift is invisible until someone
compares two windows side by side.

Suggested fix: introduce one semantic-colour enum with `attention`, `failure`,
`connected` and `working`, and route every call site through it. See SOL-14.

#### Freshness colouring is the app's best idea and is underused

aesthetics and theming — nice-to-have

`apple/Foldover/Views/PairSidebar.swift:104-110` and `:134-154`. The sidebar's
three-tier freshness tint — primary within an hour, secondary within a day, soft
orange beyond — is the quietest, most useful trust signal in the app, and it
exists nowhere else. The menu bar's pair rows and the inspector's "Recent Syncs"
both show the same age as flat grey text.

Suggested fix: reuse the same tier function for the menu bar's pair status and
the inspector's history rows, so "how stale is this?" reads the same way in all
three surfaces.

### Usability — nice-to-have

#### Sync All gives no feedback about what is queued or finished

usability — nice-to-have

`apple/Foldover/ViewModels/MainViewModel.swift`, `apple/Foldover/App/Commands.swift:160-162`,
`apple/Foldover/Views/MenuBarView.swift:34-36`. The button and the menu item
disable while any pair cannot sync, then go silent. Nothing says how many pairs
are running, how many are waiting, or when the last one finished, so a user who
clicks Sync All and switches away has no way to tell whether it is still working.

Suggested fix: carry a small aggregate state on `MainViewModel` ("Syncing 2 of
5") and show it beside the menu bar's status line. This falls out of the
concurrency fix in pull request 194.

#### Space for Quick Look is undocumented

usability — nice-to-have

`apple/Foldover/Views/PairDetailView.swift:306-310`. The change table handles
Space as Quick Look, as Finder does, but no menu item, help tag or column header
mentions it. The Pair menu's "Quick Look" has no shortcut, so the only
discoverable path is the context menu.

Suggested fix: put a shortcut on the Pair menu's "Quick Look" item and add a help
tag to the table naming the key.

#### The first-run guide's third step can never complete

usability — nice-to-have

`apple/Foldover/ViewModels/MainViewModel.swift:667-672` and `:644`. Step 3 is
hardcoded `.upcoming`, and `showsSetup` requires `pairs.isEmpty`, so the guide
disappears the moment a pair exists. The row reads as unfinished work that was
never finished.

Suggested fix: either drop the third step, or mark it informational with no
progress badge.

### Novel, delightful, or quirky ideas worth trying — nice-to-have

#### A one-shot completion pulse on the menu-bar icon

novel, delightful, or quirky ideas worth trying — nice-to-have

`apple/Foldover/ViewModels/MenuBarAnimation.swift:41-64`. The working animation
already cycles four fold-arrow frames at 400 ms and respects Reduce Motion by not
running at all. A single two-frame "settle" when a run finishes would give the
menu bar the same completion beat the main window's completion card has, without
adding a permanent animation. Reuse `MotionStore.preference` so Reduce Motion
users get a static swap.

#### Archive run rows as a timeline of the pair's recent life

novel, delightful, or quirky ideas worth trying — nice-to-have

`apple/Foldover/Views/ArchiveView.swift:85-99`. The run list is a plain list of
titles. Because every run already has a start time and, with the size fields
above, a byte total, a compact horizontal bar per run would turn the archive into
a readable history of how much the pair moved and when — the same story the
inspector's "Recent Syncs" tells in words.

#### Let the plan sentence be the shareable artefact

novel, delightful, or quirky ideas worth trying — nice-to-have

`apple/Foldover/ViewModels/PairEditorViewModel.swift:332-339`,
`apple/Foldover/Views/PairEditorView.swift:363-369`. The editor rewrites
"Changes in either folder go to the other; a file deleted in one folder is
archived in the other. Syncs every 15 minutes." as settings change, which is the
clearest explanation of a sync pair anywhere in the product. Offering "Copy
Summary" on that sentence, and showing the same sentence in the completion
notification, would let a user paste an accurate description into a message to
whoever else uses the folders.

#### Reveal the archived file from the change table

novel, delightful, or quirky ideas worth trying — nice-to-have

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
