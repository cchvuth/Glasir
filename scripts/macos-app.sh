#!/bin/sh
# Builds Glasir.app (release) from this checkout; with --install, also copies
# it to /Applications. Needs macOS (sips, iconutil, codesign) and Rust.
#   scripts/macos-app.sh            -> target/release/Glasir.app
#   scripts/macos-app.sh --install  -> also /Applications/Glasir.app
set -eu
cd "$(dirname "$0")/.."
target="${CARGO_TARGET_DIR:-target}"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

cargo build --release
app="$target/release/Glasir.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target/release/Glasir" "$app/Contents/MacOS/Glasir"

# Icon: every size macOS asks for, from the 1024 px master.
set_dir=$(mktemp -d)/Glasir.iconset
mkdir -p "$set_dir"
for s in 16 32 128 256 512; do
  sips -z $s $s assets/icon.png --out "$set_dir/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) assets/icon.png --out "$set_dir/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$set_dir" -o "$app/Contents/Resources/Glasir.icns"
rm -rf "$(dirname "$set_dir")"

cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Glasir</string>
  <key>CFBundleDisplayName</key><string>Glasir</string>
  <key>CFBundleIdentifier</key><string>io.github.cchvuth.glasir</string>
  <key>CFBundleExecutable</key><string>Glasir</string>
  <key>CFBundleIconFile</key><string>Glasir</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
# Ad-hoc signature for the whole bundle (Apple Silicon refuses unsigned code).
codesign --force --sign - "$app"
echo "built $app"

if [ "${1:-}" = "--install" ]; then
  # Replace in place: a running Glasir keeps its old files until it restarts.
  rsync -a --delete "$app/" /Applications/Glasir.app/
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f /Applications/Glasir.app
  echo "installed /Applications/Glasir.app"
fi
