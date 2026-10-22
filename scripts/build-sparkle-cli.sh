#!/bin/bash
# Build Sparkle's official 2.10.0 CLI solely for isolated CI update verification.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SPARKLE="$(bash "$ROOT/scripts/fetch-sparkle.sh")"
SRC="$ROOT/.build-deps/sparkle-cli-2.10.0-source"
mkdir -p "$SRC"
while read -r EXPECTED NAME; do
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 60 \
        "https://raw.githubusercontent.com/sparkle-project/Sparkle/2.10.0/sparkle-cli/$NAME" -o "$SRC/$NAME.partial"
    test "$(shasum -a 256 "$SRC/$NAME.partial" | awk '{print $1}')" = "$EXPECTED"
    mv "$SRC/$NAME.partial" "$SRC/$NAME"
done < "$ROOT/scripts/sparkle-cli-files.sha256"
STAGE="$(mktemp -d "$ROOT/.build-deps/sparkle-cli-build.XXXXXX")"
APP="$STAGE/sparkle.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks"
cp "$SRC/Info.plist" "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Set :CFBundleExecutable sparkle' "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Set :CFBundleIdentifier com.choseway.usage-monitor.ci-sparkle' "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Set :CFBundleName UsageMonitor CI Sparkle' "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Set :CFBundleShortVersionString 2.10.0' "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Set :CFBundleVersion 2.10.0' "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Set :LSMinimumSystemVersion 13.0' "$APP/Contents/Info.plist"
ditto "$SPARKLE/Sparkle.framework" "$APP/Contents/Frameworks/Sparkle.framework"
xcrun clang -fobjc-arc -fblocks -O2 -target arm64-apple-macos13.0 -isysroot "$(xcrun --sdk macosx --show-sdk-path)" \
    '-DSPU_OBJC_DIRECT=__attribute__((objc_direct))' '-DSPU_OBJC_DIRECT_MEMBERS=__attribute__((objc_direct_members))' \
    -framework Foundation -framework AppKit -framework Sparkle -F "$SPARKLE" \
    -Wl,-rpath,@executable_path/../Frameworks \
    "$SRC/main.m" "$SRC/SPUCommandLineDriver.m" "$SRC/SPUCommandLineUserDriver.m" -o "$APP/Contents/MacOS/sparkle"
codesign --force --sign - "$APP"
printf '%s\n' "$APP/Contents/MacOS/sparkle"
