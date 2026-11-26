# Sourced by the packaging scripts: the product's identity, read from
# crates/xen-core/src/branding.rs (the one place it is defined).
#   brand_name      ReXen Center      (display name, .app name)
#   brand_subtitle  the tagline
#   brand_id        rexen-center      (package, file and desktop-entry names)
#   brand_bin       rexencenter       (the executable)
# Needs $root (the workspace).
brand_const() {
    sed -n "s/^pub const $1: &str = \"\(.*\)\";\$/\1/p" "$root/crates/xen-core/src/branding.rs"
}
brand_name=$(brand_const PRODUCT_NAME)
brand_subtitle=$(brand_const SUBTITLE)
brand_id=$(brand_const APP_ID)
brand_bin=$(brand_const BINARY_NAME)
if [ -z "$brand_name" ] || [ -z "$brand_subtitle" ] || [ -z "$brand_id" ] || [ -z "$brand_bin" ]; then
    echo "cannot read the product identity from crates/xen-core/src/branding.rs" >&2
    exit 1
fi
