#!/bin/sh
# Wraps the release binary in ScreenTalk.app and zips it: ScreenTalk-macos.zip.
# A bundle is needed because macOS only asks for microphone access on behalf of an app
# that declares why it wants it (NSMicrophoneUsageDescription in Info.plist).
# The signature is ad hoc (no developer account): macOS runs it, but on first launch the
# user has to right-click > Open, or allow it in System Settings > Privacy & Security.
set -eu

# Without an explicit binary, build the current source first so the bundle is never stale.
[ $# -ge 1 ] || cargo build --release -p client
BIN="${1:-target/release/client}"
OUT="${2:-target/bundle}"
APP="$OUT/ScreenTalk.app"

rm -rf "$APP" "$OUT/ScreenTalk-macos.zip"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/client"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key><string>ScreenTalk</string>
	<key>CFBundleDisplayName</key><string>ScreenTalk</string>
	<key>CFBundleIdentifier</key><string>app.screentalk.client</string>
	<key>CFBundleExecutable</key><string>client</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleVersion</key><string>1</string>
	<key>CFBundleShortVersionString</key><string>0.1.0</string>
	<key>LSMinimumSystemVersion</key><string>13.0</string>
	<key>NSHighResolutionCapable</key><true/>
	<key>NSMicrophoneUsageDescription</key>
	<string>ScreenTalk sends your voice to the people in your room.</string>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "$APP"
ditto -c -k --keepParent "$APP" "$OUT/ScreenTalk-macos.zip"
echo "$OUT/ScreenTalk-macos.zip"
