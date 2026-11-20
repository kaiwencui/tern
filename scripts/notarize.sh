#!/bin/zsh
# Usage: scripts/notarize.sh [zip|dmg]  (default: zip)
# Builds a Developer ID signed, notarized and stapled release in dist/.
#
# Required environment variables:
#   TERN_SIGNING_IDENTITY   e.g. "Developer ID Application: Your Name (TEAMID)"
#   TERN_NOTARY_PROFILE     keychain profile saved via `xcrun notarytool store-credentials`
set -euo pipefail
cd "$(dirname "$0")/.."

FORMAT="${1:-zip}"
case "$FORMAT" in
    zip|dmg) ;;
    *) echo "Usage: $0 [zip|dmg]" >&2; exit 2 ;;
esac

: "${TERN_SIGNING_IDENTITY:?set TERN_SIGNING_IDENTITY to your Developer ID Application certificate}"
: "${TERN_NOTARY_PROFILE:?set TERN_NOTARY_PROFILE to your notarytool keychain profile}"

./scripts/build-app.sh release

APP="build/Tern.app"
if ! codesign -d --verbose=2 "$APP" 2>&1 | awk '/^Authority=Developer ID Application:/ { signed = 1 } END { exit !signed }'; then
    echo "Tern.app must be signed with a Developer ID Application certificate" >&2
    exit 1
fi

mkdir -p dist
WORK=$(mktemp -d "$PWD/dist/.notarize.XXXXXXXX")
trap 'rm -rf -- "$WORK"' EXIT

if [[ "$FORMAT" == dmg ]]; then
    VERSION=$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist")
    NAME="Tern-$VERSION-$(uname -m).dmg"
    mkdir "$WORK/payload"
    ditto "$APP" "$WORK/payload/Tern.app"
    ln -s /Applications "$WORK/payload/Applications"
    cp LICENSE "$WORK/payload/LICENSE.txt"

    hdiutil create -format UDZO -volname Tern -srcfolder "$WORK/payload" "$WORK/$NAME"
    codesign --sign "$TERN_SIGNING_IDENTITY" --timestamp "$WORK/$NAME"
    xcrun notarytool submit "$WORK/$NAME" --keychain-profile "$TERN_NOTARY_PROFILE" --wait
    xcrun stapler staple "$WORK/$NAME"
    xcrun stapler validate "$WORK/$NAME"
    spctl --assess --type open --context context:primary-signature --verbose=2 "$WORK/$NAME"
else
    NAME="Tern.zip"
    ditto -c -k --keepParent "$APP" "$WORK/Tern-submit.zip"
    xcrun notarytool submit "$WORK/Tern-submit.zip" --keychain-profile "$TERN_NOTARY_PROFILE" --wait
    xcrun stapler staple "$APP"
    spctl --assess --type execute --verbose=2 "$APP"
    ditto -c -k --keepParent "$APP" "$WORK/$NAME"
fi

mv -f "$WORK/$NAME" "dist/$NAME"
echo "Notarized and packaged dist/$NAME"
