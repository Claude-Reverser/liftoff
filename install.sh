#!/bin/zsh
# Build Liftoff, bundle it into /Applications/Liftoff.app and (re)start it at login.
set -e
cd "${0:a:h}"
cargo build --release
if [[ ! -f assets/AppIcon.icns ]]; then
  # apple's icon, pulled off this mac. not shipping their art in the repo
  T=$(mktemp -d)
  swift scripts/exporticon.swift /System/Applications/Apps.app "$T/AppIcon.iconset"
  mkdir -p assets && iconutil -c icns "$T/AppIcon.iconset" -o assets/AppIcon.icns
  rm -rf "$T"
fi
APP=/Applications/Liftoff.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/liftoff "$APP/Contents/MacOS/liftoff"
[[ -f assets/AppIcon.icns ]] && cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
cat > "$APP/Contents/Info.plist" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>app.liftoff.launcher</string>
  <key>CFBundleName</key><string>Liftoff</string>
  <key>CFBundleExecutable</key><string>liftoff</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PL
codesign --force --sign - "$APP" >/dev/null 2>&1

AGENT=~/Library/LaunchAgents/app.liftoff.launcher.plist
cat > "$AGENT" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>app.liftoff.launcher</string>
  <key>ProgramArguments</key><array><string>$APP/Contents/MacOS/liftoff</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardErrorPath</key><string>/tmp/liftoff.log</string>
</dict></plist>
PL
launchctl bootout gui/$(id -u)/app.liftoff.launcher 2>/dev/null || true
launchctl bootstrap gui/$(id -u) "$AGENT"
echo "installed: $APP (hotkeys in ~/Library/Application Support/Liftoff/hotkeys)"
