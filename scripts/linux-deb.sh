#!/bin/sh
# Packages target/release/Glasir (build it first) as a .deb: /usr/bin/glasir, its launcher entry
# and icon. Needs dpkg-deb (Debian / Ubuntu).
#   scripts/linux-deb.sh dist   -> dist/Glasir-linux-amd64.deb
set -eu
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
root=$(mktemp -d)
mkdir -p "$root/usr/bin" "$root/usr/share/icons/hicolor/512x512/apps" "$root/usr/share/applications" "$root/DEBIAN"
cp "${CARGO_TARGET_DIR:-target}/release/Glasir" "$root/usr/bin/glasir"
cp assets/icon-512.png "$root/usr/share/icons/hicolor/512x512/apps/glasir.png"
chmod 755 "$root" "$root/usr/bin/glasir"
cat > "$root/usr/share/applications/Glasir.desktop" <<END
[Desktop Entry]
Type=Application
Name=Glasir
Comment=Git client
Exec=glasir
Icon=glasir
Terminal=false
Categories=Development;RevisionControl;
StartupWMClass=Glasir
END
cat > "$root/DEBIAN/control" <<END
Package: glasir
Version: $version
Architecture: amd64
Maintainer: ChanVuth Chea <cchvuth@gmail.com>
Depends: git
Section: devel
Priority: optional
Homepage: https://github.com/cchvuth/Glasir
Description: Fast, free and open-source Git client
 Glasir is a Git client with a modern commit graph and smart, time-saving Git operations.
END
mkdir -p "${1:-dist}"
dpkg-deb --build --root-owner-group "$root" "${1:-dist}/Glasir-linux-amd64.deb"
rm -rf "$root"
