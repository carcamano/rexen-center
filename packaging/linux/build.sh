#!/bin/sh
# Builds the Linux packages (the release workflow runs it on Ubuntu 22.04,
# so the binary needs glibc 2.35 or newer):
#   <out>/rexen-center-<version>-linux-<arch>.tar.gz   always
#   <out>/rexen-center_<version>-1_<debarch>.deb        when cargo-deb is installed
#   <out>/rexen-center-<version>-1.<arch>.rpm           when cargo-generate-rpm is installed
#   <out>/rexen-center-<version>-<arch>.AppImage        when appimagetool is installed
# (the names come from crates/xen-core/src/branding.rs, see ../brand.sh; the
# .deb and .rpm contents and dependencies from [package.metadata.deb] and
# [package.metadata.generate-rpm] in crates/xenadmin/Cargo.toml).
#
# Usage: packaging/linux/build.sh [out-dir]      (default: target/dist)
#
# Build needs: Rust, a C compiler, pkg-config and the D-Bus headers for the
# keyring (Debian/Ubuntu: build-essential pkg-config libdbus-1-dev; Fedora:
# gcc pkgconf-pkg-config dbus-devel). eframe loads its X11, Wayland and
# OpenGL libraries at run time, so it needs no headers to build. The .deb
# dependencies are computed with dpkg-shlibdeps (package dpkg-dev), the .rpm
# ones with rpm's find-requires (package rpm; without it cargo-generate-rpm
# falls back to ldd, which also lists the libraries' own dependencies).
# Packaging tools: cargo install --locked cargo-deb cargo-generate-rpm
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-"$root/target/dist"}
mkdir -p "$out"
out=$(cd "$out" && pwd)
. "$root/packaging/brand.sh"
name=$brand_id
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
arch=$(uname -m)

# The package metadata repeats the product identity; refuse to build
# packages under a stale name.
manifest="$root/crates/xenadmin/Cargo.toml"
for want in "name = \"$brand_id\"" "target/release/$brand_bin\"" "packaging/linux/$brand_id.desktop\""; do
    if ! grep -qF "$want" "$manifest"; then
        echo "crates/xenadmin/Cargo.toml: the Linux package metadata does not match branding.rs (no $want)" >&2
        exit 1
    fi
done

cargo build --manifest-path "$root/Cargo.toml" --release --locked -p xenadmin
target_dir=${CARGO_TARGET_DIR:-"$root/target"}
bin="$target_dir/release/$brand_bin"
# The packages ship the stripped binary (cargo-generate-rpm does not strip).
strip "$bin"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# A /usr-style tree for the tarball and the AppImage (the .deb and .rpm
# list the same files in their metadata).
tree="$work/tree"
install -Dm755 "$bin" "$tree/usr/bin/$brand_bin"
install -Dm644 "$root/packaging/linux/$name.desktop" "$tree/usr/share/applications/$name.desktop"
install -Dm644 "$root/packaging/icons/icon.svg" "$tree/usr/share/icons/hicolor/scalable/apps/$name.svg"
for size in 16 24 32 48 64 128 256 512; do
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

if cargo deb --version >/dev/null 2>&1; then
    (cd "$root" && cargo deb -p xenadmin --no-build --output "$out")
fi

if cargo generate-rpm --version >/dev/null 2>&1; then
    (cd "$root" && cargo generate-rpm -p crates/xenadmin --output "$out")
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
