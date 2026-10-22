#!/bin/bash
# Pinned official Sparkle distribution. Download is authenticated by a fixed SHA-256.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION=2.10.0
EXPECTED=c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c
BASE="$ROOT/.build-deps"
ARCHIVE="$BASE/Sparkle-$VERSION.tar.xz"
DEST="$BASE/Sparkle-$VERSION"
mkdir -p "$BASE"
if [[ ! -f "$ARCHIVE" ]]; then
    curl --fail --location --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 180 \
        "https://github.com/sparkle-project/Sparkle/releases/download/$VERSION/Sparkle-$VERSION.tar.xz" -o "$ARCHIVE.partial"
    [[ "$(shasum -a 256 "$ARCHIVE.partial" | awk '{print $1}')" == "$EXPECTED" ]] || { echo 'Sparkle checksum mismatch' >&2; exit 1; }
    mv "$ARCHIVE.partial" "$ARCHIVE"
fi
[[ "$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')" == "$EXPECTED" ]] || { echo 'Sparkle checksum mismatch' >&2; exit 1; }
if [[ ! -f "$DEST/.verified-$EXPECTED" ]]; then
    [[ ! -e "$DEST" ]] || { echo 'Unverified Sparkle directory already exists; inspect it first.' >&2; exit 1; }
    STAGE="$(mktemp -d "$BASE/sparkle.XXXXXX")"
    tar -xJf "$ARCHIVE" -C "$STAGE"
    [[ -d "$STAGE/Sparkle.framework" && -x "$STAGE/bin/sign_update" ]] || { echo 'Invalid Sparkle distribution' >&2; exit 1; }
    touch "$STAGE/.verified-$EXPECTED"
    mv "$STAGE" "$DEST"
fi
printf '%s\n' "$DEST"
