#!/bin/bash
# the Info.plist for Liftoff.app, version as $1. one copy, shared by the release
# build (make-dmg.sh) and ./install.sh --local, so both make the same app
set -euo pipefail
v="${1:?version}"
cat <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>eu.instantnode.liftoff</string>
  <key>CFBundleName</key><string>Liftoff</string>
  <key>CFBundleDisplayName</key><string>Liftoff</string>
  <key>CFBundleExecutable</key><string>liftoff</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$v</string>
  <key>CFBundleVersion</key><string>$v</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PL
