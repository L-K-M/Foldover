#!/usr/bin/env bash
#
# The single home of the release-version shape: strict semver X.Y.Z
# with an optional -suffix (numeric identifiers forbid leading zeros).
# scripts/release.sh, scripts/package-app.sh, scripts/sync-cargo-version.sh
# and .github/workflows/release.yml all consume this so a tag the
# pipeline accepts is always one ReleaseVersion(parsing:) can compare.
set -euo pipefail

printf '%s' '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-(0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)(\.(0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*)?$'
