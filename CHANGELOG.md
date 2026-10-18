# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

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

- Keep the agent running when a client disconnects before authentication.
