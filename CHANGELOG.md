# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

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
