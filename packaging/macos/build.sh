#!/bin/sh
# Builds the macOS application bundle and disk image:
#   "<out>/ReXen Center.app" and <out>/rexen-center-<version>-macos-<arch>.dmg
# (the names come from crates/xen-core/src/branding.rs, see ../brand.sh).
#
# Usage: packaging/macos/build.sh [out-dir]      (default: target/dist)
# Environment: TARGET=<rust target triple> to cross-build for the other
# architecture (e.g. x86_64-apple-darwin, after `rustup target add`).
#
# Needs only Xcode's command line tools (iconutil, hdiutil, plutil,
# codesign). The bundle is ad-hoc signed, not signed with an Apple Developer
# ID or notarized: Gatekeeper says Apple could not verify it and blocks the
# first start until System Settings > Privacy & Security > Open Anyway
# (right-click > Open on macOS 14 and earlier), or
# `xattr -dr com.apple.quarantine "ReXen Center.app"`. Without the ad-hoc
# signature of the whole bundle, the executable keeps only the linker's
# signature, which seals neither Info.plist nor Resources, and macOS calls
# the downloaded app "damaged" instead, with no Open Anyway.
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
    # Apple's name, as `uname -m` prints it on Apple Silicon.
    [ "$arch" = aarch64 ] && arch=arm64
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

# Ad-hoc signature of the assembled bundle (last, as nothing may change it
# afterwards): it replaces the linker's signature of the executable, seals
# Resources and binds Info.plist; the signing identifier is the bundle's
# CFBundleIdentifier. The bundle has a single Mach-O, so no --deep.
codesign --force --sign - "$app"
verify() {
    if ! codesign --verify --deep --strict --verbose=2 "$1"; then
        echo "the code signature of $1 does not verify" >&2
        exit 1
    fi
}
verify "$app"

# Disk image: the app and a link to /Applications.
stage="$work/dmg"
mkdir -p "$stage"
ditto "$app" "$stage/$brand_name.app"
ln -s /Applications "$stage/Applications"
dmg="$out/$brand_id-$version-macos-$arch.dmg"
rm -f "$dmg"
hdiutil create -quiet -volname "$brand_name $version" -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg"

# The app as users get it: from the disk image.
mnt="$work/mnt"
mkdir -p "$mnt"
hdiutil attach -nobrowse -readonly -noautoopen -quiet -mountpoint "$mnt" "$dmg"
status=0
( verify "$mnt/$brand_name.app" ) || status=1
hdiutil detach -quiet "$mnt" || hdiutil detach -quiet -force "$mnt"
if [ "$status" -ne 0 ]; then
    rm -f "$dmg"
    exit 1
fi

echo "$app"
echo "$dmg"
