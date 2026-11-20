#!/bin/zsh
# Renders the stable or Next icon from scripts/icon.swift.
# Usage: scripts/make-icon.sh [--next]
set -euo pipefail
cd "$(dirname "$0")/.."
ICON_NAME="Tern"
if [[ "${1:-}" == "--next" ]] && (( $# == 1 )); then
    ICON_NAME="TernNext"
elif (( $# != 0 )); then
    echo "Usage: $0 [--next]" >&2
    exit 2
fi
WORK="$(mktemp -d "${TMPDIR:-/tmp}/tern-icon.XXXXXX")"
trap 'rm -rf -- "$WORK"' EXIT
SET="$WORK/$ICON_NAME.iconset"
mkdir -p "$SET"
swift scripts/icon.swift "$WORK/icon.png" "$@"
# Every size iconutil expects, at 1x and 2x.
for px in 16 32 128 256 512; do
    sips -z $px $px "$WORK/icon.png" --out "$SET/icon_${px}x${px}.png" >/dev/null
    sips -z $((px * 2)) $((px * 2)) "$WORK/icon.png" --out "$SET/icon_${px}x${px}@2x.png" >/dev/null
done
iconutil -c icns "$SET" -o "resources/$ICON_NAME.icns"
echo "Wrote resources/$ICON_NAME.icns"
