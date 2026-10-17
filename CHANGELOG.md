# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Health-only background agent, typed Unix-socket client, `foldover status`,
  native registration/status controls, and embedded ad-hoc-signed helpers.
- Read-only local metadata access through held directory handles, with
  no-follow traversal and exact name/link-target bytes.
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
