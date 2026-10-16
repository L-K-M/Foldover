#!/usr/bin/env bash
#
# Cuts a release: bumps MARKETING_VERSION in apple/project.yml (the
# generated .xcodeproj follows the spec), syncs the Cargo workspace
# version through RELEASE_POST_BUMP, commits, tags "v<version>", and
# with --push pushes branch + tag.
#
# Usage: scripts/release.sh [X.Y[.Z]] [--push]
# Shared engine: https://github.com/L-K-M/release-tool (this stub only
# sets config). Adapted from L-K-M/release-tool.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

export RELEASE_APP_NAME="Foldover"
export RELEASE_KIND="xcode"
export RELEASE_XCODE_PROJECT="apple/Foldover.xcodeproj"
export RELEASE_XCODE_SCHEME="Foldover"
export RELEASE_XCODEGEN_YML="apple/project.yml"
export RELEASE_INVOKED_AS="scripts/release.sh"
export RELEASE_CI_NOTE="The tag-triggered release workflow arrives with spike S6 (PLAN.md section 8); <tag> publishes nothing yet."
export RELEASE_POST_BUMP="scripts/sync-cargo-version.sh"

# The engine reads the current version via `xcodebuild
# -showBuildSettings`, but Foldover.xcodeproj is generated and
# gitignored: regenerate it first so the read has something to open.
if ! command -v xcodegen >/dev/null 2>&1; then
  echo "error: xcodegen not found - install it with: brew install xcodegen" >&2
  exit 1
fi
xcodegen generate --spec "$RELEASE_XCODEGEN_YML"

BIN="${LKM_RELEASE_BIN:-lkm-release}"
command -v "$BIN" >/dev/null 2>&1 || {
  echo "error: lkm-release not found - clone https://github.com/L-K-M/release-tool and run ./install.sh" >&2
  exit 1
}
exec "$BIN" "$@"
