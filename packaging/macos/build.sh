#!/bin/sh
# Builds the macOS application bundle and disk image:
#   <out>/xenadmin-rs.app and <out>/xenadmin-rs-<version>-macos-<arch>.dmg
#
# Usage: packaging/macos/build.sh [out-dir]      (default: target/dist)
# Environment: TARGET=<rust target triple> to cross-build for the other
# architecture (e.g. x86_64-apple-darwin, after `rustup target add`).
#
# Needs only Xcode's command line tools (iconutil, sips, hdiutil). The
# bundle is not signed or notarized: Gatekeeper asks before the first start
# (right-click > Open, or `xattr -dr com.apple.quarantine xenadmin-rs.app`).
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-"$root/target/dist"}
mkdir -p "$out"
out=$(cd "$out" && pwd)
name=xenadmin-rs
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)

if [ -n "${TARGET:-}" ]; then
    cargo build --manifest-path "$root/Cargo.toml" --release --locked -p xenadmin --target "$TARGET"
    bin="$root/target/$TARGET/release/xenadmin"
    arch=${TARGET%%-*}
else
    cargo build --manifest-path "$root/Cargo.toml" --release --locked -p xenadmin
    bin="$root/target/release/xenadmin"
    arch=$(uname -m)
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Icon: every size iconutil wants, from the 1024 px master.
iconset="$work/$name.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z $size $size "$root/packaging/icons/icon-1024.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z $double $double "$root/packaging/icons/icon-1024.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done

app="$out/$name.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/xenadmin"
iconutil -c icns "$iconset" -o "$app/Contents/Resources/$name.icns"
cp "$root/LICENSE" "$app/Contents/Resources/LICENSE"
sed -e "s/@VERSION@/$version/g" "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null

# Disk image: the app and a link to /Applications.
stage="$work/dmg"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
dmg="$out/$name-$version-macos-$arch.dmg"
rm -f "$dmg"
hdiutil create -quiet -volname "$name $version" -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg"

echo "$app"
echo "$dmg"
