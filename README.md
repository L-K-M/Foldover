# Foldover

> [!IMPORTANT]
> LLM disclosure: This codebase was written with substantial help from large language models: AI coding agents working from the [`AGENTS.md`](AGENTS.md) brief in this repo.

**Source version:** v<!-- version -->0.1.0<!-- /version --> · [Releases](https://github.com/L-K-M/Foldover/releases)

[![CI](https://github.com/L-K-M/Foldover/actions/workflows/ci.yml/badge.svg)](https://github.com/L-K-M/Foldover/actions/workflows/ci.yml)

Sync folders between your Mac's disks, NAS shares, SFTP servers and other
Macs, and see exactly what will happen before it happens. No cloud, no
account, no subscription.

Free and open-source under the Unlicense. No sponsorships or paid features.
The macOS app requires macOS 27 and Apple silicon.

> [!NOTE]
> **Status: engine and agent foundations.** Nothing syncs yet. [`PLAN.md`](PLAN.md) is the
> source of truth for scope and design; the research behind it lives in
> [`docs/research`](docs/research/README.md).

## Planned highlights

- **Preview first.** Every change is listed before it runs, with per-item
  overrides.
- **Nothing lost quietly.** Replaced and deleted files go to an archive;
  conflicts wait for you instead of being silently resolved.
- **Mac to Mac, built in.** Other Macs appear on the network; pair with a
  code, and sync encrypted over the LAN.
- **Native and fast.** A SwiftUI app over a portable Rust engine that keeps
  syncing with the window closed. A Linux app follows.

## Building

```
scripts/build.sh            # everything this host can build, staged in dist/
scripts/build.sh core       # Rust agent and CLI (macOS or Linux)
scripts/build.sh app        # macOS app (macOS 27+, Xcode 27+, XcodeGen)
cargo xtask ci              # the Linux CI gate
```

Toolchain details and every other command: [`AGENTS.md`](AGENTS.md).

## Background agent

The app embeds the Rust agent and CLI. Choose **Enable Background Agent**;
if macOS requests approval, open Login Items and allow Foldover, then refresh.
The current agent exposes health/version checks only. It cannot sync files.

For development, run these in separate terminals:

```sh
cargo run -p foldoverd
cargo run -p foldover-cli -- status
```

`cargo xtask agent-smoke` tests the built agent and CLI in a private runtime
directory, including single-instance locking, malformed requests, crash
restart and shutdown. It also runs inside `cargo xtask ci`.

## Opening a downloaded build

Builds are signed ad hoc, without Developer ID or notarization. Extract the
download, move `Foldover.app` to `/Applications`, then run:

```sh
xattr -dr com.apple.quarantine "/Applications/Foldover.app"
open "/Applications/Foldover.app"
```

This removes quarantine only from Foldover and its embedded code. File
access requires separate permissions.

## License

Public domain ([Unlicense](LICENSE)).
