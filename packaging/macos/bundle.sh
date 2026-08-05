#!/usr/bin/env bash
#
# Builds ProtocolSimulator.app from the two macOS binaries.
#
# Kept out of the workflow so it can be run and checked on a real Mac:
#   packaging/macos/bundle.sh 0.1.0
#
# Expects both targets to have been built already:
#   cargo build --release --target aarch64-apple-darwin -p sim-gui
#   cargo build --release --target x86_64-apple-darwin  -p sim-gui

set -euo pipefail

VERSION="${1:-0.0.0}"
BIN=protocol-simulator
APP="target/ProtocolSimulator.app"
IDENTIFIER="dev.tdameros.protocol-simulator"

for arch in aarch64 x86_64; do
    binary="target/${arch}-apple-darwin/release/${BIN}"
    if [ ! -f "$binary" ]; then
        echo "missing $binary. Build that target first" >&2
        exit 1
    fi
done

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"

# One file that runs on both kinds of Mac, so there is a single download.
lipo -create -output "$APP/Contents/MacOS/$BIN" \
    "target/aarch64-apple-darwin/release/$BIN" \
    "target/x86_64-apple-darwin/release/$BIN"

# Without a bundle, double-clicking from Finder opens a Terminal window
# alongside the app and the Dock shows no proper name or icon.
cat >"$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>Protocol Simulator</string>
	<key>CFBundleDisplayName</key>
	<string>Protocol Simulator</string>
	<key>CFBundleIdentifier</key>
	<string>${IDENTIFIER}</string>
	<key>CFBundleExecutable</key>
	<string>${BIN}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>${VERSION}</string>
	<key>CFBundleVersion</key>
	<string>${VERSION}</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
PLIST

# lipo discards the ad-hoc signature the linker puts on the arm64 slice, and
# macOS flatly refuses to launch an unsigned arm64 binary. Sign it again, still
# ad hoc: this is not notarisation and Gatekeeper will still ask on first open.
codesign --force --sign - "$APP"
codesign --verify --strict "$APP"

echo "built $APP"
lipo -archs "$APP/Contents/MacOS/$BIN"
