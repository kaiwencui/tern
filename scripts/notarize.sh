#!/bin/zsh
# Usage: scripts/notarize.sh [--next] [zip|dmg]  (default: zip)
# Builds a Developer ID signed, notarized and stapled release in dist/.
#
# Required environment variables:
#   TERN_SIGNING_IDENTITY   e.g. "Developer ID Application: Your Name (TEAMID)"
#   TERN_NOTARY_PROFILE     keychain profile saved via `xcrun notarytool store-credentials`
set -euo pipefail
cd "$(dirname "$0")/.."

IS_NEXT=0
FORMAT=""
for arg in "$@"; do
    case "$arg" in
        --next)
            IS_NEXT=1
            ;;
        zip|dmg)
            if [[ -n "$FORMAT" ]]; then
                echo "Usage: $0 [--next] [zip|dmg]" >&2
                exit 2
            fi
            FORMAT="$arg"
            ;;
        *)
            echo "Usage: $0 [--next] [zip|dmg]" >&2
            exit 2
            ;;
    esac
done
FORMAT="${FORMAT:-zip}"

: "${TERN_SIGNING_IDENTITY:?set TERN_SIGNING_IDENTITY to your Developer ID Application certificate}"
: "${TERN_NOTARY_PROFILE:?set TERN_NOTARY_PROFILE to your notarytool keychain profile}"

APP_NAME="Tern"
FILE_PREFIX="Tern"
BUILD_ARGS=(release)
if (( IS_NEXT )); then
    APP_NAME="Tern Next"
    FILE_PREFIX="Tern-Next"
    BUILD_ARGS=(--next release)
fi

mkdir -p dist
WORK=$(mktemp -d "$PWD/dist/.notarize.XXXXXXXX")
trap 'rm -rf -- "$WORK"' EXIT

TERN_BUILD_DIR="$WORK/build" ./scripts/build-app.sh "${BUILD_ARGS[@]}"

APP="$WORK/build/$APP_NAME.app"
if ! codesign -d --verbose=2 "$APP" 2>&1 | awk '/^Authority=Developer ID Application:/ { signed = 1 } END { exit !signed }'; then
    echo "$APP_NAME.app must be signed with a Developer ID Application certificate" >&2
    exit 1
fi

if [[ "$FORMAT" == dmg ]]; then
    VERSION=$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist")
    VERSION="${VERSION%-next}"
    NAME="$FILE_PREFIX-$VERSION-$(uname -m).dmg"
    mkdir "$WORK/payload"
    ditto "$APP" "$WORK/payload/$APP_NAME.app"
    ln -s /Applications "$WORK/payload/Applications"
    cp LICENSE "$WORK/payload/LICENSE.txt"

    hdiutil create -format UDZO -volname "$APP_NAME" -srcfolder "$WORK/payload" "$WORK/$NAME"
    codesign --sign "$TERN_SIGNING_IDENTITY" --timestamp "$WORK/$NAME"
    xcrun notarytool submit "$WORK/$NAME" --keychain-profile "$TERN_NOTARY_PROFILE" --wait
    xcrun stapler staple "$WORK/$NAME"
    xcrun stapler validate "$WORK/$NAME"
    spctl --assess --type open --context context:primary-signature --verbose=2 "$WORK/$NAME"
else
    NAME="$FILE_PREFIX.zip"
    ditto -c -k --keepParent "$APP" "$WORK/$FILE_PREFIX-submit.zip"
    xcrun notarytool submit "$WORK/$FILE_PREFIX-submit.zip" --keychain-profile "$TERN_NOTARY_PROFILE" --wait
    xcrun stapler staple "$APP"
    spctl --assess --type execute --verbose=2 "$APP"
    ditto -c -k --keepParent "$APP" "$WORK/$NAME"
fi

mv -f "$WORK/$NAME" "dist/$NAME"
echo "Notarized and packaged dist/$NAME"
