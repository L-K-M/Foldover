#!/usr/bin/env bash
# Build the ad-hoc-signed Release app, verify it, and zip it for download.
# Usage: bash scripts/package-app.sh <output.zip> <derived-data-directory>
# Requires macOS 27, Xcode 27, the generated Xcode project and the
# XCFramework (cargo xtask ffi). Shared by CI and the release workflow.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $# -ne 2 || "$1" != *.zip ]]; then
  echo "usage: bash scripts/package-app.sh <output.zip> <derived-data-directory>" >&2
  exit 2
fi
OUTPUT="$1"
DERIVED="$2"
MINIMUM_SYSTEM="27.0"
ARCHITECTURE="arm64"

xcodebuild -project "$ROOT/apple/Foldover.xcodeproj" \
  -scheme Foldover \
  -destination 'generic/platform=macOS' \
  -derivedDataPath "$DERIVED" \
  -configuration Release \
  CODE_SIGN_IDENTITY=- \
  build
app="$DERIVED/Build/Products/Release/Foldover.app"

# Apple silicon only, macOS 27+, ad hoc signed inside out (PLAN.md §6).
test "$(lipo -archs "$app/Contents/MacOS/Foldover")" = "$ARCHITECTURE"
test "$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$app/Contents/Info.plist")" = "$MINIMUM_SYSTEM"
# The update check compares this version against release tags; anything
# else can never be compared, so every check would report failure
# instead of a real answer (ADR 0045). The optional -suffix carries
# pre-releases.
version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")"
# Read the shape on its own line and fail loudly: a failed or silent
# run would otherwise leave an empty pattern that matches everything.
shape="$(bash "$ROOT/scripts/version-shape.sh")" \
  || { echo "error: version-shape.sh failed" >&2; exit 1; }
[[ -n "$shape" ]] \
  || { echo "error: version-shape.sh printed nothing" >&2; exit 1; }
if ! printf '%s' "$version" | grep -Eq "$shape"; then
  echo "error: CFBundleShortVersionString '$version' is not X.Y.Z(-suffix); the update check could never compare it" >&2
  exit 1
fi
# Third-party notices (embed-agent.sh): present, non-empty and naming
# crates certain to ship, including the MPL-2.0 UniFFI runtime and the
# UniFFI template code in the generated bindings.
notices="$app/Contents/Resources/Third-Party-Notices.txt"
std_notices="$app/Contents/Resources/Rust-Licenses.html"
test -s "$notices"
test -s "$std_notices"
grep -q '^tokio ' "$notices"
grep -q '^uniffi ' "$notices"
grep -q '^UniFFI-generated Swift bindings ' "$notices"
grep -q 'Copyright notices for The Rust Standard Library' "$std_notices"
codesign --verify --deep --strict --verbose=2 "$app"
codesign -d --verbose=4 "$app" 2>&1 | grep -Fx 'Signature=adhoc'
for executable in foldoverd foldover; do
  helper="$app/Contents/Library/LaunchServices/$executable"
  test "$(lipo -archs "$helper")" = "$ARCHITECTURE"
  codesign --verify --strict "$helper"
  codesign -d --verbose=4 "$helper" 2>&1 | grep -E 'flags=.*runtime'
done
plutil -lint "$app/Contents/Library/LaunchAgents/ch.lkmc.foldover.agent.plist"
python3 "$ROOT/scripts/agent-plist.py" check \
  "$app/Contents/Library/LaunchServices/foldoverd" \
  "$app/Contents/Library/LaunchAgents/ch.lkmc.foldover.agent.plist"

# The packaged helpers must pass the same process smoke as the build.
python3 "$ROOT/scripts/test-agent.py" \
  "$app/Contents/Library/LaunchServices/foldoverd" \
  "$app/Contents/Library/LaunchServices/foldover"

ditto -c -k --sequesterRsrc --keepParent "$app" "$OUTPUT"

# What users download carries the notices too.
for notice in Third-Party-Notices.txt Rust-Licenses.html; do
  unzip -l "$OUTPUT" "Foldover.app/Contents/Resources/$notice" >/dev/null
done
