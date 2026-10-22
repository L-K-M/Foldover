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
app ─ api.github.com/releases/latest ─▶ newer? ─▶ panel ─▶ download page
```

- **App:** `FoldoverSystem.UpdateChecker` reads
  `repos/L-K-M/Foldover/releases/latest`, which never names a draft or
  a pre-release, and compares its `vX.Y.Z` tag with
  `CFBundleShortVersionString`. `UpdateStore` runs the check once when
  the app opens and again for the app menu's Check for Updates…;
  `UpdatePrompt` shows the outcome in a panel whose Download button
  opens the release page. The launch check announces only a newer
  release; the menu check also reports up-to-date and failure.
- **Release:** `release.yml` needs no keys or secrets now. It builds,
  verifies and zips the app and attaches the zip to the release.
- **Versions:** a release is still `vX.Y.Z`; anything the feed returns
  that is not three numbers is ignored, and a build whose own version
  cannot be parsed treats itself as current.

## Consequences

- No update keys to create, guard or lose; no third-party updater code
  runs in the app.
- Every update is a deliberate manual step: download, replace, re-run
  the documented `xattr` activation. The embedded agent restarts from
  the new bundle.
- The download page is reached over HTTPS to github.com, the same trust
  the first install already placed in the release page; nothing about
  the check is signed, so the check itself proves only what GitHub says.
- The check needs GitHub reachable at launch; a failure is silent unless
  the person asked for it.
- Updating relies on `releases/latest`, so a withdrawn or yanked release
  simply stops being offered.
