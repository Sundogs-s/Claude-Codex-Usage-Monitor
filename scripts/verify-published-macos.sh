#!/bin/bash
# Validate precisely the published DMG, not a later rebuild with the same version.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="$(sed -n 's/^version = "\([0-9.]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
OUT="$ROOT/dist/macos-arm64"
mkdir -p "$OUT"
NAME="UsageMonitor-$VERSION-arm64-unsigned-updates.dmg"
for FILE in "$NAME" "$NAME.sha256"; do
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 120 \
        "$MACOS_UPDATE_BASE_URL/v$VERSION/$FILE" -o "$OUT/$FILE"
done
(cd "$OUT" && shasum -a 256 -c "$NAME.sha256")
MOUNT="$(mktemp -d)"
trap 'hdiutil detach "$MOUNT" || true' EXIT
hdiutil attach -nobrowse -readonly -mountpoint "$MOUNT" "$OUT/$NAME"
mkdir -p "$OUT/build.published"
ditto "$MOUNT/Usage Monitor.app" "$OUT/build.published/Usage Monitor.app"
codesign --verify --deep --strict --verbose=2 "$OUT/build.published/Usage Monitor.app"
hdiutil detach "$MOUNT"
trap - EXIT
if [[ -n "${MACOS_UPDATE_FROM_VERSION:-}" ]]; then
    [[ "$MACOS_UPDATE_FROM_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
    OLDNAME="UsageMonitor-$MACOS_UPDATE_FROM_VERSION-arm64-unsigned-updates.dmg"
    for FILE in "$OLDNAME" "$OLDNAME.sha256"; do
        curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 120 \
            "$MACOS_UPDATE_BASE_URL/v$MACOS_UPDATE_FROM_VERSION/$FILE" -o "$OUT/$FILE"
    done
    (cd "$OUT" && shasum -a 256 -c "$OLDNAME.sha256")
    trap 'hdiutil detach "$MOUNT" || true' EXIT
    hdiutil attach -nobrowse -readonly -mountpoint "$MOUNT" "$OUT/$OLDNAME"
    mkdir -p "$OUT/previous.published"
    ditto "$MOUNT/Usage Monitor.app" "$OUT/previous.published/Usage Monitor.app"
    codesign --verify --deep --strict --verbose=2 "$OUT/previous.published/Usage Monitor.app"
    hdiutil detach "$MOUNT"
    trap - EXIT
    export MACOS_UPDATE_TEST_APP="$OUT/previous.published/Usage Monitor.app"
fi
bash "$ROOT/scripts/test-macos-updates.sh"
