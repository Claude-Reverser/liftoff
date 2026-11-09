#!/bin/zsh
# builds dist/Liftoff-<version>.dmg with the original (non-apple) icon
set -e
cd "${0:a:h}/.."
VER=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
RUSTFLAGS="-C target-cpu=apple-m1" cargo build --release
T=$(mktemp -d)
APP="$T/stage/Liftoff.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/liftoff "$APP/Contents/MacOS/liftoff"
swift scripts/liftoff-icon.swift "$T/AppIcon.iconset"
iconutil -c icns "$T/AppIcon.iconset" -o "$APP/Contents/Resources/AppIcon.icns"
cat > "$APP/Contents/Info.plist" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>eu.instantnode.liftoff</string>
  <key>CFBundleName</key><string>Liftoff</string>
  <key>CFBundleExecutable</key><string>liftoff</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VER</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PL
codesign --force --sign - "$APP"
ln -s /Applications "$T/stage/Applications"
mkdir -p dist
rm -f "dist/Liftoff-$VER.dmg"
hdiutil create -volname Liftoff -srcfolder "$T/stage" -ov -format UDZO -fs HFS+ "dist/Liftoff-$VER.dmg" >/dev/null
rm -rf "$T"
echo "dist/Liftoff-$VER.dmg"
