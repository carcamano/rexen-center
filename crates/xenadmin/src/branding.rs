//! Product identity. The final product name is not chosen yet: change it here
//! and nowhere else.

/// Product name shown in the window title, About box and login originator.
pub const PRODUCT_NAME: &str = "xenadmin-rs";

/// Product version (from Cargo).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// SPDX identifier of the product license.
pub const LICENSE_ID: &str = "BSD-2-Clause";

/// Full license text shown in the About window.
pub const LICENSE_TEXT: &str = include_str!("../../../LICENSE");

/// Login originator: identifies this client in the server's logs.
pub fn originator() -> String {
    format!("{PRODUCT_NAME}/{VERSION}")
}
