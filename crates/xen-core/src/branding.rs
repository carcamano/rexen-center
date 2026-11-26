//! Product identity, shared by the desktop app, the MCP server and the
//! packaging scripts (which read these lines with `sed`, so keep each
//! constant on one line). Change the name here and nowhere else.
//!
//! ReXen Center is an independent console for XCP-ng; it is not an official
//! XCP-ng, Vates or Citrix product.

/// Product name shown in the window title, About box, home page and menus.
pub const PRODUCT_NAME: &str = "ReXen Center";

/// The tagline shown where one fits (About box, home page, packages).
pub const SUBTITLE: &str = "ReXen Center — XCP-ng Center, born again in Rust";

/// Machine-friendly identifier: the configuration folder, the keyring
/// service, log file names, the login originator and the desktop entry.
pub const APP_ID: &str = "rexen-center";

/// The executable's name (`rexencenter`, `rexencenter --mcp`).
pub const BINARY_NAME: &str = "rexencenter";

/// The identifier the console used before it was named ReXen Center:
/// its configuration folder and keyring service, carried over on the first
/// start (see [`crate::migration`]).
pub const LEGACY_APP_ID: &str = "xenadmin-rs";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitle_names_the_product() {
        assert!(SUBTITLE.starts_with(PRODUCT_NAME), "{SUBTITLE}");
        assert_eq!(SUBTITLE, "ReXen Center — XCP-ng Center, born again in Rust");
        assert_ne!(APP_ID, LEGACY_APP_ID);
        for id in [APP_ID, BINARY_NAME] {
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{id}"
            );
            assert!(!id.contains("xenadmin"), "{id}");
        }
    }
}
