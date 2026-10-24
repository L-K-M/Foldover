# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- An aborted first sync still requires review on retry. Editing a pair's
  folders re-subscribes its change watchers. Peer scan counters cannot
  claim unbounded work in one progress report.
- Put Back All reserves the archive through its reload and shows recovery
  activity. Periodic health probes keep pair commands enabled.
- Unicode spelling preparation counts aliases once, and preview rows are
  cached across renders and searches.
- Remote requests, including archive listings, get longer client timeouts.
  Read timeouts invite retry; saves retain the uncertain-commit warning.
- The menu-bar pair list scrolls while commands remain reachable. Its
  error captions are bounded and selectable, and busy pairs offer Stop.

- Unreadable or changing files, folders and ambiguous AppleDouble sidecars
  no longer stop independent, safe copies. Preview and sync results list
  errors with readable paths; affected branches stay unchanged in both
  folders and retry next sync. Archive retention still runs after a partial sync.

- Agent connection checks time out and keep Turn On available. Starting and
  restarting show their own status, and Turn On also repairs an agent that
  rejects an updated app's handshake.

- Sockets, named pipes and device files no longer stop local scans, so a
  home folder containing 1Password's daemon socket can sync. These paths
  are skipped in both folders, preserving existing copies.

### Changed

- CLI syncs with unresolved conflicts and no failures now exit with
  status 4 instead of 0. Partial `--put-back-all` restores exit with
  status 1 instead of 0 and still print the restore report as JSON.
  Failed items stay archived; sync failures take precedence over conflicts.

- The app is laid out like a Mac app: pairs in a sidebar with their
  status, the selected pair's folders, plan and a table of planned
  changes in the middle, and an inspector for the selected change, the
  pair's settings and its recent syncs. Preview, Sync Now and Stop sit in
  a customizable toolbar and a new Pair menu (⌘R, ⌘S, ⌘.), with every
  other command in the menus, context menus and the Delete key.
  [Using the app](docs/guide/using-the-app.md) lists them.

- The archive opens in its own window per pair instead of a sheet, so it
  can stay open while you work, and puts back several items at once.

- The pair editor, Settings and the menu bar extra are more compact and
  fit small screens; the pair editor's Save button is always visible.

- The pair editor shows both folders side by side, with the sync mode
  and direction between them.

- Info buttons keep optional guidance out of the pair editor, Settings,
  inspector and archive browser, with setup steps and tradeoffs on demand.

- A pair or shared folder may be your home folder — or any folder that
  holds Foldover's state directory. The state directory is left out of
  scans, refused to reads, writes and paired Macs, and protected from
  removals or replacements of the folders above it; its own bookkeeping
  writes never count as changes. A root inside the state directory is
  still rejected.

- Check for Updates… and the check the app makes when it opens now ask
  GitHub for the newest release and link to its download page instead of
  updating in place; the Sparkle updater and its signing keys are gone.
  A beta install hears about the next beta — and the stable release that
  ends the line — while a stable install is only ever told about stable
  releases. Version strings now follow strict semantic versioning:
  leading zeros such as `1.02.3` are rejected by the release pipeline
  and by the update check.

- Time Machine no longer backs up Foldover's state on this Mac: a
  restored, older state could carry stale deletions over. A Mac set up
  from a backup starts without pairs.

### Added

- CLI help, live `sync ID --progress`, complete filtered preview listings
  with `--changes [KIND]`, and bounded concurrent `sync --all`.
- Agent-measured copy throughput and ETA, labeled progress indicators,
  sidebar freshness tints and relative archive dates.

- Edit › Undo reverses the last Sync Now from the window, named after
  its pair. Edit › Find filters the list of changes, Edit › Copy copies
  the selected change's path, and Space shows its file in Quick Look.
  The window remembers the selected pair and whether the inspector is
  shown.

- The Pair menu also pauses and resumes automatic syncs and reviews the
  conflicts a sync left, which only buttons offered before.

- A pair can limit how fast it copies, in megabytes per second, so a
  large sync leaves room on the network for everything else.

- Files copy four at a time between solid-state disks, mounted shares,
  SFTP servers (over several sessions) and other Macs. Spinning disks,
  disks that do not say what they are, and pairs between two SFTP
  servers still take one file at a time.

- A file moved or renamed in one folder is moved the same way in the
  other instead of being copied again and archived; the preview lists it
  as a move, and Undo moves it back. Folders on another Mac still copy.

- The pair editor browses folders on an SFTP server, so you can walk to
  the folder to sync instead of typing its path.

- Names reach SFTP servers in composed Unicode (NFC), as Linux and
  Windows expect, even from a disk that stores them decomposed, and
  Foldover finds a server's file in either spelling.

- A background agent that stops unexpectedly three times in five minutes
  pauses all syncing instead of crashing again and again; the app shows
  why and notifies you, and Resume Syncing (or `foldover safe-mode off`)
  starts again.

- A Backup can go to a folder that cannot hold Foldover's `.foldover`
  folder, such as a share that refuses names starting with a dot, once
  you choose to keep nothing there: files replaced in it are deleted,
  not archived. The pair editor says when a folder refuses it.

- `scripts/build.sh --install` installs the app it built into
  `/Applications`, quitting and reopening a running Foldover, which
  restarts the background agent so both use the new build.

- "When files change" catches up on changes made while the agent was
  not running, syncs after 5 minutes of nonstop changes, and checks
  shares and SFTP servers for other computers' changes every 5 minutes.
  It now waits for 10 seconds of quiet instead of 5.

- Simulation over in-memory folders that act like the file systems
  Foldover meets: names that ignore case or are stored decomposed,
  2-second times, two-step replaces, a full disk, and writes that start
  failing mid-run. Each night it runs 100,000 random histories.

- At start, "When files change" replays each local folder's file event
  history from when the pair was last quiet, and syncs only folders
  that changed while the agent was not running.

- Signature moments: drop one or two folders on the window's sidebar or
  empty detail to start a pair with them; the pair editor shows a
  sentence that says what the pair will do and rewrites itself as you
  change settings; a search field narrows the preview's change list; and
  a finished sync offers a card to review the conflicts it left or undo
  what it changed.

- Guides: what each folder kind can do, setting up NAS shares and SFTP
  servers, and current limitations.

- Servers nearby: the pair editor lists the SMB file servers Bonjour
  finds on your local network, and Connect opens one in Finder's
  Connect to Server so you can mount a share and pick it. The same list
  prints as JSON lines with `foldover servers`.

- Pair two Macs: open pairing on one, connect from the other, and
  confirm the same six-digit code on both (`foldover peers`). Revoking a
  Mac forgets it. Syncing folders between paired Macs comes next.

- Readiness warnings while you edit a pair (also `foldover pairs
  check`): a folder another sync engine manages (iCloud Drive, Desktop &
  Documents in iCloud, or a provider folder such as Dropbox), a side
  that cannot keep the extended attributes you chose to sync, and two
  sides that compare names differently, one ignoring case and one exact.
  Warnings never block saving.

- Pair two Macs: open pairing on one, pick it on the other from the Macs
  found on your network, and confirm the same six-digit code on both
  (Settings › Mac-to-Mac, or `foldover peers`). Share folders with a
  paired Mac there, and pick them in the pair editor with Another Mac…. Forgetting a Mac revokes
  it. Share a folder with a paired Mac (`foldover shares`), and list what
  a paired Mac shares with yours (`foldover peers shares`). A pair's
  folder can be one a paired Mac shares (`peer://DEVICE/SHARE`): syncs
  run through that Mac, which stages, verifies and archives in its own
  folder, and "When files change" pairs run when that folder changes.
  Tags, Finder info and other extended attributes, resource forks and
  file flags travel between Macs.

- A large copy over the network that breaks off, from another Mac or an
  SFTP server, continues where it stopped the next time it runs. Copies
  from SFTP servers read each file once instead of twice.

- VoiceOver reads pairing codes digit by digit and says which setup
  steps are done.

- Quick Look a change's file from the preview list with Space or its
  context menu. Preview with ⌘R and stop a running sync with ⌘.

- Check for Updates… in the app menu. Updates arrive through Sparkle, and
  each one must carry the release key's signature.

- Two Macs never sync the same folder at once: a sync waits while
  another Mac's sync of that folder is running, and takes over from one
  that stopped responding for two minutes.

- Names that one folder would store as one file, such as `Report.txt`
  and `report.txt` on a case-insensitive Mac volume, are a conflict to
  rename instead of failing on every sync.

- Live progress while comparing and syncing: items checked, changes made
  and bytes copied, with a progress bar in the app.

- Per-item overrides in a preview: copy a change the other way, restore an
  item a sync would archive, or skip a change for one sync.

- Undo the last sync: what it created moves to the archive, and what it
  replaced or archived comes back. Anything changed since is left as it is
  and listed. Automatic syncs then wait until you sync again.

- A pair's first sync always shows its preview first; nothing is written
  until you approve that plan.
- A folder that is suddenly empty no longer empties its partner without
  review, however few files it held.
- A sync stops at once when a folder's disk or share goes away or is
  swapped mid-run, instead of failing file by file.

- The Mac stays awake while a preview scans or a sync runs; sleeping on
  purpose still works.
- Native app icon from the supplied artwork, at every macOS asset size.
- Keychain-backed peer identity loading, with stable certificate fingerprints,
  exclusive creation and explicit storage failures; agent integration is pending.
- Peer transport driver: pinned QUIC/TLS connections, six-digit pairing
  codes and live-connection revocation. Mac-to-Mac sync integration is pending.
- SFTP folders: either side of a pair can be `sftp://host/path`, reached
  with your SSH keys and configuration, with the same staging, archive and
  conflict rules as local folders.
- NAS shares that went away are mounted again before a sync, with the
  password saved in the Keychain and no dialog.
- Pause automatic syncs for an hour, four hours or until resumed, from the
  menu bar, Shortcuts or `foldover pause`; the pause survives restarts.
- Automatic syncs wait while Low Power Mode is on, unless "Sync
  automatically in Low Power Mode" is on in Settings
  (`foldover automation low-power run`), and syncs that reach folders
  over the network wait while the network is expensive or Low Data Mode
  is on. Syncs you start yourself always run.
- Every previewed change can be listed, 100 at a time, and filtered to
  copies, updates, archives or conflicts.
- Skip large files: a size limit per pair, in the editor and as
  `--max-size MEGABYTES`; a file over it in either folder is skipped in both.
- Skip old files: an age limit per pair, in the editor and as
  `--max-age DAYS`; a file is skipped while unchanged that long in both
  folders, and syncs again once edited.
- Canary file: name a file a pair must find unchanged in both folders, in
  the editor and as `--canary PATH` (`none` clears it). If the file is
  missing or changed on either side, the sync stops before writing
  anything, catching a wrong folder or a disk that never mounted.
- First-run guide: turn on the agent, create a pair, preview and sync.
- A folder macOS refuses to share (Documents, external volumes) now says
  to allow Foldover in Full Disk Access; Settings opens that list.
- Always Skip on a preview row adds a filter rule for exactly that path.
- Previews show how much data a sync copies, in total and per file.
- Shortcuts actions: Sync Pair and Preview Pair, answered with what changed.
- Settings window with "Open Foldover at login", so the menu bar icon and
  notifications are there after a restart.
- Keep Both for files changed in both folders: Folder B's version is
  saved beside the file as "name (from Folder B, date)".
- Automatic syncs wait for a disconnected disk or share instead of failing,
  and resume once it is back.
- Run history: each pair's last 100 syncs with their outcome and counts,
  shown in the app and printed by `foldover history`.
- Archive browser: list each folder's archived syncs and put items back,
  one at a time or a whole sync at once, beside the current version when
  the original path is taken.
- Free-space gate: a sync stops before writing when a folder's disk cannot
  hold the files copied to it plus a 256 MiB reserve.
- Releases: pushing a version tag publishes the ad-hoc-signed app as a GitHub
  release with install instructions.
- Menu bar: every pair's status with Sync and Sync All; notifications when an
  automatic sync needs approval, fails, or leaves conflicts.
- Daily schedules: a pair can sync at a time of day on chosen weekdays in
  the Mac's local time zone, like "Weekdays at 18:00" or "Every day at
  02:30". Choose Daily in the pair editor and set the time and days, or
  use `--schedule daily@18:00:mon-fri`. A schedule missed while the Mac
  slept or the agent was off runs once when they are back.
- Filters: gitignore-style rules per pair and in `.foldoverignore` files,
  applied to both folders so excluded files are never deleted.
- Sync when files change: pairs set to "When files change" watch both
  folders and sync after 5 seconds of quiet, plus hourly.
- Faster repeat syncs: files whose complete stat is unchanged since the last
  observation are not reread.
- Conflict resolution: keep A or keep B for a previewed file conflict; the
  other version is archived, and choices expire if either side changes again.
- Automatic sync on an interval, set per pair in the app or with
  `--schedule`; it starts after the first manual sync and stops instead of
  asking when deletions need approval.
- 30-day archive retention: archive folders are named by run start time,
  and runs older than 30 days are deleted after each completed sync.
- Preview and Sync Now in the app, with the change list, results, cancellation
  and a confirmation before archiving many items.
- Sync from the command line: `foldover sync ID` previews and runs as agent
  background jobs, with deletion approval and cancellation.
- Folder identity check: the first sync writes `.foldover/id` into both
  folders, and later runs stop if either is missing or different, such as an
  empty folder left at the path of an unmounted disk.
- Sync runs in the engine: observe, plan, gate deletions, copy and archive in
  safe order, and commit the base in revision-bound batches; previews list
  every change, conflict and blocked path.
- Endpoint operations that execute one planned change: copy, metadata update
  and removal, each verifying both sides against the preview first and
  returning the stored version; leftover temporaries are reported and archived.
- Local write primitives: staged copies placed by atomic no-replace or
  exchange renames, archive-by-rename, metadata applied per pair policy and
  durability; nothing is unlinked except empty folders and unplaced copies.
- Per-pair options in the app, CLI and agent API: sync modification times,
  extended attributes and permissions independently, and choose standard or
  power-loss-safe durability. IPC protocol version 3.
- Per-pair base storage (state schema v2) with revision-bound atomic updates;
  root changes and pair removal discard it.
- Canonical local entry versions: content (data plus resource fork, or link
  target) and a digest of the metadata categories a pair syncs (modification
  times, extended attributes, permissions), observed for whole trees.
- Held-volume filesystem feature declarations, with explicit supported,
  unsupported and unreported states; operation fidelity remains separately gated.
- Bounded held-root xattr/resource-fork digests, native ACL observations and
  stat fidelity fields, with cancellation and explicit unsupported metadata errors.
- Revision-checked pair removal in the app and CLI, including offline pairs,
  while preserving endpoint folders and files.
- Held-root macOS volume UUID observations, with physical-device ancestry and
  validated attribute replies; mount-continuity/readiness integration remains pending.
- Explicit non-materializing LaunchAgent policy, with native process/thread/child
  inheritance fixtures; account-backed File Provider verification remains pending.
- Enforced macOS non-materializing thread policy around filesystem traversal
  and hashing, isolated behind audited driver bindings and native tests.
- Shrinking property tests for hierarchy-aware planning over independently
  edited raw-byte trees, with persisted failure seeds.
- Deterministic tree-plan candidates, with hierarchy validation, conflicted-branch
  blocking, per-target deletion review and cancellation; no execution API yet.
- Exact interactive/unattended deletion-review thresholds, with validated
  counts and finite-model boundary checks; execution integration remains pending.
- Darwin dataless-file observations, with explicit scan/hash refusal for known
  placeholders and no inferred residency on other platforms.
- Native pair sidebar, folder selection and revision-checked settings editor,
  with paged loading through UniFFI and preserved drafts on save failures.
- Authenticated local pair create/read/update APIs, paged lists and CLI commands,
  with cross-pair/state-root overlap checks and revision-checked updates.
- Bounded BLAKE3 reads of observed local data forks, with cancellation,
  no-follow access, change detection and protected endpoint paths.
- Mutual installed-code authentication for private macOS IPC, bound to kernel
  audit tokens, Hardened Runtime and a process-pinned verified bundle.
- Streaming local folder metadata inventory, with protected internal paths,
  cancellation and explicit errors for changed or unsupported entries.
- Typed local pair configuration and SQLite WAL persistence, with exclusive
  agent-owned writer locks, compatibility guards, migration backups and revision checks.
- Health-only background agent, typed Unix-socket client, `foldover status`,
  native registration/status controls, and embedded ad-hoc-signed helpers.
- Read-only local metadata access through held directory handles, with
  no-follow traversal, exact name/link-target bytes and identity-based root-overlap checks.
- Validated root-relative sync paths that preserve exact name bytes and
  reject traversal, rooted paths and ambiguous components.
- Three-way entry classification for Sync, Mirror and Backup, with
  per-side metadata baselines and finite-model safety checks.
- Competitor research and the product plan ([#1](https://github.com/L-K-M/Foldover/pull/1)).
- Fleet conventions: README, AGENTS.md, CLAUDE.md, the GLM review
  workflow and documentation checks ([#2](https://github.com/L-K-M/Foldover/pull/2)).
- P0 foundations: Rust workspace with crate skeletons and a layer check,
  macOS app skeleton bridged to the core through UniFFI, build and release
  scripts, CI.

### Fixed

- The background agent starts again after the app is replaced: Foldover
  registers it anew when it opens as a changed build, when a status check
  finds it unreachable while turned on, and Turn On does when the agent
  does not answer. macOS kept the agent registered to the previous build
  and would not start the new one, so it stayed unavailable ("Connection
  refused").

- Files synced to a network share no longer get macOS's quarantine
  mark (Gatekeeper's note that a file was downloaded) written there,
  nor do files in a pair that syncs permissions but not extended
  attributes, where copying the ACL carried it.

- A folder holding files iCloud keeps only in the cloud no longer stops
  every sync: those files and folders are skipped in both folders until
  you download them, and never taken for deletions.

- An edit that kept a file's size and modification time is found within
  a week: each file's content is read again at least weekly, spread
  over the week, instead of only when its size or time changes.

- Syncing to disks formatted HFS+ (Mac OS Extended), exFAT or FAT32
  works: every copy to them failed with "Operation not supported".

- A file copied to a folder that stores names decomposed, such as an
  HFS+ disk, no longer makes the next sync rename it on the other
  folder.

- A name in conflict now blocks copying its other Unicode or case
  spelling into a folder that stores both as one file; the copy landed
  on the conflicted file.

- Folders on exFAT and FAT disks no longer stop with an AppleDouble
  error: the `._` files macOS keeps there for extended attributes are
  skipped, as the attributes travel with their files.

- A canary file kept unchanged for longer than the pair's age limit no
  longer stops every sync as if it were missing. Filters that leave out
  the canary are refused when you save the pair, and a `.foldoverignore`
  file that does is named as the cause.

- A large copy that arrived whole but could not be placed, because the
  file it replaces changed meanwhile or its metadata could not be set,
  keeps its data; the next attempt no longer fetches it again.

- A copy to another Mac whose source changed partway no longer waits
  forever for that Mac; asking another Mac gives up after 30 seconds.
- Stopping a share asks first, since pairs on the other Mac that use it
  stop syncing.

- Keep the agent running when a client disconnects before authentication.
