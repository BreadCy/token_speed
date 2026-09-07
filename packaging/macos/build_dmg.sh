#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
MANIFEST="$ROOT/tokenspeed-rs/Cargo.toml"
VERSION=$(awk -F '"' '/^version = / { print $2; exit }' "$MANIFEST")
STAMP=$(date +%Y%m%d-%H%M%S)
OUT="$ROOT/dist"
STAGE=$(mktemp -d "${TMPDIR:-/tmp}/tokenspeed-dmg.XXXXXX")
APP="$STAGE/TokenSpeed.app"
DMG="$OUT/TokenSpeed-${VERSION}-macos-unsigned-${STAMP}.dmg"

cleanup() {
  rm -rf "$STAGE"
}
trap cleanup EXIT INT TERM

mkdir -p "$OUT" "$APP/Contents/MacOS" "$APP/Contents/Resources"
cargo build --release --locked --manifest-path "$MANIFEST"
cp "$ROOT/tokenspeed-rs/target/release/tokenspeed" "$APP/Contents/MacOS/tokenspeed"
chmod 755 "$APP/Contents/MacOS/tokenspeed"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDisplayName</key><string>TokenSpeed</string>
  <key>CFBundleExecutable</key><string>tokenspeed</string>
  <key>CFBundleIdentifier</key><string>local.tokenspeed.monitor</string>
  <key>CFBundleName</key><string>TokenSpeed</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.5.10</string>
  <key>CFBundleVersion</key><string>0.5.10</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
</dict>
</plist>
PLIST

hdiutil create -volname "TokenSpeed" -srcfolder "$STAGE" -format UDZO "$DMG" >/dev/null
printf '%s\n' "$DMG"
