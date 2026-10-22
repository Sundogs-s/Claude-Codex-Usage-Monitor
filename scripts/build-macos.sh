#!/bin/bash
# Apple Silicon + macOS 13+. Uses installed Xcode CLI tools / Rust; installs nothing.
set -euo pipefail
umask 077
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
[[ "$(uname -s)" == Darwin && "$(uname -m)" == arm64 ]] || {
    echo 'Build requires an Apple Silicon Mac (run outside Rosetta).' >&2; exit 1;
}
for tool in cargo rustup xcrun codesign hdiutil ditto shasum curl tar; do
    command -v "$tool" >/dev/null || { echo "Missing build tool: $tool" >&2; exit 1; }
done
TARGET=aarch64-apple-darwin
rustup target list --installed | /usr/bin/grep -qx "$TARGET" || {
    echo "Install the Rust target first: rustup target add $TARGET" >&2; exit 1;
}
VERSION="$(/usr/bin/sed -n 's/^version = "\([0-9.]*\)"/\1/p' "$ROOT/Cargo.toml" | /usr/bin/head -n 1)"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'Invalid Cargo version' >&2; exit 1; }
for key in CFBundleShortVersionString CFBundleVersion; do
    [[ "$(/usr/libexec/PlistBuddy -c "Print :$key" "$ROOT/macos/Info.plist")" == "$VERSION" ]] || {
        echo 'Cargo and Info.plist versions differ' >&2; exit 1;
    }
done
SDK="$(xcrun --sdk macosx --show-sdk-path)"
SPARKLE="$(bash "$ROOT/scripts/fetch-sparkle.sh")"
UPDATE_BASE="${MACOS_UPDATE_BASE_URL:-}"
FEED_BASE="${MACOS_UPDATE_FEED_BASE_URL:-$UPDATE_BASE}"
SIGN_ACCOUNT="${SPARKLE_KEYCHAIN_ACCOUNT:-ed25519}"
PUBLIC_KEY="${SPARKLE_PUBLIC_ED_KEY:-}"
if [[ -n "$UPDATE_BASE" || -n "$PUBLIC_KEY" ]]; then
    [[ "$PUBLIC_KEY" =~ ^[A-Za-z0-9+/]{43}=$ ]] || { echo 'Invalid Sparkle public key (expected 32-byte base64).' >&2; exit 1; }
    [[ "$UPDATE_BASE" == https://* && "$FEED_BASE" == https://* && -n "$PUBLIC_KEY" ]] || {
        echo 'Configure both MACOS_UPDATE_BASE_URL (HTTPS) and SPARKLE_PUBLIC_ED_KEY.' >&2; exit 1;
    }
fi
if [[ -n "${MACOS_NOTARY_PROFILE:-}" ]]; then
    command -v python3 >/dev/null || { echo 'Python 3 is required to generate the signed update feed.' >&2; exit 1; }
    [[ -n "$UPDATE_BASE" && -n "$PUBLIC_KEY" ]] || {
        echo 'A notarized team build requires the update feed URL and Sparkle public key.' >&2; exit 1;
    }
fi
if [[ -n "$UPDATE_BASE" ]]; then
    command -v python3 >/dev/null || { echo "Python 3 is required for update builds." >&2; exit 1; }
    python3 - "$UPDATE_BASE" "$FEED_BASE" <<'PY'
import sys
from urllib.parse import urlsplit
for value in sys.argv[1:]:
    url = urlsplit(value)
    if url.scheme != "https" or not url.hostname or url.username or url.password or url.query or url.fragment:
        raise SystemExit("Update URLs must be HTTPS without credentials/query/fragment")
PY
    if [[ -n "${SPARKLE_PRIVATE_ED_KEY:-}" ]]; then
        printf '%s' "$SPARKLE_PRIVATE_ED_KEY" | xcrun swift "$ROOT/scripts/verify-sparkle-key.swift"
    else
        [[ "$("$SPARKLE/bin/generate_keys" --account "$SIGN_ACCOUNT" -p)" == "$PUBLIC_KEY" ]] || {
            echo "Sparkle signing key does not match the configured public key." >&2; exit 1;
        }
    fi
fi
export MACOSX_DEPLOYMENT_TARGET=13.0
cd "$ROOT"
cargo test --locked --target "$TARGET" --lib
cargo build --release --locked --target "$TARGET" --bin usage-monitor-core
# Fresh staging directory; never recursively delete a caller-supplied path.
OUT="$ROOT/dist/macos-arm64"
mkdir -p "$OUT"
STAGE="$(mktemp -d "$OUT/build.XXXXXX")"
APP="$STAGE/Usage Monitor.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
cp "$ROOT/macos/Info.plist" "$APP/Contents/Info.plist"
mkdir -p "$APP/Contents/Resources/Icons"
cp "$ROOT"/assets/ui/*.png "$APP/Contents/Resources/Icons/"
ditto "$SPARKLE/Sparkle.framework" "$APP/Contents/Frameworks/Sparkle.framework"
cp "$SPARKLE/LICENSE" "$APP/Contents/Resources/Sparkle-LICENSE.txt"
if [[ -n "$UPDATE_BASE" ]]; then
    /usr/libexec/PlistBuddy -c "Add :SUFeedURL string ${FEED_BASE%/}/appcast.xml" "$APP/Contents/Info.plist"
    /usr/libexec/PlistBuddy -c "Add :UsageMonitorPreviewFeedURL string ${FEED_BASE%/}/appcast-preview.xml" "$APP/Contents/Info.plist"
    /usr/libexec/PlistBuddy -c "Add :SUPublicEDKey string $PUBLIC_KEY" "$APP/Contents/Info.plist"
    /usr/libexec/PlistBuddy -c "Add :SURequireSignedFeed bool true" "$APP/Contents/Info.plist"
fi
cp "$ROOT/target/$TARGET/release/usage-monitor-core" "$APP/Contents/Resources/usage-monitor-core"
xcrun swiftc -swift-version 5 -parse-as-library -O -target arm64-apple-macos13.0 -sdk "$SDK" \
    -framework SwiftUI -framework AppKit -framework ServiceManagement -framework Sparkle -F "$SPARKLE" \
    -Xlinker -rpath -Xlinker '@executable_path/../Frameworks' \
    "$ROOT/macos/UIRegression.swift" "$ROOT/macos/ChoseWayUI.swift" "$ROOT/macos/Updater.swift" "$ROOT/macos/UsageMonitor.swift" -o "$APP/Contents/MacOS/UsageMonitorUI"
chmod 755 "$APP/Contents/MacOS/UsageMonitorUI" "$APP/Contents/Resources/usage-monitor-core"
for binary in "$APP/Contents/MacOS/UsageMonitorUI" "$APP/Contents/Resources/usage-monitor-core"; do
    [[ "$(xcrun lipo -archs "$binary")" == arm64 ]] || { echo 'Unexpected binary architecture' >&2; exit 1; }
done
IDENTITY="${MACOS_SIGN_IDENTITY:--}"
PROFILE="${MACOS_NOTARY_PROFILE:-}"
SUFFIX=unsigned
if [[ -n "$UPDATE_BASE" && "${MACOS_ALLOW_ADHOC_UPDATES:-0}" == 1 ]]; then SUFFIX=unsigned-updates; fi
SIGN_FLAGS=(--force --sign "$IDENTITY")
if [[ "$IDENTITY" != - ]]; then
    SIGN_FLAGS+=(--options runtime --timestamp)
    SUFFIX=signed-unnotarized
elif [[ -n "$PROFILE" ]]; then
    echo 'Notarization requires MACOS_SIGN_IDENTITY (Developer ID Application).' >&2; exit 1
fi
FRAMEWORK="$APP/Contents/Frameworks/Sparkle.framework"
codesign "${SIGN_FLAGS[@]}" "$FRAMEWORK/Versions/B/XPCServices/Installer.xpc"
codesign "${SIGN_FLAGS[@]}" --preserve-metadata=entitlements "$FRAMEWORK/Versions/B/XPCServices/Downloader.xpc"
codesign "${SIGN_FLAGS[@]}" "$FRAMEWORK/Versions/B/Autoupdate"
codesign "${SIGN_FLAGS[@]}" "$FRAMEWORK/Versions/B/Updater.app"
codesign "${SIGN_FLAGS[@]}" "$FRAMEWORK"
codesign "${SIGN_FLAGS[@]}" "$APP/Contents/Resources/usage-monitor-core"
codesign "${SIGN_FLAGS[@]}" "$APP/Contents/MacOS/UsageMonitorUI"
codesign "${SIGN_FLAGS[@]}" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
if [[ -n "$PROFILE" ]]; then
    ditto -c -k --keepParent "$APP" "$STAGE/notarize.zip"
    xcrun notarytool submit "$STAGE/notarize.zip" --keychain-profile "$PROFILE" --wait
    xcrun stapler staple "$APP"
    xcrun stapler validate "$APP"
    SUFFIX=notarized
fi
DMG="$OUT/UsageMonitor-$VERSION-arm64-$SUFFIX.dmg"
[[ ! -e "$DMG" ]] || { echo "Refusing to replace existing artifact: $DMG" >&2; exit 1; }
PAYLOAD="$STAGE/dmg"
mkdir -p "$PAYLOAD"
ditto "$APP" "$PAYLOAD/Usage Monitor.app"
ln -s /Applications "$PAYLOAD/Applications"
hdiutil create -volname 'Usage Monitor' -srcfolder "$PAYLOAD" -ov -format UDZO "$DMG"
if [[ "$IDENTITY" != - ]]; then codesign "${SIGN_FLAGS[@]}" "$DMG"; fi
if [[ -n "$PROFILE" ]]; then
    xcrun notarytool submit "$DMG" --keychain-profile "$PROFILE" --wait
    xcrun stapler staple "$DMG"
    xcrun stapler validate "$DMG"
    spctl --assess --type execute --verbose=2 "$APP"
fi
if [[ "$SUFFIX" == notarized || "$SUFFIX" == unsigned-updates ]]; then
    ZIP="$OUT/UsageMonitor-$VERSION-arm64-update.zip"
    [[ ! -e "$ZIP" ]] || { echo "Refusing to replace existing artifact: $ZIP" >&2; exit 1; }
    ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"
    bash "$ROOT/scripts/sparkle-sign.sh" "$ZIP" > "$ZIP.signature.xml"
    python3 "$ROOT/scripts/generate-appcast.py" --archive "$ZIP" --signature "$ZIP.signature.xml" \
        --version "$VERSION" --base-url "$UPDATE_BASE" --channel "${MACOS_UPDATE_CHANNEL:-stable}" --output-directory "$OUT"
    FEED_NAME=appcast.xml
    if [[ "${MACOS_UPDATE_CHANNEL:-stable}" == preview ]]; then FEED_NAME=appcast-preview.xml; fi
    bash "$ROOT/scripts/sparkle-sign.sh" "$OUT/$FEED_NAME"
    bash "$ROOT/scripts/sparkle-sign.sh" --verify "$OUT/$FEED_NAME"
    (cd "$OUT" && shasum -a 256 "$(basename "$ZIP")") > "$ZIP.sha256"
fi
(cd "$OUT" && shasum -a 256 "$(basename "$DMG")") > "$DMG.sha256"
echo "Built: $DMG"
echo "Staging retained for inspection: $STAGE"
if [[ "$SUFFIX" != notarized ]]; then
    echo 'Unnotarized testing artifact. Public production distribution requires Developer ID signing and notarization.'
fi
