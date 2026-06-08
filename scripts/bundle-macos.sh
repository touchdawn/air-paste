#!/usr/bin/env bash
# Build a minimal AirPaste.app menu-bar bundle from the release tray binary.
#
# Usage:
#   scripts/bundle-macos.sh        # build release + assemble dist/AirPaste.app
#
# The bundle is a menu-bar accessory (LSUIElement = no Dock icon). The binary also sets
# NSApplicationActivationPolicy.accessory at runtime, so it behaves the same run bare or bundled.
# Drop an .icns at crates/airpaste-tray/assets/AppIcon.icns to give it a Finder icon (optional;
# an LSUIElement app has no Dock icon anyway).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP="dist/AirPaste.app"
BIN="target/release/airpaste-tray"

cargo build --release -p airpaste-tray

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/AirPaste"
chmod +x "$APP/Contents/MacOS/AirPaste"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>AirPaste</string>
    <key>CFBundleDisplayName</key><string>AirPaste</string>
    <key>CFBundleIdentifier</key><string>com.airpaste.tray</string>
    <key>CFBundleVersion</key><string>0.1.0</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleExecutable</key><string>AirPaste</string>
    <key>LSMinimumSystemVersion</key><string>10.15</string>
    <key>LSUIElement</key><true/>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

if [ -f "crates/airpaste-tray/assets/AppIcon.icns" ]; then
    cp "crates/airpaste-tray/assets/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
    /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string AppIcon" "$APP/Contents/Info.plist" \
        2>/dev/null || true
fi

echo "Built $APP"
echo "Run it:   open \"$ROOT/$APP\""
echo "Install:  cp -R \"$ROOT/$APP\" /Applications/   (then toggle 开机自启 in the window)"
