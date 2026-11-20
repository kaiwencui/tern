#!/bin/zsh
# Builds build/Tern.app from the release binary (no Xcode project needed).
# Usage: scripts/build-app.sh [cargo profile]   (default: release)
set -euo pipefail
cd "$(dirname "$0")/.."
PROFILE="${1:-release}"
cargo build --profile "$PROFILE" -p tern-app --bin tern
# Cargo puts the dev profile in target/debug.
OUT_DIR="$PROFILE"
[[ "$PROFILE" == "dev" ]] && OUT_DIR="debug"
BIN="${CARGO_TARGET_DIR:-target}/$OUT_DIR/tern"
APP="build/Tern.app"
# The bundle holds exactly these files, so copying over an older bundle
# leaves nothing stale behind; codesign rewrites the signature.
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
# Put the binary in place by renaming: a Tern running from this bundle keeps
# the file it started from instead of having it overwritten under it.
cp "$BIN" "$APP/Contents/MacOS/tern.new"
mv -f "$APP/Contents/MacOS/tern.new" "$APP/Contents/MacOS/tern"
cp resources/Info.plist "$APP/Contents/Info.plist"
cp resources/Tern.icns "$APP/Contents/Resources/Tern.icns"
plutil -lint -s "$APP/Contents/Info.plist"
# Sign with TERN_SIGNING_IDENTITY when it is in the keychain, so macOS keeps
# permissions granted to Tern from one build to the next; an ad-hoc
# signature changes with every build.
IDENTITY="${TERN_SIGNING_IDENTITY:-}"
SIGNED="ad hoc"
if [[ -n "$IDENTITY" ]]; then
    # Read the list first: grep -q stops reading early, and with pipefail the
    # writer's SIGPIPE would make a present identity look missing.
    IDENTITIES="$(security find-identity -p codesigning 2>/dev/null || true)"
    if grep -qF "\"$IDENTITY\"" <<< "$IDENTITIES"; then
        if codesign --force --options runtime --timestamp --sign "$IDENTITY" "$APP" >/dev/null 2>&1; then
            SIGNED="with \"$IDENTITY\""
        else
            echo "warning: signing with \"$IDENTITY\" failed; signing ad hoc" >&2
        fi
    else
        echo "warning: \"$IDENTITY\" is not in the keychain; signing ad hoc" >&2
    fi
fi
if [[ "$SIGNED" == "ad hoc" ]]; then
    codesign --force --sign - "$APP" >/dev/null 2>&1 || echo "warning: ad-hoc signing failed; the app may not open" >&2
fi
codesign --verify --strict "$APP"
echo "Built $APP, signed $SIGNED"
