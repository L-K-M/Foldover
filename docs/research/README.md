# Research

Background for [PLAN.md](../../PLAN.md), gathered 2026-10-16.

- **[competitors.md](competitors.md)**: strengths and weaknesses of GoodSync,
  ChronoSync and SyncMate, side-by-side comparison, user sentiment, market
  gaps. Start here.
- **[sources/](sources/)**: the unedited reports behind it, each with
  per-claim source URLs and UNVERIFIED markers.

| Report | Topic |
|---|---|
| [goodsync.md](sources/goodsync.md) · [chronosync.md](sources/chronosync.md) · [syncmate.md](sources/syncmate.md) | Competitor deep dives |
| [user-sentiment.md](sources/user-sentiment.md) | Forums, reviews and app stores: praise, complaints, unmet needs, pricing |
| [landscape.md](sources/landscape.md) | 17 other tools, comparison table, market gaps, license reuse |
| [mac-ux.md](sources/mac-ux.md) | Mac design language, exemplary apps, sync UX patterns, wireframes |
| [macos-platform.md](sources/macos-platform.md) | FSEvents, file-system semantics, metadata, SMB/NFS, permissions, distribution |
| [rust-core.md](sources/rust-core.md) | Swift–Rust interop, process model, crates, Linux, testing |
| [sync-algorithm.md](sources/sync-algorithm.md) | Reconciliation models, edge cases, crash safety, design spec |
| `fact-check-*.md` | Independent checks of the load-bearing claims |

## Method

1. Nine research jobs ran on AI helper CLIs (devin SWE-2 max, opencode
   GLM-5.3). Each had to cite a source per claim and mark anything it could
   not confirm.
2. Fact-checks re-verified 58 load-bearing claims using a model other than
   the report's author: 53 confirmed, 3 partly right, 1 wrong,
   1 unverifiable.
3. Three independent plan proposals (product-first, architecture-first,
   risk-first) were synthesized into the plan.

## Corrections

The reports are unedited; these corrections override them.

| Report | Correction |
|---|---|
| macos-platform.md | Scoped "macOS 14 → 26", but **macOS 27 "Golden Gate" shipped 2026-09-14**, Apple silicon only. It removed the AFP client. |
| mac-ux.md | "macOS 26+ implies ARM64" is wrong: Tahoe still runs on some Intel Macs. "Finder Sync is broken on Tahoe" is contested: the framework is not deprecated, but users report breakage. |
| rust-core.md | UniFFI's Swift 6 support is **partial**: async code is not fully `Sendable` (uniffi-rs #2458). The `smb` crate is MIT. `pavao` is MIT since 0.3.0 (Samba's libsmbclient license still applies). Some source numbers are offset (the report notes this itself). |
| chronosync.md | Bootable backups on Apple Silicon are **warned against, not blocked**. Macworld's roundup lists ChronoSync **second** (Time Machine first), not as the top pick. The cited `CS_Manual.pdf` is the legacy 4.x manual. ChronoAgent's 2–3× figure refers to scanning only. |
| sync-algorithm.md | §3: a deletion propagates *to* a side that still matches the base, not *from* one (§8.3 states it correctly). |
| macos-platform.md | HFS+ stores a Unicode 3.2 decomposed variant, not literal NFD. |
| goodsync.md | Sib-SMB ships on Windows too. All 12 checked claims held. |
| syncmate.md | Conflict-policy names come from KB screenshots and app strings, not KB text. |
| user-sentiment.md | SyncTime costs $22.99 on the Mac App Store (5.6.1, checked 2026-10-16), not ~$10. |
