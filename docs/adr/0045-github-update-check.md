# ADR 0045: Update checks through GitHub releases

Status: Implemented

Supersedes [ADR 0035](0035-sparkle-updates.md).

## Context

PLAN.md §9: Foldover ships ad-hoc-signed downloads activated with
`xattr`. ADR 0035 routed updates through Sparkle 2, whose EdDSA
signature was the only authentication available to an ad-hoc-signed app.
Sparkle also brought a feed, a full updater UI and a permanent private
key the owner would have to keep safe; losing it would have stranded
every installed copy. The owner does not want that machinery or its key
management (owner decision, 2026-10-22).

## Decision

The app does not update itself. It asks GitHub whether a newer release
exists and tells the person, with a link to the release's download page;
downloading and replacing the app stay manual, like the first install.

```text
release tag ─▶ release.yml ─▶ app zip as the GitHub release's asset
app ─ github.com/releases/latest ─ 302 ─▶ /releases/tag/vX.Y.Z
```

- **App:** `FoldoverSystem.UpdateChecker` opens
  `github.com/L-K-M/Foldover/releases/latest`, which redirects to the
  newest release's tag page; the final URL carries both the version and
  the download link. The plain web redirect needs no API call, so the
  check stays outside the unauthenticated REST API's per-address rate
  limit, which a shared NAT can exhaust. Latest never names a draft or
  a pre-release, matching the tags `release.sh` writes.
- **Pre-releases:** `release.sh` also accepts `vX.Y.Z-suffix` (semver
  pre-release), which `release.yml` publishes with `--prerelease` so
  `latest` never names it — a stable install is never told about a
  beta. A pre-release install instead reads `releases.atom`, the plain
  feed that lists every published release; `UpdateChecker` compares all
  of its tags by semver and takes the highest, so a beta install hears
  about the next beta and, once it ships, the stable release it
  preceded (`1.0.0-beta.9` < `1.0.0`). Feed order is newest-published,
  not newest-versioned, which is why the whole feed is compared.
- **Checks:** `UpdateStore` runs the check once when the app opens and
  again for the app menu's Check for Updates…; `UpdatePrompt` shows the
  outcome in a panel whose Download button opens the release page. The
  launch check announces only a newer release; the menu check also
  reports up-to-date and failure. Test launches skip it.
- **Release:** `release.yml` needs no keys or secrets now. It builds,
  verifies and zips the app and attaches the zip to the release;
  `package-app.sh` refuses a build whose `CFBundleShortVersionString`
  is not `X.Y.Z` or `X.Y.Z-suffix`, and the same shape gates the tag,
  `MARKETING_VERSION` and the Cargo workspace version.
- **Versions:** a release is `vX.Y.Z` or `vX.Y.Z-suffix`; a feed answer
  that does not parse and a build whose own version cannot be parsed
  both report the check as failed — "up to date" only follows a real
  comparison.

## Consequences

- No update keys to create, guard or lose; no third-party updater code
  runs in the app.
- Every update is a deliberate manual step: download, replace, re-run
  the documented `xattr` activation. The embedded agent restarts from
  the new bundle.
- The download page is reached over HTTPS to github.com, the same trust
  the first install already placed in the release page; nothing about
  the check is signed, so the check itself proves only what GitHub says.
- Every app open sends an unauthenticated request to github.com, so
  GitHub sees opens from the requester's address. The launch check
  needs GitHub reachable; a failure is silent, and only the menu item's
  check reports one.
- Updating relies on "latest", so a withdrawn or yanked release simply
  stops being offered.
