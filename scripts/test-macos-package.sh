#!/bin/bash
# Run on an isolated macOS build runner after build-macos.sh.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SPARKLE="$(bash "$ROOT/scripts/fetch-sparkle.sh")"
OUT="$ROOT/dist/macos-arm64"
cd "$OUT"
shasum -a 256 -c *.sha256
DMG="$(find "$PWD" -maxdepth 1 -name '*.dmg' -print -quit)"
hdiutil verify "$DMG"
MOUNT="$(mktemp -d)"
trap 'hdiutil detach "$MOUNT" || true' EXIT
hdiutil attach -nobrowse -readonly -mountpoint "$MOUNT" "$DMG"
APP="$MOUNT/Usage Monitor.app"
test -L "$MOUNT/Applications"
for icon in claude codex cursor settings repeat close; do test -s "$APP/Contents/Resources/Icons/$icon.png"; done
codesign --verify --deep --strict --verbose=2 "$APP"
for binary in "$APP/Contents/MacOS/UsageMonitorUI" "$APP/Contents/Resources/usage-monitor-core"; do
    test "$(xcrun lipo -archs "$binary")" = arm64
done
"$APP/Contents/Resources/usage-monitor-core" --version
/usr/libexec/PlistBuddy -c 'Print :SURequireSignedFeed' "$APP/Contents/Info.plist" | grep -qx true
test "$(/usr/libexec/PlistBuddy -c 'Print :SUPublicEDKey' "$APP/Contents/Info.plist")" = "$SPARKLE_PUBLIC_ED_KEY"
test "$(/usr/libexec/PlistBuddy -c 'Print :SUFeedURL' "$APP/Contents/Info.plist")" = "$MACOS_UPDATE_FEED_BASE_URL/appcast.xml"
test "$(/usr/libexec/PlistBuddy -c 'Print :UsageMonitorPreviewFeedURL' "$APP/Contents/Info.plist")" = "$MACOS_UPDATE_FEED_BASE_URL/appcast-preview.xml"
bash "$ROOT/scripts/sparkle-sign.sh" --verify "$OUT/appcast.xml"
# Bootstrap independent feeds at the same baseline; later releases can diverge.
cp "$OUT/appcast.xml" "$OUT/appcast-preview.xml"
bash "$ROOT/scripts/sparkle-sign.sh" --verify "$OUT/appcast-preview.xml"
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$APP/Contents/Info.plist")"
ZIP="$OUT/UsageMonitor-$VERSION-arm64-update.zip"
SIGNATURE="$(python3 - "$OUT/appcast.xml" <<'PY'
import sys, xml.etree.ElementTree as ET
print(ET.parse(sys.argv[1]).find('./channel/item/enclosure').attrib['{http://www.andymatuschak.org/xml-namespaces/sparkle}edSignature'])
PY
)"
bash "$ROOT/scripts/sparkle-sign.sh" --verify "$ZIP" "$SIGNATURE"
python3 - "$ZIP" "$OUT" <<'PY'
from pathlib import Path
import sys
archive=Path(sys.argv[1]).read_bytes()
(Path(sys.argv[2])/'tampered-test.zip').write_bytes(archive[:-1]+bytes([archive[-1]^1]))
PY
if bash "$ROOT/scripts/sparkle-sign.sh" --verify "$OUT/tampered-test.zip" "$SIGNATURE"; then
    echo 'ERROR: tampered update was accepted' >&2; exit 1
fi
rm "$OUT/tampered-test.zip"
echo 'PASS: mounted DMG, arm64, configured feeds/public key, authenticated feeds/archive and tampered archive rejection'
