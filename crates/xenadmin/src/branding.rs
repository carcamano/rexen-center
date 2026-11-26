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
