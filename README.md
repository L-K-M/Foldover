# Foldover

> [!IMPORTANT]
> LLM disclosure: This codebase was written with substantial help from large language models: AI coding agents working from the [`AGENTS.md`](AGENTS.md) brief in this repo.

Foldover syncs folders between your Mac's disks, NAS shares, SFTP servers
and other Macs, and shows exactly what it will do before it does it. No
cloud, no account, no subscription — **planning stage**.

- [PLAN.md](PLAN.md) — the product plan: a portable Rust engine under a
  background agent, native per-platform UIs (SwiftUI/AppKit on macOS,
  GTK4/libadwaita on Linux later), three-way reconciliation, and
  Mac-to-Mac sync over encrypted QUIC with Bonjour pairing.
- [docs/research/](docs/research/README.md) — the research behind the
  plan, including the [competitor review](docs/research/competitors.md).

## License

Public domain (Unlicense) — see [LICENSE](LICENSE).
