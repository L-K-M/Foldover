#!/usr/bin/env bash
# Check scripts/verify-update.swift on RFC 8032's second Ed25519 test
# vector (the one-byte message 0x72): it accepts the vector's public key
# and refuses the first vector's. macOS only.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

readonly signature="kqAJqfDUyrhyDoILX2QlQKKye1QWUD+Ps3YiI+vbadoIWsHkPhWZbkWPNhPQ8R2MOHsurrQwKu6wDSkWErsMAA=="
readonly key="PUAXw+hDiVqStwqnTRt+vJyYLM8uxJaMwM1V8Sr0Zgw="
readonly other_key="11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo="
readonly mismatch=1

xcrun swiftc -swift-version 6 -warnings-as-errors "$here/verify-update.swift" \
  -o "$work/verify-update"
printf '\x72' > "$work/archive"

"$work/verify-update" "$work/archive" "$signature" "$key"

status=0
"$work/verify-update" "$work/archive" "$signature" "$other_key" 2>/dev/null || status=$?
if (( status != mismatch )); then
  echo "test-verify-update: another key's check exited $status, not $mismatch" >&2
  exit 1
fi
echo "test-verify-update: ok"
