//! A GitHub release (the fields of the REST API's release object this
//! program reads) and the choice of its download for an installation.

use serde::Deserialize;

use crate::Product;
use crate::method::{Arch, InstallMethod};
use crate::version::Version;

/// The file of checksums the release workflow publishes with every
/// release: `sha256sum`'s output for each package.
pub const CHECKSUMS_ASSET: &str = "SHA256SUMS";

/// A release, as `GET /repos/{owner}/{repo}/releases/latest` returns it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    /// The release notes (Markdown).
    #[serde(default)]
    pub body: Option<String>,
    /// The release's page.
    pub html_url: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

/// A file attached to a release.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    /// Bytes.
    #[serde(default)]
    pub size: u64,
}

impl Release {
    /// The version its tag names (`v0.2.0`), if it is one.
    pub fn version(&self) -> Option<Version> {
        Version::parse(&self.tag_name)
    }

    /// A published stable release (the "latest" endpoint gives only these;
    /// checked again all the same).
    pub fn is_stable(&self) -> bool {
        !self.draft && !self.prerelease && self.version().is_some_and(|v| !v.is_prerelease())
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// The checksum file.
    pub fn checksums(&self) -> Option<&Asset> {
        self.asset(CHECKSUMS_ASSET)
    }

    /// The download for `method` on `arch`: the package the release
    /// workflow builds for it, by name. `None` for an unknown method or
    /// a system the release has no package for.
    pub fn package_for(
        &self,
        product: &Product,
        method: &InstallMethod,
        arch: Arch,
    ) -> Option<&Asset> {
        let version = self.version()?;
        self.assets
            .iter()
            .find(|a| is_package_for(product, method, arch, &version, &a.name))
    }
}

/// Whether `name` is the release package of `version` for `method` on
/// `arch` (the names `packaging/*/build.*` give them).
pub fn is_package_for(
    product: &Product,
    method: &InstallMethod,
    arch: Arch,
    version: &Version,
    name: &str,
) -> bool {
    let id = &product.app_id;
    match method {
        InstallMethod::MacBundle { .. } => {
            let Some(arch) = arch_name(arch, "arm64", "x86_64") else {
                return false;
            };
            name == format!("{id}-{version}-macos-{arch}.dmg")
        }
        InstallMethod::WindowsMsi { .. } | InstallMethod::WindowsPortable { .. } => {
            let Some(arch) = arch_name(arch, "arm64", "x64") else {
                return false;
            };
            let ext = if matches!(method, InstallMethod::WindowsMsi { .. }) {
                "msi"
            } else {
                "zip"
            };
            name == format!("{id}-{version}-windows-{arch}.{ext}")
        }
        // rexen-center_0.2.0-1_amd64.deb (any package revision)
        InstallMethod::LinuxDeb => {
            let Some(arch) = arch_name(arch, "arm64", "amd64") else {
                return false;
            };
            revision(name, &format!("{id}_{version}-"), &format!("_{arch}.deb"))
        }
        // rexen-center-0.2.0-1.x86_64.rpm
        InstallMethod::LinuxRpm => {
            let Some(arch) = arch_name(arch, "aarch64", "x86_64") else {
                return false;
            };
            revision(name, &format!("{id}-{version}-"), &format!(".{arch}.rpm"))
        }
        InstallMethod::LinuxTarball { .. } => {
            let Some(arch) = arch_name(arch, "aarch64", "x86_64") else {
                return false;
            };
            name == format!("{id}-{version}-linux-{arch}.tar.gz")
        }
        InstallMethod::Unknown(_) => false,
    }
}

fn arch_name(arch: Arch, arm: &'static str, x86: &'static str) -> Option<&'static str> {
    match arch {
        Arch::Aarch64 => Some(arm),
        Arch::X86_64 => Some(x86),
        Arch::Other => None,
    }
}

/// `name` is `prefix`, a package revision (letters and digits) and
/// `suffix`.
fn revision(name: &str, prefix: &str, suffix: &str) -> bool {
    name.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
        .is_some_and(|rev| !rev.is_empty() && rev.chars().all(|c| c.is_ascii_alphanumeric()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::method::UnknownReason;

    fn product() -> Product {
        Product::rexen_center()
    }

    /// The assets the release workflow publishes for 0.2.0.
    fn release() -> Release {
        let names = [
            "rexen-center-0.2.0-macos-arm64.dmg",
            "rexen-center-0.2.0-macos-x86_64.dmg",
            "rexen-center-0.2.0-windows-x64.zip",
            "rexen-center-0.2.0-windows-x64.msi",
            "rexen-center_0.2.0-1_amd64.deb",
            "rexen-center-0.2.0-1.x86_64.rpm",
            "rexen-center-0.2.0-linux-x86_64.tar.gz",
            "SHA256SUMS",
        ];
        Release {
            tag_name: "v0.2.0".into(),
            name: Some("ReXen Center 0.2.0".into()),
            body: Some("Notes".into()),
            html_url: "https://github.com/carcamano/rexen-center/releases/tag/v0.2.0".into(),
            draft: false,
            prerelease: false,
            assets: names
                .iter()
                .map(|n| Asset {
                    name: (*n).into(),
                    browser_download_url: format!("https://example.invalid/{n}"),
                    size: 1,
                })
                .collect(),
        }
    }

    fn chosen(method: InstallMethod, arch: Arch) -> Option<String> {
        release()
            .package_for(&product(), &method, arch)
            .map(|a| a.name.clone())
    }

    #[test]
    fn each_method_and_architecture_gets_its_package() {
        let mac = || InstallMethod::MacBundle {
            bundle: "/Applications/ReXen Center.app".into(),
        };
        assert_eq!(
            chosen(mac(), Arch::Aarch64).as_deref(),
            Some("rexen-center-0.2.0-macos-arm64.dmg")
        );
        assert_eq!(
            chosen(mac(), Arch::X86_64).as_deref(),
            Some("rexen-center-0.2.0-macos-x86_64.dmg")
        );
        assert_eq!(
            chosen(
                InstallMethod::WindowsMsi { dir: "C:/x".into() },
                Arch::X86_64
            )
            .as_deref(),
            Some("rexen-center-0.2.0-windows-x64.msi")
        );
        assert_eq!(
            chosen(
                InstallMethod::WindowsPortable { dir: "C:/x".into() },
                Arch::X86_64
            )
            .as_deref(),
            Some("rexen-center-0.2.0-windows-x64.zip")
        );
        assert_eq!(
            chosen(InstallMethod::LinuxDeb, Arch::X86_64).as_deref(),
            Some("rexen-center_0.2.0-1_amd64.deb")
        );
        assert_eq!(
            chosen(InstallMethod::LinuxRpm, Arch::X86_64).as_deref(),
            Some("rexen-center-0.2.0-1.x86_64.rpm")
        );
        assert_eq!(
            chosen(
                InstallMethod::LinuxTarball { root: "/x".into() },
                Arch::X86_64
            )
            .as_deref(),
            Some("rexen-center-0.2.0-linux-x86_64.tar.gz")
        );
    }

    #[test]
    fn no_package_means_no_download() {
        // Windows and Linux are built for x86_64 only.
        for method in [
            InstallMethod::WindowsMsi { dir: "C:/x".into() },
            InstallMethod::WindowsPortable { dir: "C:/x".into() },
            InstallMethod::LinuxDeb,
            InstallMethod::LinuxRpm,
            InstallMethod::LinuxTarball { root: "/x".into() },
        ] {
            assert_eq!(chosen(method.clone(), Arch::Aarch64), None, "{method:?}");
            assert_eq!(chosen(method, Arch::Other), None);
        }
        assert_eq!(
            chosen(
                InstallMethod::Unknown(UnknownReason::Unrecognized),
                Arch::X86_64
            ),
            None
        );
        // Another version's file is never taken.
        let mut old = release();
        old.tag_name = "v0.3.0".into();
        assert_eq!(
            old.package_for(&product(), &InstallMethod::LinuxDeb, Arch::X86_64),
            None
        );
    }

    #[test]
    fn package_names_are_matched_exactly() {
        let v = Version::parse("0.2.0").unwrap();
        let deb = InstallMethod::LinuxDeb;
        assert!(is_package_for(
            &product(),
            &deb,
            Arch::X86_64,
            &v,
            "rexen-center_0.2.0-12_amd64.deb"
        ));
        for bad in [
            "rexen-center_0.2.0-_amd64.deb",
            "rexen-center_0.2.0-1_amd64.deb.sig",
            "rexen-center_0.2.0-1-evil_amd64.deb",
            "rexen-center_0.2.0-1_arm64.deb",
            "rexen-center_0.2.01-1_amd64.deb",
            "other_0.2.0-1_amd64.deb",
        ] {
            assert!(
                !is_package_for(&product(), &deb, Arch::X86_64, &v, bad),
                "{bad}"
            );
        }
        let rpm = InstallMethod::LinuxRpm;
        assert!(!is_package_for(
            &product(),
            &rpm,
            Arch::X86_64,
            &v,
            "rexen-center-0.2.0-1.src.rpm"
        ));
    }

    #[test]
    fn stable_releases_and_the_checksum_file() {
        let r = release();
        assert!(r.is_stable());
        assert_eq!(r.version(), Version::parse("0.2.0"));
        assert_eq!(r.checksums().map(|a| a.name.as_str()), Some("SHA256SUMS"));
        let mut pre = release();
        pre.prerelease = true;
        assert!(!pre.is_stable());
        let mut draft = release();
        draft.draft = true;
        assert!(!draft.is_stable());
        let mut rc = release();
        rc.tag_name = "v0.3.0-rc.1".into();
        assert!(!rc.is_stable());
        let mut odd = release();
        odd.tag_name = "nightly".into();
        assert!(!odd.is_stable());
    }

    #[test]
    fn parses_the_api_answer() {
        let json = r###"{
            "url": "https://api.github.com/repos/carcamano/rexen-center/releases/1",
            "html_url": "https://github.com/carcamano/rexen-center/releases/tag/v0.2.0",
            "tag_name": "v0.2.0", "name": "ReXen Center 0.2.0",
            "draft": false, "prerelease": false, "body": "## What's Changed\n* x",
            "author": {"login": "someone"},
            "assets": [{"name": "SHA256SUMS", "size": 712, "content_type": "text/plain",
                        "browser_download_url": "https://github.com/carcamano/rexen-center/releases/download/v0.2.0/SHA256SUMS"}]
        }"###;
        let r: Release = serde_json::from_str(json).unwrap();
        assert_eq!(r.tag_name, "v0.2.0");
        assert_eq!(r.assets[0].size, 712);
        assert!(r.body.unwrap().starts_with("## What's Changed"));
    }
}
