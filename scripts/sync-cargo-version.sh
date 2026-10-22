#!/usr/bin/env bash
#
# Post-bump hook for the release engine (RELEASE_POST_BUMP in
# scripts/release.sh): rewrites the [workspace.package] version in
# Cargo.toml to the release version, verifies the edit by re-reading
# it, then runs `cargo update --workspace` so Cargo.lock follows.
#
# Usage: RELEASE_NEW_VERSION=X.Y.Z scripts/sync-cargo-version.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

die() { echo "error: $*" >&2; exit 1; }

VERSION="${RELEASE_NEW_VERSION:-}"
[[ -n "$VERSION" ]] \
  || die "RELEASE_NEW_VERSION is not set - this script is driven by scripts/release.sh"
# The value goes straight into a sed replacement, and Cargo needs all
# three components (release.sh rejects anything else up front). The
# optional -suffix carries pre-releases.
SHAPE="${RELEASE_VERSION_REGEX:-$(bash "$ROOT/scripts/version-shape.sh")}" \
  || die "failed to read the version shape"
[[ "$VERSION" =~ $SHAPE ]] \
  || die "RELEASE_NEW_VERSION must look like X.Y.Z or X.Y.Z-suffix, got '$VERSION'"

TOML="Cargo.toml"
[[ -f "$TOML" ]] || die "$TOML not found"

# The only `version =` line worth touching is the one inside
# [workspace.package]; the section range keeps dependency `version`
# keys elsewhere in the file out of the edit.
IN_PACKAGE='/^\[workspace\.package\]/,/^\[/'

read_version() {
  sed -nE "$IN_PACKAGE s/^[[:space:]]*version[[:space:]]*=[[:space:]]*\"([^\"]*)\".*$/\1/p" "$TOML" | head -n 1
}

# Already at the target version (a hand edit, or a re-run): nothing to
# rewrite, but Cargo.lock still has to follow below.
if [[ "$(read_version)" == "$VERSION" ]]; then
  echo "-- $TOML already at $VERSION"
else
  # An explicit template: BSD mktemp on older macOS needs one.
  tmp="$(mktemp "${TMPDIR:-/tmp}/foldover-version.XXXXXX")"
  trap 'rm -f "$tmp"' EXIT
  sed -E "$IN_PACKAGE s/^([[:space:]]*version[[:space:]]*=[[:space:]]*\")[^\"]*(\".*)$/\1$VERSION\2/" "$TOML" > "$tmp"

  # sed exits 0 on a pattern that matched nothing, so compare instead.
  if cmp -s "$TOML" "$tmp"; then
    die "no [workspace.package] version line found in $TOML"
  fi
  # Write through the existing file so its permissions survive.
  cat "$tmp" > "$TOML"

  [[ "$(read_version)" == "$VERSION" ]] \
    || die "$TOML version is '$(read_version)' after the edit, expected '$VERSION'"
fi

# A missing cargo must fail the release, not silently ship a stale
# lockfile.
command -v cargo >/dev/null 2>&1 \
  || die "cargo not found - Cargo.lock cannot follow the version bump"
cargo update --workspace

echo "-- synced $TOML and Cargo.lock to $VERSION"
