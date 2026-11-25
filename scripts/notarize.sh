#!/bin/bash
# Submit the signed app to Apple, attach the ticket, and verify the final ZIP.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${NOTARY_PROFILE:?Set NOTARY_PROFILE to the name of a notarytool Keychain profile.}"
APP="$PWD/dist/MacSweep.app"
VERSION=$(/usr/bin/plutil -extract CFBundleShortVersionString raw -o - "$APP/Contents/Info.plist")
ARCHS=$(/usr/bin/lipo -archs "$APP/Contents/MacOS/MacSweep")
case "$ARCHS" in
    "x86_64 arm64"|"arm64 x86_64") ARCHS=universal ;;
esac
OUTPUT="$PWD/dist/MacSweep-${VERSION}-${ARCHS}.zip"
STAGE=$(mktemp -d "$PWD/dist/.notary.XXXXXXXX")
trap 'rm -rf "$STAGE"' EXIT
PROFILE=(--keychain-profile "$NOTARY_PROFILE")
if [[ -n "${NOTARY_KEYCHAIN:-}" ]]; then PROFILE+=(--keychain "$NOTARY_KEYCHAIN"); fi

echo "Checking the release signature and preparing the Apple submission."
/usr/bin/codesign --verify --deep --strict --all-architectures "$APP"
/usr/bin/ditto "$APP" "$STAGE/MacSweep.app"
/usr/bin/ditto -c -k --keepParent "$STAGE/MacSweep.app" "$STAGE/submission.zip"
xcrun notarytool submit "$STAGE/submission.zip" "${PROFILE[@]}" --output-format json > "$STAGE/submitted.json"
ID=$(/usr/bin/plutil -extract id raw -o - "$STAGE/submitted.json")
echo "Apple submission $ID. Waiting for notarization."
xcrun notarytool wait "$ID" "${PROFILE[@]}" || true
xcrun notarytool info "$ID" "${PROFILE[@]}" --output-format json > "$STAGE/result.json"
STATUS=$(/usr/bin/plutil -extract status raw -o - "$STAGE/result.json")
if [[ "$STATUS" != "Accepted" ]]; then
    echo "Apple submission $ID has status $STATUS." >&2
    if [[ "$STATUS" == "Invalid" ]]; then
        xcrun notarytool log "$ID" "${PROFILE[@]}" "$PWD/dist/notary-$ID.json"
    fi
    exit 1
fi
echo "Apple accepted $ID. Attaching the ticket and building the transfer archive."
xcrun stapler staple "$STAGE/MacSweep.app"
xcrun stapler validate "$STAGE/MacSweep.app"
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$STAGE/MacSweep.app" "$STAGE/final.zip"
mkdir "$STAGE/extracted"
/usr/bin/ditto -x -k "$STAGE/final.zip" "$STAGE/extracted"
RESTORED="$STAGE/extracted/MacSweep.app"
/usr/bin/codesign --verify --deep --strict --all-architectures "$RESTORED"
xcrun stapler validate "$RESTORED"
/usr/sbin/spctl --assess --type execute --verbose=3 "$RESTORED"
# Keep a verified archive even if another build has since replaced the local app.
mv "$STAGE/final.zip" "$OUTPUT"
/usr/bin/shasum -a 256 "$OUTPUT"
/bin/cp "$STAGE/result.json" "$PWD/dist/notary-$ID.json"
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
