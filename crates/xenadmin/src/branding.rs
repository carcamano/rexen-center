//! Product identity. The name, subtitle and identifiers are defined once, in
//! [`xen_core::branding`] (shared with the MCP server and the packaging
//! scripts); this module adds what only the desktop app needs.

pub use xen_core::branding::{APP_ID, PRODUCT_NAME, SUBTITLE};

/// The server platform the console manages (the original's
/// `BrandManager.ProductBrand`).
pub const PLATFORM_NAME: &str = "XCP-ng";

/// Product version (from Cargo).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// SPDX identifier of the product license.
pub const LICENSE_ID: &str = "BSD-2-Clause";

/// Full license text shown in the About window.
pub const LICENSE_TEXT: &str = include_str!("../../../LICENSE");

/// The project's page (from Cargo: the workspace's `repository`).
pub const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// The product as the updater knows it: the names of its release packages
/// and the GitHub repository they are published in.
pub fn update_product() -> xen_update::Product {
    let repository = REPOSITORY_URL
        .trim_end_matches('/')
        .trim_start_matches("https://github.com/");
    xen_update::Product::new(
        APP_ID,
        xen_core::branding::BINARY_NAME,
        PRODUCT_NAME,
        repository,
    )
}

/// Login originator: identifies this client in the server's logs.
pub fn originator() -> String {
    format!("{APP_ID}/{VERSION}")
}

/// The application icon, 256 px (rendered from the SVG source in
/// `packaging/icons/`, see its README): the window and taskbar icon and the
/// About window's.
pub const ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");

/// [`ICON_PNG`] decoded, for the window's icon.
pub fn icon_data() -> Result<egui::IconData, String> {
    eframe::icon_data::from_png_bytes(ICON_PNG).map_err(|e| e.to_string())
}

/// [`ICON_PNG`] as an image to draw (a transparent pixel if it cannot be
/// decoded, which the tests rule out).
pub fn icon_image() -> egui::ColorImage {
    match icon_data() {
        Ok(icon) => egui::ColorImage::from_rgba_unmultiplied(
            [icon.width as usize, icon.height as usize],
            &icon.rgba,
        ),
        Err(e) => {
            tracing::warn!(error = %e, "application icon");
            egui::ColorImage::filled([1, 1], egui::Color32::TRANSPARENT)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The packaged icons (`packaging/icons/`): every PNG decodes at its
    /// size, the window icon is the 256 px one, and the Windows icon holds
    /// 16 to 256 px.
    #[test]
    fn packaged_icons_load() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packaging/icons");
        for size in [16, 24, 32, 48, 64, 128, 256, 512, 1024] {
            let path = format!("{dir}/png/icon-{size}.png");
            let bytes = std::fs::read(&path).expect(&path);
            let icon = eframe::icon_data::from_png_bytes(&bytes).expect(&path);
            assert_eq!((icon.width, icon.height), (size, size), "{path}");
            if size == 256 {
                assert_eq!(bytes, ICON_PNG, "assets/icon-256.png is the packaged one");
            }
        }
        let ico = std::fs::read(format!("{dir}/icon.ico")).expect("icon.ico");
        assert_eq!(ico[..4], [0, 0, 1, 0], "an icon file");
        let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
        let sizes: Vec<u32> = (0..count)
            .map(|i| match ico[6 + 16 * i] {
                0 => 256,
                n => u32::from(n),
            })
            .collect();
        assert_eq!(sizes, [16, 24, 32, 48, 64, 128, 256]);
        for i in 0..count {
            let entry = &ico[6 + 16 * i..6 + 16 * (i + 1)];
            let len = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
            let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
            let icon = eframe::icon_data::from_png_bytes(&ico[offset..offset + len])
                .expect("an .ico image");
            assert_eq!(icon.width, sizes[i]);
        }
    }
}
