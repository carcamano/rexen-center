//! Updates of the desktop app from its GitHub releases.
//!
//! * [`version`]: release versions and the "newer, stable, never a
//!   downgrade" rule.
//! * [`method`]: how the running copy was installed (macOS app bundle,
//!   Windows installer or portable zip, Linux .deb, .rpm or tarball): a
//!   platform-neutral core over a [`method::Probe`] of the system's facts.
//! * [`release`]: the GitHub release object and the choice of the package
//!   for an installation method and architecture.
//! * [`client`]: the release server (latest release, checksum file,
//!   download with progress and cancel).
//! * [`checksum`]: SHA-256 verification against the release's `SHA256SUMS`.
//! * [`state`]: what is remembered between runs (last check, skipped
//!   version) and the daily throttle.
//! * [`apply`]: unpacking a verified package, replacing the installation in
//!   place with a rollback until the new version has started, and handing
//!   installer packages to the system.
//! * [`updater`]: the flow the window drives, from the check to the
//!   restart, on a tokio runtime.

pub mod apply;
pub mod checksum;
pub mod client;
pub mod method;
pub mod release;
pub mod state;
pub mod updater;
pub mod version;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use method::{Arch, InstallMethod, Os, UnknownReason};
pub use release::{Asset, Release};
pub use version::Version;

/// The product being updated: its names, as the release packages use them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Product {
    /// Package and file names (`rexen-center`).
    pub app_id: String,
    /// The executable (`rexencenter`; `.exe` on Windows).
    pub binary: String,
    /// The macOS bundle's name without `.app` (`ReXen Center`).
    pub bundle_name: String,
    /// The macOS bundle identifier (`local.rexen-center.console`, as
    /// `packaging/macos/Info.plist` makes it).
    pub bundle_id: String,
    /// The GitHub repository the releases are published in
    /// (`carcamano/rexen-center`).
    pub repository: String,
    /// The file beside the executable that turns on portable mode.
    pub portable_marker: String,
}

impl Product {
    /// The product with these names; the bundle identifier is the one the
    /// macOS packaging derives from `app_id`.
    pub fn new(app_id: &str, binary: &str, bundle_name: &str, repository: &str) -> Self {
        Self {
            app_id: app_id.to_owned(),
            binary: binary.to_owned(),
            bundle_name: bundle_name.to_owned(),
            bundle_id: format!("local.{app_id}.console"),
            repository: repository.to_owned(),
            portable_marker: "portable".to_owned(),
        }
    }

    /// The executable's file name on this system.
    pub fn exe_name(&self, os: Os) -> String {
        match os {
            Os::Windows => format!("{}.exe", self.binary),
            _ => self.binary.clone(),
        }
    }

    /// ReXen Center's names (for the tests).
    #[cfg(any(test, feature = "testing"))]
    pub fn rexen_center() -> Self {
        Self::new(
            "rexen-center",
            "rexencenter",
            "ReXen Center",
            "carcamano/rexen-center",
        )
    }
}
