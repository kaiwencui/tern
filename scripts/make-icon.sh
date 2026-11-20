#!/bin/zsh
# Renders resources/Tern.icns from scripts/icon.swift.
# Usage: scripts/make-icon.sh
set -euo pipefail
cd "$(dirname "$0")/.."
WORK="$(mktemp -d "${TMPDIR:-/tmp}/tern-icon.XXXXXX")"
SET="$WORK/Tern.iconset"
mkdir -p "$SET"
swift scripts/icon.swift "$WORK/icon.png"
# Every size iconutil expects, at 1x and 2x.
for px in 16 32 128 256 512; do
    sips -z $px $px "$WORK/icon.png" --out "$SET/icon_${px}x${px}.png" >/dev/null
    sips -z $((px * 2)) $((px * 2)) "$WORK/icon.png" --out "$SET/icon_${px}x${px}@2x.png" >/dev/null
done
iconutil -c icns "$SET" -o resources/Tern.icns
echo "Wrote resources/Tern.icns (work files in $WORK)"
