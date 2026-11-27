//! Embeds the application icon (`packaging/icons/icon.ico`) and the product
//! name in the Windows executable, so Explorer, shortcuts and the taskbar show
//! them. Nothing to do on other systems: macOS and Linux take the icon from
//! the package (see `packaging/`), and the window icon is compiled in from
//! `assets/icon-256.png`.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=../../packaging/icons/icon.ico");
    println!("cargo::rerun-if-changed=../xen-core/src/branding.rs");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows::embed_resources();
    }
}

#[cfg(windows)]
mod windows {
    use std::path::Path;

    pub fn embed_resources() {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
        let manifest_dir = Path::new(&manifest_dir);
        let icon = manifest_dir.join("../../packaging/icons/icon.ico");
        let name = product_name(&manifest_dir.join("../xen-core/src/branding.rs"));
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon(&icon.to_string_lossy())
            .set("ProductName", &name)
            .set("FileDescription", &name);
        if let Err(e) = resource.compile() {
            panic!("cannot embed the icon (needs the Windows SDK's resource compiler): {e}");
        }
    }

    /// `PRODUCT_NAME` from xen-core's branding module (the same way the
    /// packaging scripts read it).
    fn product_name(branding: &Path) -> String {
        let source = std::fs::read_to_string(branding).expect("xen-core/src/branding.rs");
        source
            .lines()
            .find_map(|line| {
                line.strip_prefix("pub const PRODUCT_NAME: &str = \"")?
                    .strip_suffix("\";")
            })
            .expect("PRODUCT_NAME in xen-core/src/branding.rs")
            .to_owned()
    }
}
