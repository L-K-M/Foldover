# ADR 0035: Sparkle updates

Status: Superseded by [ADR 0045](0045-github-update-check.md); the owner
chose not to ship an updater framework or keep signing keys (2026-10-22)

## Context

PLAN.md §9: Foldover ships ad-hoc-signed downloads and updates through
Sparkle 2. Ad-hoc code signing proves nothing about who built an
update, so Sparkle's EdDSA signature is the only authentication. The
private key must never be in the repository or a build.

## Decision

```text
release tag ─▶ release.yml ─▶ app zip (public key in Info.plist)
                           └▶ appcast.xml (zip's EdDSA signature)
app ─ SUFeedURL ─▶ releases/latest/download/appcast.xml ─▶ zip
```

- **App:** `FoldoverSystem.AppUpdater` wraps Sparkle's standard updater
  (2.10.0, pinned exactly); `UpdateStore` exposes it, and the app menu
  offers Check for Updates…. Sparkle asks before its first automatic
  check. The feed is the latest release's `appcast.xml`, so pre-releases
  never reach it.
- **Keys:** `SPARKLE_PUBLIC_KEY` is a build setting written into the
  Info.plist (`apple/Foldover-Info.plist`, the only hand-written plist).
  Empty, as in development and CI builds, the app has no updater and the
  menu item is disabled. The release workflow takes it from the
  repository variable of that name, and signs with the
  `SPARKLE_PRIVATE_KEY` secret; a release without them fails.
- **Versions:** Sparkle compares `CFBundleVersion`, so it now equals
  `MARKETING_VERSION`, which every release raises.
- **Appcast:** `scripts/appcast.sh` downloads Sparkle's tools (checksum
  pinned), signs the zip with `sign_update`, and writes a one-item feed
  uploaded with the release. It first checks the signature against
  `SPARKLE_PUBLIC_KEY` (`scripts/verify-update.swift`), so a private
  key from another pair fails the release before anything is published,
  instead of shipping an update no installed copy accepts.

### Owner setup, once

1. Download `Sparkle-2.10.0.tar.xz` from Sparkle's releases and run
   `bin/generate_keys`. It keeps the private key in your login Keychain
   and prints the public key.
2. Add the public key as the repository variable `SPARKLE_PUBLIC_KEY`.
3. Run `bin/generate_keys -x sparkle-private-key`, paste the file's
   content into the repository secret `SPARKLE_PRIVATE_KEY`, then delete
   the file.

Losing the private key means installed copies can never update again:
keep the Keychain item backed up.

## Consequences

- Builds made before the keys existed cannot update themselves; their
  users download the next release by hand.
- Each update replaces the app and its embedded agent together; the
  agent restarts from the new bundle.
- Release notes come from the GitHub release page.
