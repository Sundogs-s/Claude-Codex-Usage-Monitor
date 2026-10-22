#!/bin/bash
# Uses a disposable, lower-version bundle. Never changes an installed member app.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SPARKLE="$(bash "$ROOT/scripts/fetch-sparkle.sh")"
CLI="$(bash "$ROOT/scripts/build-sparkle-cli.sh")"
test -x "$CLI"
OUT="$ROOT/dist/macos-arm64"
APP="$(find "$OUT" -path '*/build.*/Usage Monitor.app' -type d -maxdepth 3 -print -quit)"
test -d "$APP"
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$APP/Contents/Info.plist")"
TESTROOT="$(mktemp -d "$OUT/update-check.XXXXXX")"
OLDAPP="$TESTROOT/Usage Monitor.app"
CHANNEL="${MACOS_UPDATE_CHANNEL:-stable}"
case "$CHANNEL" in
    stable) INSTALL_FEED=appcast.xml ;;
    preview) INSTALL_FEED=appcast-preview.xml ;;
    *) echo 'Unsupported update channel' >&2; exit 1 ;;
esac
if [[ -n "${MACOS_UPDATE_TEST_APP:-}" ]]; then
    ditto "$MACOS_UPDATE_TEST_APP" "$OLDAPP"
    test "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$OLDAPP/Contents/Info.plist")" = "$MACOS_UPDATE_FROM_VERSION"
else
    ditto "$APP" "$OLDAPP"
    /usr/libexec/PlistBuddy -c 'Set :CFBundleVersion 0.0.1' "$OLDAPP/Contents/Info.plist"
    /usr/libexec/PlistBuddy -c 'Set :CFBundleShortVersionString 0.0.1' "$OLDAPP/Contents/Info.plist"
    codesign --force --sign - "$OLDAPP"
fi
PROBEAPP="$TESTROOT/probe/Usage Monitor.app"
mkdir -p "$(dirname "$PROBEAPP")"
ditto "$APP" "$PROBEAPP"
/usr/libexec/PlistBuddy -c 'Set :CFBundleVersion 0.0.1' "$PROBEAPP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Set :CFBundleShortVersionString 0.0.1' "$PROBEAPP/Contents/Info.plist"
codesign --force --sign - "$PROBEAPP"
# Check independent public feeds using Sparkle's own signed-feed validation.
for FEED in appcast.xml appcast-preview.xml; do
    "$CLI" "$PROBEAPP" --probe --feed-url "$MACOS_UPDATE_FEED_BASE_URL/$FEED" --user-agent-name UsageMonitor-CI --verbose
done
# Downloads the public signed archive, authenticates it, and replaces only OLDAPP.
"$CLI" "$OLDAPP" --check-immediately --feed-url "$MACOS_UPDATE_FEED_BASE_URL/$INSTALL_FEED" --user-agent-name UsageMonitor-CI --verbose
test "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$OLDAPP/Contents/Info.plist")" = "$VERSION"
codesign --verify --deep --strict --verbose=2 "$OLDAPP"
"$OLDAPP/Contents/Resources/usage-monitor-core" --version
# Equal version must not be offered again (Sparkle probe returns nonzero).
set +e
"$CLI" "$OLDAPP" --probe --feed-url "$MACOS_UPDATE_FEED_BASE_URL/$INSTALL_FEED" --user-agent-name UsageMonitor-CI --verbose
PROBE_STATUS=$?
set -e
test "$PROBE_STATUS" = 4 # Sparkle CLI: no update, not a transport/signature failure.
# The updated preview must also suppress a downgrade on the stable channel.
if [[ "$CHANNEL" == preview ]]; then
    set +e
    "$CLI" "$OLDAPP" --probe --feed-url "$MACOS_UPDATE_FEED_BASE_URL/appcast.xml" --user-agent-name UsageMonitor-CI --verbose
    STABLE_STATUS=$?
    set -e
    test "$STABLE_STATUS" = 4
fi
echo "PASS: public HTTPS stable/preview probes, actual $CHANNEL Sparkle download/authentication/bundle replacement, equal-version and downgrade suppression"
