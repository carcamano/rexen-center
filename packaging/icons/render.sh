#!/bin/sh
# Renders ReXen Center's icon from its SVG sources into the files the app and
# the packages use (see README.md in this folder):
#   png/icon-<size>.png             16 to 1024 px (macOS .icns, Linux hicolor)
#   icon.ico                        16 to 256 px (Windows)
#   ../../crates/xenadmin/assets/icon-256.png   (window icon and About)
# 16 and 24 px come from icon-small.svg, every other size from icon.svg.
#
# Usage: packaging/icons/render.sh      (builds render/ with cargo first)
set -eu

here=$(cd "$(dirname "$0")" && pwd)
cargo build --quiet --release --manifest-path "$here/render/Cargo.toml"
tool="$here/render/target/release/icon-render"

mkdir -p "$here/png"
for size in 16 24; do
    "$tool" png "$here/icon-small.svg" $size "$here/png/icon-$size.png"
done
for size in 32 48 64 128 256 512 1024; do
    "$tool" png "$here/icon.svg" $size "$here/png/icon-$size.png"
done
"$tool" ico "$here/icon.ico" $(for size in 16 24 32 48 64 128 256; do echo "$here/png/icon-$size.png"; done)
cp "$here/png/icon-256.png" "$here/../../crates/xenadmin/assets/icon-256.png"
