#!/bin/zsh
# builds the release files in dist/:
#   Liftoff-<version>.dmg            drag-to-Applications disk image, for people
#   Liftoff-macos-arm64.zip          the bare app, what install.sh downloads
#   Liftoff-macos-arm64.zip.sha256   its checksum, which install.sh checks
# with the original (non-apple) icon
set -e
cd "${0:a:h}/.."
VER=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
# runs on every apple silicon mac, not just this one
RUSTFLAGS="-C target-cpu=apple-m1" cargo build --release
T=$(mktemp -d)
APP="$T/stage/Liftoff.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/liftoff "$APP/Contents/MacOS/liftoff"
swift scripts/liftoff-icon.swift "$T/AppIcon.iconset"
iconutil -c icns "$T/AppIcon.iconset" -o "$APP/Contents/Resources/AppIcon.icns"
scripts/info-plist.sh "$VER" > "$APP/Contents/Info.plist"
codesign --force --sign - "$APP"
mkdir -p dist
rm -f "dist/Liftoff-$VER.dmg" dist/Liftoff-macos-arm64.zip dist/Liftoff-macos-arm64.zip.sha256
ditto -c -k --keepParent "$APP" dist/Liftoff-macos-arm64.zip
(cd dist && shasum -a 256 Liftoff-macos-arm64.zip > Liftoff-macos-arm64.zip.sha256)
ln -s /Applications "$T/stage/Applications"
hdiutil create -volname Liftoff -srcfolder "$T/stage" -ov -format UDZO -fs HFS+ "dist/Liftoff-$VER.dmg" >/dev/null
rm -rf "$T"
ls -1 dist/Liftoff-$VER.dmg dist/Liftoff-macos-arm64.zip dist/Liftoff-macos-arm64.zip.sha256
