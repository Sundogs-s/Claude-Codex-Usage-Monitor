#!/bin/bash
# The seed is passed through private stdin, never as a command argument or file.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SPARKLE="$(bash "$ROOT/scripts/fetch-sparkle.sh")"
if [[ -n "${SPARKLE_PRIVATE_ED_KEY:-}" ]]; then
    printf '%s' "$SPARKLE_PRIVATE_ED_KEY" | "$SPARKLE/bin/sign_update" --ed-key-file - "$@"
else
    "$SPARKLE/bin/sign_update" --account "${SPARKLE_KEYCHAIN_ACCOUNT:-ed25519}" "$@"
fi
