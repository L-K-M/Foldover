#!/usr/bin/env bash
#
# Cuts a release: bumps MARKETING_VERSION in apple/project.yml (the
# generated .xcodeproj follows the spec), syncs the Cargo workspace
# version through RELEASE_POST_BUMP, commits, tags "v<version>", and
# with --push pushes branch + tag.
#
# Usage: scripts/release.sh [X.Y.Z] [--push]
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
# Cargo needs all three components, so X.Y (which the xcode kind would
# accept) is rejected up front instead of failing inside cargo.
export RELEASE_VERSION_REGEX='^[0-9]+\.[0-9]+\.[0-9]+$'

# Flag-only calls (--help, --version, --capabilities) touch nothing,
# so they skip the project preparation below.
case "${1:-}" in
  -h|--help|--version|--capabilities)
    PREPARE=false ;;
  *)
    PREPARE=true ;;
esac

# The engine reads the current version via `xcodebuild
# -showBuildSettings`. That needs the generated (gitignored) project,
# and resolving its FoldoverCore package needs the XCFramework, which
# is gitignored too: build both first on a fresh checkout.
if $PREPARE; then
  XCFRAMEWORK="apple/FoldoverCore/FoldoverCoreFFI.xcframework"
  if [[ ! -d "$XCFRAMEWORK" ]]; then
    echo "-- $XCFRAMEWORK missing, building it (cargo xtask ffi)"
    cargo xtask ffi
  fi

  if ! command -v xcodegen >/dev/null 2>&1; then
    echo "error: xcodegen not found - install it with: brew install xcodegen" >&2
    exit 1
  fi
  xcodegen generate --spec "$RELEASE_XCODEGEN_YML"
fi

BIN="${LKM_RELEASE_BIN:-lkm-release}"
command -v "$BIN" >/dev/null 2>&1 || {
  echo "error: lkm-release not found - clone https://github.com/L-K-M/release-tool and run ./install.sh" >&2
  exit 1
}
exec "$BIN" "$@"
