#!/usr/bin/env bash
# Write the Sparkle appcast for one release (ADR 0035): a feed with one
# item whose enclosure is the release's zip, signed with the owner's
# EdDSA key. Apps fetch it from the latest release's assets.
# Usage: SPARKLE_PRIVATE_KEY=... bash scripts/appcast.sh <app.zip> <X.Y.Z> <appcast.xml>
# Requires macOS (Sparkle's sign_update) and the key in the environment.
set -euo pipefail

if [[ $# -ne 3 || "$1" != *.zip ]]; then
  echo "usage: SPARKLE_PRIVATE_KEY=... bash scripts/appcast.sh <app.zip> <X.Y.Z> <appcast.xml>" >&2
  exit 2
fi
ARCHIVE="$1"
VERSION="$2"
OUTPUT="$3"
if [[ -z "${SPARKLE_PRIVATE_KEY:-}" ]]; then
  echo "error: SPARKLE_PRIVATE_KEY is not set; see docs/adr/0035-sparkle-updates.md" >&2
  exit 1
fi

# Sparkle's tools, pinned like its framework in apple/FoldoverSystem.
SPARKLE_VERSION="2.10.0"
SPARKLE_SHA256="c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c"
MINIMUM_SYSTEM="27.0"
REPOSITORY="https://github.com/L-K-M/Foldover"

tools="$(mktemp -d)"
trap 'rm -rf "$tools"' EXIT
curl -fsSL -o "$tools/sparkle.tar.xz" \
  "https://github.com/sparkle-project/Sparkle/releases/download/$SPARKLE_VERSION/Sparkle-$SPARKLE_VERSION.tar.xz"
echo "$SPARKLE_SHA256  $tools/sparkle.tar.xz" | shasum -a 256 -c -
tar -xJf "$tools/sparkle.tar.xz" -C "$tools"

# e.g. sparkle:edSignature="…" length="1234"
( umask 077 && printf '%s' "$SPARKLE_PRIVATE_KEY" > "$tools/key" )
attributes="$("$tools/bin/sign_update" --ed-key-file "$tools/key" "$ARCHIVE")"
[[ "$attributes" == *'sparkle:edSignature="'* ]]

name="$(basename "$ARCHIVE")"
cat > "$OUTPUT" <<XML
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>Foldover</title>
    <item>
      <title>Foldover $VERSION</title>
      <pubDate>$(LC_ALL=C date -u '+%a, %d %b %Y %H:%M:%S +0000')</pubDate>
      <sparkle:version>$VERSION</sparkle:version>
      <sparkle:shortVersionString>$VERSION</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>$MINIMUM_SYSTEM</sparkle:minimumSystemVersion>
      <sparkle:releaseNotesLink>$REPOSITORY/releases/tag/v$VERSION</sparkle:releaseNotesLink>
      <enclosure url="$REPOSITORY/releases/download/v$VERSION/$name" $attributes type="application/octet-stream"/>
    </item>
  </channel>
</rss>
XML
xmllint --noout "$OUTPUT"
