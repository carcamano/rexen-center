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
