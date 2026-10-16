# Foldover

> [!IMPORTANT]
> LLM disclosure: This codebase was written with substantial help from large language models: AI coding agents working from the [`AGENTS.md`](AGENTS.md) brief in this repo.

**Source version:** v<!-- version -->0.1.0<!-- /version --> · [Releases](https://github.com/L-K-M/Foldover/releases)

[![CI](https://github.com/L-K-M/Foldover/actions/workflows/ci.yml/badge.svg)](https://github.com/L-K-M/Foldover/actions/workflows/ci.yml)

Sync folders between your Mac's disks, NAS shares, SFTP servers and other
Macs, and see exactly what will happen before it happens. No cloud, no
account, no subscription.

> [!NOTE]
> **Status: foundations.** Nothing syncs yet. [`PLAN.md`](PLAN.md) is the
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
scripts/build.sh app        # macOS app (macOS 26+, Xcode, XcodeGen)
cargo xtask ci              # the Linux CI gate
```

Toolchain details and every other command: [`AGENTS.md`](AGENTS.md).

## License

Public domain ([Unlicense](LICENSE)).
