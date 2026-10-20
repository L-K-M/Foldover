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
  ${SPARKLE_PUBLIC_KEY:+"SPARKLE_PUBLIC_KEY=$SPARKLE_PUBLIC_KEY"} \
  build
app="$DERIVED/Build/Products/Release/Foldover.app"

# A release build carries the key its updates are checked against.
if [[ -n "${SPARKLE_PUBLIC_KEY:-}" ]]; then
  test "$(/usr/libexec/PlistBuddy -c 'Print :SUPublicEDKey' "$app/Contents/Info.plist")" = "$SPARKLE_PUBLIC_KEY"
fi

# Apple silicon only, macOS 27+, ad hoc signed inside out (PLAN.md §6).
test "$(lipo -archs "$app/Contents/MacOS/Foldover")" = "$ARCHITECTURE"
test "$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$app/Contents/Info.plist")" = "$MINIMUM_SYSTEM"
codesign --verify --deep --strict --verbose=2 "$app"
codesign -d --verbose=4 "$app" 2>&1 | grep -Fx 'Signature=adhoc'
for executable in foldoverd foldover; do
  helper="$app/Contents/Library/LaunchServices/$executable"
  test "$(lipo -archs "$helper")" = "$ARCHITECTURE"
  codesign --verify --strict "$helper"
  codesign -d --verbose=4 "$helper" 2>&1 | grep -E 'flags=.*runtime'
done
plutil -lint "$app/Contents/Library/LaunchAgents/ch.lkmc.foldover.agent.plist"

# The packaged helpers must pass the same process smoke as the build.
python3 "$ROOT/scripts/test-agent.py" \
  "$app/Contents/Library/LaunchServices/foldoverd" \
  "$app/Contents/Library/LaunchServices/foldover"

ditto -c -k --sequesterRsrc --keepParent "$app" "$OUTPUT"
