#!/bin/bash
# Builds dist/MacSweep.app
#   ./bundle.sh              build for this Mac only
#   UNIVERSAL=1 ./bundle.sh  build one app for Apple Silicon + Intel
#   ZIP=1 ./bundle.sh        archive the build; a Developer ID archive requires NOTARY_PROFILE
set -euo pipefail
cd "$(dirname "$0")"

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
# Xcode opens with a minimal PATH. Prefer the explicitly selected compiler if present.
export PATH="${PATH}:/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin"
CARGO_BIN="${MACSWEEP_CARGO:-cargo}"
# Xcode 27 requires its external build driver to target 12+. Cargo links the app.
export MACOSX_DEPLOYMENT_TARGET="${MACSWEEP_DEPLOYMENT_TARGET:-11.0}"
mkdir -p dist
STAGE=$(mktemp -d "$PWD/dist/.bundle.XXXXXXXX")
trap 'rm -rf "$STAGE"' EXIT
APP="$STAGE/MacSweep.app"
# The menu bar monitor is a separate tiny app inside MacSweep.app, started as a
# login item only if the user turns it on.
HELPER="$APP/Contents/Library/LoginItems/MacSweep Monitor.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$HELPER/Contents/MacOS" "$HELPER/Contents/Resources"

if [[ "${UNIVERSAL:-0}" == "1" ]]; then
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
        libdir=$("${RUSTC:-rustc}" --print target-libdir --target "$target")
        if ! compgen -G "$libdir/libstd-*.rlib" >/dev/null; then
            rustup target add "$target"
        fi
    done
    "$CARGO_BIN" build --locked --release --workspace --target aarch64-apple-darwin
    "$CARGO_BIN" build --locked --release --workspace --target x86_64-apple-darwin
    lipo -create -output "$APP/Contents/MacOS/MacSweep" \
        target/aarch64-apple-darwin/release/macsweep \
        target/x86_64-apple-darwin/release/macsweep
    lipo -create -output "$HELPER/Contents/MacOS/macsweep-monitor" \
        target/aarch64-apple-darwin/release/macsweep-monitor \
        target/x86_64-apple-darwin/release/macsweep-monitor
else
    "$CARGO_BIN" build --locked --release --workspace
    cp target/release/macsweep "$APP/Contents/MacOS/MacSweep"
    cp target/release/macsweep-monitor "$HELPER/Contents/MacOS/macsweep-monitor"
fi

cp assets/AppIcon.icns "$HELPER/Contents/Resources/AppIcon.icns"
cat > "$HELPER/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>MacSweep Monitor</string>
    <key>CFBundleDisplayName</key><string>MacSweep Monitor</string>
    <key>CFBundleIdentifier</key><string>me.i0532386223.macsweep.monitor</string>
    <key>CFBundleExecutable</key><string>macsweep-monitor</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundleVersion</key><string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key><string>${MACOSX_DEPLOYMENT_TARGET}</string>
    <key>LSUIElement</key><true/>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>MacSweep</string>
    <key>CFBundleDisplayName</key><string>MacSweep</string>
    <key>CFBundleIdentifier</key><string>me.i0532386223.macsweep</string>
    <key>CFBundleExecutable</key><string>MacSweep</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundleVersion</key><string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key><string>${MACOSX_DEPLOYMENT_TARGET}</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSAppleEventsUsageDescription</key><string>MacSweep asks Finder to move selected items to the Trash.</string>
</dict>
</plist>
PLIST

# Sign. With a stable local certificate (scripts/setup-signing.sh) macOS keeps
# Full Disk Access across rebuilds; otherwise fall back to an ad-hoc signature,
# which macOS treats as a brand-new app after every build.
SIGN_ID="${MACSWEEP_SIGN_IDENTITY:-}"
if [[ -z "$SIGN_ID" && -n "${MACSWEEP_SIGN_TEAM:-}" && "${ADHOC:-0}" != "1" ]]; then
    SIGN_ID=$(security find-identity -v -p codesigning | awk -v team="($MACSWEEP_SIGN_TEAM)" '
        /"Developer ID Application: / && index($0, team) && !id { id = $2 }
        END { print id }')
    if [[ -z "$SIGN_ID" ]]; then
        echo "A Developer ID Application certificate for team $MACSWEEP_SIGN_TEAM is required." >&2
        exit 1
    fi
fi
DEVELOPER_SIGNED=0
if [[ -n "$SIGN_ID" ]]; then
    DEVELOPER_SIGNED=1
    # The inner helper requires no Apple events. Only the host can ask Finder to move an item.
    codesign --force --options runtime --timestamp --sign "$SIGN_ID" "$HELPER"
    codesign --force --options runtime --timestamp --entitlements signing/MacSweep.entitlements --sign "$SIGN_ID" "$APP"
else
    SIGN_ID="-"
    if [[ "${ADHOC:-0}" != "1" ]] && security find-identity -p codesigning 2>/dev/null | grep -q "MacSweep Local Signing"; then
        SIGN_ID="MacSweep Local Signing"
    fi
    codesign --force --sign "$SIGN_ID" "$HELPER"
    codesign --force --sign "$SIGN_ID" "$APP"
fi
codesign --verify --strict "$HELPER"
codesign --verify --deep --strict "$APP"

# Replace only the previous application after the new signatures pass verification.
if [[ -e dist/MacSweep.app ]]; then
    mv dist/MacSweep.app "$STAGE/previous.app"
fi
if ! mv "$APP" dist/MacSweep.app; then
    if [[ -d "$STAGE/previous.app" ]]; then mv "$STAGE/previous.app" dist/MacSweep.app; fi
    exit 1
fi
echo "Built dist/MacSweep.app v${VERSION}. Signing identity: $SIGN_ID"

if [[ "${ZIP:-0}" == "1" ]]; then
    if [[ "$DEVELOPER_SIGNED" == "1" ]]; then
        exec ./scripts/notarize.sh
    fi
    (cd dist && ditto -c -k --keepParent MacSweep.app "MacSweep-${VERSION}.zip")
    echo "Packed dist/MacSweep-${VERSION}.zip"
fi
