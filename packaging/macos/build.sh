#!/bin/sh
# Builds the macOS application bundle and disk image:
#   "<out>/ReXen Center.app" and <out>/rexen-center-<version>-macos-<arch>.dmg
# (the names come from crates/xen-core/src/branding.rs, see ../brand.sh).
#
# Usage: packaging/macos/build.sh [out-dir]      (default: target/dist)
# Environment: TARGET=<rust target triple> to cross-build for the other
# architecture (e.g. x86_64-apple-darwin, after `rustup target add`).
#
# Needs only Xcode's command line tools (iconutil, hdiutil, plutil). The
# bundle is not signed or notarized: Gatekeeper asks before the first start
# (right-click > Open, or `xattr -dr com.apple.quarantine "ReXen Center.app"`).
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-"$root/target/dist"}
mkdir -p "$out"
out=$(cd "$out" && pwd)
. "$root/packaging/brand.sh"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)

if [ -n "${TARGET:-}" ]; then
    cargo build --manifest-path "$root/Cargo.toml" --release --locked -p xenadmin --target "$TARGET"
    bin="$root/target/$TARGET/release/$brand_bin"
    arch=${TARGET%%-*}
else
    cargo build --manifest-path "$root/Cargo.toml" --release --locked -p xenadmin
    bin="$root/target/release/$brand_bin"
    arch=$(uname -m)
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Icon: every size iconutil wants, rendered from the SVG source
# (packaging/icons/render.sh; 16 px from the version drawn for small sizes).
iconset="$work/$brand_id.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    cp "$root/packaging/icons/png/icon-$size.png" "$iconset/icon_${size}x${size}.png"
    cp "$root/packaging/icons/png/icon-$((size * 2)).png" "$iconset/icon_${size}x${size}@2x.png"
done

app="$out/$brand_name.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/$brand_bin"
iconutil -c icns "$iconset" -o "$app/Contents/Resources/$brand_id.icns"
cp "$root/LICENSE" "$app/Contents/Resources/LICENSE"
sed -e "s/@VERSION@/$version/g" -e "s/@NAME@/$brand_name/g" -e "s/@SUBTITLE@/$brand_subtitle/g" \
    -e "s/@ID@/$brand_id/g" -e "s/@BIN@/$brand_bin/g" \
    "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null

# Disk image: the app and a link to /Applications.
stage="$work/dmg"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
dmg="$out/$brand_id-$version-macos-$arch.dmg"
rm -f "$dmg"
hdiutil create -quiet -volname "$brand_name $version" -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg"

echo "$app"
echo "$dmg"
