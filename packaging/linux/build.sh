#!/bin/sh
# Builds the Linux packages (not built or run on the development machine;
# see README.md):
#   <out>/rexen-center-<version>-linux-<arch>.tar.gz   always
#   <out>/rexen-center_<version>_<debarch>.deb          when dpkg-deb exists
#   <out>/rexen-center-<version>-<arch>.AppImage        when appimagetool exists
# (the names come from crates/xen-core/src/branding.rs, see ../brand.sh).
#
# Usage: packaging/linux/build.sh [out-dir]      (default: target/dist)
#
# Build needs: Rust, a C compiler, and pkg-config with the X11/Wayland and
# xkbcommon headers eframe uses (Debian/Ubuntu: build-essential pkg-config
# libx11-dev libxcursor-dev libxrandr-dev libxi-dev libxkbcommon-dev
# libwayland-dev libgl1-mesa-dev). At run time the file dialogs use the
# XDG desktop portal (xdg-desktop-portal with a GTK or KDE backend), and the
# saved passwords the Secret Service (GNOME Keyring or KWallet).
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-"$root/target/dist"}
mkdir -p "$out"
out=$(cd "$out" && pwd)
. "$root/packaging/brand.sh"
name=$brand_id
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
arch=$(uname -m)

cargo build --manifest-path "$root/Cargo.toml" --release --locked -p xenadmin
bin="$root/target/release/$brand_bin"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# A /usr-style tree shared by the tarball, the .deb and the AppImage.
tree="$work/tree"
install -Dm755 "$bin" "$tree/usr/bin/$brand_bin"
install -Dm644 "$root/packaging/linux/$name.desktop" "$tree/usr/share/applications/$name.desktop"
for size in 16 24 32 48 64 128 256 512 1024; do
    install -Dm644 "$root/packaging/icons/png/icon-$size.png" "$tree/usr/share/icons/hicolor/${size}x${size}/apps/$name.png"
done
install -Dm644 "$root/LICENSE" "$tree/usr/share/doc/$name/copyright"
install -Dm644 "$root/README.md" "$tree/usr/share/doc/$name/README.md"

# Tarball: unpack anywhere, run bin/rexencenter.
tar_dir="$work/$name-$version"
mkdir -p "$tar_dir"
cp -R "$tree/usr/." "$tar_dir/"
tar -C "$work" -czf "$out/$name-$version-linux-$arch.tar.gz" "$name-$version"
echo "$out/$name-$version-linux-$arch.tar.gz"

if command -v dpkg-deb >/dev/null 2>&1; then
    debarch=$(dpkg --print-architecture)
    deb="$work/deb"
    cp -R "$tree" "$deb"
    mkdir -p "$deb/DEBIAN"
    size=$(du -sk "$deb/usr" | cut -f1)
    sed -e "s/@VERSION@/$version/" -e "s/@ARCH@/$debarch/" -e "s/@SIZE@/$size/" \
        -e "s/@ID@/$brand_id/" -e "s/@NAME@/$brand_name/" -e "s/@SUBTITLE@/$brand_subtitle/" \
        "$root/packaging/linux/control" > "$deb/DEBIAN/control"
    dpkg-deb --root-owner-group --build "$deb" "$out/${name}_${version}_${debarch}.deb"
    echo "$out/${name}_${version}_${debarch}.deb"
fi

if command -v appimagetool >/dev/null 2>&1; then
    appdir="$work/$name.AppDir"
    cp -R "$tree" "$appdir"
    cp "$root/packaging/linux/$name.desktop" "$appdir/$name.desktop"
    cp "$root/packaging/icons/png/icon-256.png" "$appdir/$name.png"
    printf '#!/bin/sh\nexec "$(dirname "$(readlink -f "$0")")/usr/bin/%s" "$@"\n' "$brand_bin" > "$appdir/AppRun"
    chmod +x "$appdir/AppRun"
    ARCH=$arch appimagetool "$appdir" "$out/$name-$version-$arch.AppImage"
    echo "$out/$name-$version-$arch.AppImage"
fi
