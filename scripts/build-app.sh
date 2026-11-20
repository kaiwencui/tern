#!/bin/zsh
# Builds Tern.app or Tern Next.app in build/ (no Xcode project needed).
# Usage: scripts/build-app.sh [--next] [cargo profile]   (default: release)
set -euo pipefail
cd "$(dirname "$0")/.."
APP_NAME="Tern"
ICON_NAME="Tern"
IS_NEXT=0
PROFILE=""
for arg in "$@"; do
    case "$arg" in
        --next)
            IS_NEXT=1
            APP_NAME="Tern Next"
            ICON_NAME="TernNext"
            ;;
        -*)
            echo "Usage: $0 [--next] [cargo profile]" >&2
            exit 2
            ;;
        *)
            if [[ -n "$PROFILE" ]]; then
                echo "Usage: $0 [--next] [cargo profile]" >&2
                exit 2
            fi
            PROFILE="$arg"
            ;;
    esac
done
PROFILE="${PROFILE:-release}"
VERSION="$(awk '
    /^\[workspace\.package\]/ { in_pkg = 1; next }
    /^\[/ { in_pkg = 0 }
    in_pkg && /^version[[:space:]]*=/ {
        sub(/^[^"]*"/, "")
        sub(/".*$/, "")
        print
        exit
    }
' Cargo.toml)"
if [[ -z "$VERSION" ]]; then
    echo "error: could not read workspace version from Cargo.toml" >&2
    exit 1
fi
cargo build --profile "$PROFILE" -p tern-app --bin tern
# Cargo puts the dev profile in target/debug.
OUT_DIR="$PROFILE"
[[ "$PROFILE" == "dev" ]] && OUT_DIR="debug"
BIN="${CARGO_TARGET_DIR:-target}/$OUT_DIR/tern"
APP="build/$APP_NAME.app"
PLIST="$APP/Contents/Info.plist"
# The bundle holds exactly these files, so copying over an older bundle
# leaves nothing stale behind; codesign rewrites the signature.
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
# Put the binary in place by renaming: a Tern running from this bundle keeps
# the file it started from instead of having it overwritten under it.
cp "$BIN" "$APP/Contents/MacOS/tern.new"
mv -f "$APP/Contents/MacOS/tern.new" "$APP/Contents/MacOS/tern"
cp resources/Info.plist "$PLIST"
rm -f "$APP/Contents/Resources/"*.icns(N)
cp "resources/$ICON_NAME.icns" "$APP/Contents/Resources/$ICON_NAME.icns"

SHORT_VERSION="$VERSION"
BUILD_VERSION="$VERSION"
if (( IS_NEXT )); then
    SHORT_VERSION="${VERSION}-next"
    SHA="$(git rev-parse --short=7 HEAD 2>/dev/null || echo dev)"
    if [[ -n "$(git status --porcelain --untracked-files=no 2>/dev/null || true)" ]]; then
        SHA="${SHA}-dirty"
    fi
    BUILD_VERSION="$SHA"
fi

/usr/libexec/PlistBuddy \
    -c "Set :CFBundleIconFile $ICON_NAME" \
    -c "Set :CFBundleShortVersionString $SHORT_VERSION" \
    -c "Set :CFBundleVersion $BUILD_VERSION" \
    "$PLIST"

if (( IS_NEXT )); then
    /usr/libexec/PlistBuddy \
        -c "Set :CFBundleIdentifier com.kaicui.tern.next" \
        -c "Set :CFBundleName $APP_NAME" \
        -c "Set :CFBundleDisplayName $APP_NAME" \
        "$PLIST"
    i=0
    while MENU_ITEM="$(/usr/libexec/PlistBuddy -c "Print :NSServices:$i:NSMenuItem:default" "$PLIST" 2>/dev/null)"; do
        if [[ "$MENU_ITEM" == *"Tern"* ]]; then
            UPDATED="${MENU_ITEM//Tern/$APP_NAME}"
            /usr/libexec/PlistBuddy -c "Set :NSServices:$i:NSMenuItem:default $UPDATED" "$PLIST"
        fi
        (( i += 1 ))
    done
fi
plutil -lint -s "$PLIST"
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
