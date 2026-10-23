# Foldover improvement backlog

What this repo does: Foldover previews and syncs two folders through a
single Rust agent, with a native macOS interface. Backup is additive;
Mirror also removes target-only content; Sync reconciles both sides.
Archives provide short-term recovery on each endpoint.

Reviewed 2026-10-23, starting at `8fb2405`. Assumed load: one person,
several overlapping jobs on disjoint roots, 100k to 1M tracked files,
and occasional multi-gigabyte files over slow or interrupted links.
The initial review was written in `Sol.md` before implementation. Source
references describe that baseline unless a PR status says otherwise.

## Start here

Preserve the single-writer engine, review-token approvals, endpoint-local
archives and existing layer boundaries. Reproduce a reported bug before
fixing it. Keep visual proposals separate from measured defects.

| Priority | Work | Acceptance boundary |
|---|---|---|
| P1 | SOL-21: cancel stalled peer streams | Stalled peer, slow healthy peer and idle watch |
| P1 | SOL-26: honor the selected SFTP parent | The saved root equals the highlighted folder |
| P1 | SOL-03: reject stale archive replies | Delayed success, error and cleanup cannot change a newer listing |
| P1 | SOL-04: search the complete preview | Find late matches without downloading earlier pages |
| P2 | SOL-23: index protected ancestors | Same safety decisions, bounded work with many failures |
| P2 | SOL-24 and SOL-08: complete, bounded archive browsing | Old runs and late items remain reachable without full rescans |
| P2 | SOL-05 and SOL-06: visible progress and responsive previews | Large transfers advance; search avoids repeated formatting |
| P2 | SOL-09, SOL-10 and SOL-16: visible, actionable attention | Background failures, restore causes and freshness are explicit |
| P3 | SOL-13–15 and SOL-17–20: product and visual polish | Validate with native light/dark, keyboard and accessibility checks |

## Implemented in open PRs

These remain unmerged for the owner's review. Do not implement duplicate
patches. Remove their detailed entries once merged, retaining any stated
follow-up work. No implementation in this table was pushed into main.

| Finding | Branch | PR | Verification |
|---|---|---|---|
| SOL-01 | `sol/cli-results` | [159](https://github.com/L-K-M/Foldover/pull/159) | Real-agent red/green regressions; CLI tests and clippy |
| SOL-22 | `sol/unicode-planning` | [163](https://github.com/L-K-M/Foldover/pull/163) | 50k names: 11.28 s before, 0.094–0.165 s after; 135 engine tests |
| SOL-02 | `sol/archive-state` | [165](https://github.com/L-K-M/Foldover/pull/165) | Held-reply regression failed first; 12 archive tests pass in a portable source harness |
| SOL-12 | `sol/menu-bar-controls` | [168](https://github.com/L-K-M/Foldover/pull/168) | Swift syntax, docs and layering; native routing/UI tests await macOS |

All four PRs completed two GLM review rounds without confirmed important
findings. Completed Linux checks passed; some final-revision CI jobs are
still queued. Check the exact PR head before merging.

Native app builds, UI tours and rendered layout remain unverified while
the `xcode-27` CI jobs are queued. Portable archive checks use the actual
store and view-model sources with unused FFI and formatting shims; they
do not establish native integration or visual correctness.

## Must fix

### SOL-01: CLI success can mean an incomplete operation

Status: implemented in PR 159; owner merge pending.

- **What this is:** Scripts rely on exit status to decide whether a sync
  or recovery finished successfully.
- **Problem:** `crates/foldover-cli/src/main.rs:839-855` returns success
  for a finished run with conflicts. Its archive command at lines 932-937
  returns success even when `failure_count` reports failed restores.
- **Fix:** Keep the JSON report, return failure for partial restores,
  and give unresolved sync conflicts a documented nonzero exit status.
- **If skipped:** Scheduled scripts can announce a successful backup or
  recovery while files still need attention.
- **Acceptance:** Exercise the real CLI and agent with divergent edits,
  successful restoration, and a restore blocked by a file occupying its
  parent directory. Check both exit codes and retained file contents.

### SOL-02: Put Back All does not reserve the archive UI

Status: implemented in PR 165; owner merge pending. SOL-03 is separate.

- **What this is:** Selected-item restoration already reserves one batch
  with `isPuttingBack`.
- **Problem:** `apple/Foldover/Stores/ArchiveStore.swift:161-174` neither
  checks nor sets that flag. A second whole-run restore, or a selected
  restore, can overlap it. The agent rejects competing mutations, leaving
  avoidable errors in the recovery interface.
- **Fix:** Share the existing reservation across both restore commands,
  hold it through the post-restore reload, and show that recovery is busy.
- **If skipped:** Repeated clicks produce inconsistent feedback during
  the operation where users most need certainty.
- **Acceptance:** Hold a fake client's whole-run restore; both restore
  buttons must disable and duplicate calls must send no second request.
  Test failure and recovery of the reservation too.

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

### SOL-06: Preview rendering repeatedly builds every loaded row

- **What this is:** `PairSyncViewModel` is a value recreated for rendering.
- **Problem:** Its `listedRows` at lines 404-442 formats paths, sizes,
  descriptions and Quick Look URLs for every loaded row. `rows`,
  `placeholder`, `showsSearch` and selection consumers request this work
  independently. Paging many rows moves increasing work onto the main
  actor. This is a source-level hotspot, not a measured frame-rate claim.
- **Fix:** Cache presentation rows by pair revision, review token and
  listing generation; cache search results independently. Profile before
  replacing the native Table with an outline implementation.
- **If skipped:** Search and selection become increasingly expensive as
  the user reviews more of a large plan.
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

## Nice to have: interface and product improvements

### SOL-12: Keep the menu bar usable with many pairs

Status: implemented in PR 168; native validation and owner merge pending.

- **What this is:** `MenuBarView.swift:18-28` puts every pair in an
  unbounded vertical stack above the controls.
- **Problem:** A larger catalog can push Pause, Settings and Quit beyond
  the available display height. Busy rows offer a disabled sync button
  instead of an immediate Stop action.
- **Fix:** Bound and scroll only the pair list, keeping the header and
  commands visible. Replace Sync with Stop while a cancellable job runs.
- **If skipped:** The quick-control surface requires opening the main
  window precisely when the user wants to stop activity quickly.
- **Acceptance:** A 40-pair fixture fits a small display; footer controls
  remain reachable by keyboard and Stop targets only its own pair.
- **Native follow-up:** Check large text on mixed-height displays. The
  implementation uses `NSScreen.main` and a 352-point list ceiling; a
  review suggested using the smallest attached display or the actual
  popover's display. Verify that behavior before choosing the final
  multi-monitor policy. Scroll-position persistence is optional polish.

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

### SOL-22: Unicode respelling performs quadratic duplicate checks

Status: implemented in PR 163; owner merge pending. Other protected-path
indexing costs remain in SOL-23.

- **Evidence:** `crates/foldover-engine/src/run.rs:1433-1448` scans all
  respelled paths again for every candidate destination.
- **Impact:** 50k normalized spelling changes require roughly 2.5 billion
  comparisons before a plan can be shown.
- **Task:** Count destination multiplicity once, then apply only unique
  mappings. Preserve ambiguity protection for both readable and
  unavailable entries. Do not change case-only rename behavior.
- **Acceptance:** Existing normalization safety cases pass; add a large
  respelling benchmark and ambiguous-destination regression coverage.

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
- **Acceptance:** Remove a pair with loaded pages and an outstanding
  request; its state stays forgotten after that request completes.

## Verification and implementation record

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
- Markdown links/structure and Mermaid checks passed for this document.
  The final revision's native CI and any remaining queued Linux jobs are
  outstanding, not successful or waived.

Verdict: merge the reviewed, bounded improvements after native checks;
next address stalled peer cancellation, wrong-folder selection and stale
recovery state. Then complete large-preview search and progress.
