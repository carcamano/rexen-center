//! How the running copy was installed, so that an update replaces it the
//! same way: the macOS app bundle, the Windows installer (MSI) or the
//! portable zip, the Linux .deb, .rpm or tarball.
//!
//! [`detect`] is the platform-neutral core: it reads the facts through a
//! [`Probe`] (the executable's path, the files beside it, the package
//! databases), so the tests run every platform's rules everywhere. The
//! per-OS facts come from [`SystemProbe`]. Anything it does not recognise
//! is [`InstallMethod::Unknown`], for which the update only opens the
//! release page.

use std::path::{Path, PathBuf};

use crate::Product;

/// The operating systems the releases are built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Windows,
    Linux,
    Other,
}

impl Os {
    /// The running system.
    pub fn current() -> Self {
        match std::env::consts::OS {
            "macos" => Self::MacOs,
            "windows" => Self::Windows,
            "linux" => Self::Linux,
            _ => Self::Other,
        }
    }
}

/// The processor architecture of the running program (an x86_64 build
/// under Rosetta 2 stays x86_64: its update is the same build).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
    Other,
}

impl Arch {
    pub fn current() -> Self {
        match std::env::consts::ARCH {
            "x86_64" => Self::X86_64,
            "aarch64" => Self::Aarch64,
            _ => Self::Other,
        }
    }
}

/// The package database that owns the executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageDb {
    Dpkg,
    Rpm,
}

/// How the running copy was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallMethod {
    /// `<bundle>.app` on macOS, replaced in place from the disk image.
    MacBundle { bundle: PathBuf },
    /// The Windows installer's copy in `dir` (Program Files); updated by
    /// running the new installer.
    WindowsMsi { dir: PathBuf },
    /// The Windows zip unpacked in `dir`; its files are replaced in place.
    WindowsPortable { dir: PathBuf },
    /// The Debian/Ubuntu package; the new one is handed to the system's
    /// installer.
    LinuxDeb,
    /// The Fedora/RHEL package; likewise.
    LinuxRpm,
    /// The Linux tarball unpacked in `root` (`root/bin/<binary>`); its files
    /// are replaced in place.
    LinuxTarball { root: PathBuf },
    /// Not recognised: the update opens the release page.
    Unknown(UnknownReason),
}

/// Why the installation was not recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnknownReason {
    /// The executable's path is not known.
    NoExecutable,
    /// A build from source (`target/debug`, `target/release`).
    DevelopmentBuild,
    /// macOS runs the app from a randomised read-only copy (App
    /// Translocation): move it to Applications first.
    Translocated,
    /// An AppImage (no release is published as one).
    AppImage,
    /// No release is built for this system.
    UnsupportedSystem,
    /// None of the release layouts.
    Unrecognized,
}

impl InstallMethod {
    /// The method can update without opening the release page.
    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Unknown(_))
    }

    /// The update replaces the files in place and relaunches.
    pub fn replaces_in_place(&self) -> bool {
        matches!(
            self,
            Self::MacBundle { .. } | Self::WindowsPortable { .. } | Self::LinuxTarball { .. }
        )
    }
}

/// The facts [`detect`] decides from.
pub trait Probe {
    fn os(&self) -> Os;
    /// The running executable (symbolic links resolved where the system
    /// allows).
    fn exe(&self) -> Option<PathBuf>;
    fn is_file(&self, path: &Path) -> bool;
    fn read_to_string(&self, path: &Path) -> Option<String>;
    /// A non-empty environment variable.
    fn env(&self, name: &str) -> Option<String>;
    /// The package database that lists `path` as a file of `package`.
    fn package_owner(&self, path: &Path, package: &str) -> Option<PackageDb>;
}

/// Decides how the running copy of `product` was installed.
pub fn detect(product: &Product, probe: &dyn Probe) -> InstallMethod {
    let Some(exe) = probe.exe() else {
        return InstallMethod::Unknown(UnknownReason::NoExecutable);
    };
    if is_development_build(&exe) {
        return InstallMethod::Unknown(UnknownReason::DevelopmentBuild);
    }
    match probe.os() {
        Os::MacOs => detect_macos(product, probe, &exe),
        Os::Windows => detect_windows(product, probe, &exe),
        Os::Linux => detect_linux(product, probe, &exe),
        Os::Other => InstallMethod::Unknown(UnknownReason::UnsupportedSystem),
    }
}

/// Cargo's output folders.
fn is_development_build(exe: &Path) -> bool {
    let names: Vec<&str> = exe
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    names
        .windows(2)
        .any(|w| w[0] == "target" && (w[1] == "debug" || w[1] == "release"))
        || names.windows(3).any(|w| {
            // target/<triple>/release
            w[0] == "target" && w[1].contains('-') && (w[2] == "debug" || w[2] == "release")
        })
}

/// `<name>.app/Contents/MacOS/<binary>`, whose Info.plist names the
/// product's bundle identifier.
fn detect_macos(product: &Product, probe: &dyn Probe, exe: &Path) -> InstallMethod {
    let unrecognized = InstallMethod::Unknown(UnknownReason::Unrecognized);
    let Some(macos) = exe.parent() else {
        return unrecognized;
    };
    let Some(contents) = macos.parent() else {
        return unrecognized;
    };
    let Some(bundle) = contents.parent() else {
        return unrecognized;
    };
    let named = |p: &Path, name: &str| p.file_name().is_some_and(|n| n == name);
    let is_app = bundle
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("app"));
    if !named(macos, "MacOS") || !named(contents, "Contents") || !is_app {
        return unrecognized;
    }
    if exe.to_string_lossy().contains("/AppTranslocation/") {
        return InstallMethod::Unknown(UnknownReason::Translocated);
    }
    let plist = probe.read_to_string(&contents.join("Info.plist"));
    match plist.as_deref().and_then(bundle_identifier) {
        Some(id) if id == product.bundle_id => InstallMethod::MacBundle {
            bundle: bundle.to_path_buf(),
        },
        _ => unrecognized,
    }
}

/// The `CFBundleIdentifier` of an XML property list.
pub fn bundle_identifier(plist: &str) -> Option<&str> {
    plist_string(plist, "CFBundleIdentifier")
}

/// The string value of `key` in an XML property list's top dictionary
/// (enough for the bundles this program builds).
pub fn plist_string<'a>(plist: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("<key>{key}</key>");
    let after = &plist[plist.find(&marker)? + marker.len()..];
    let after = after.trim_start();
    let value = after.strip_prefix("<string>")?;
    let end = value.find("</string>")?;
    Some(value[..end].trim())
}

/// The installer puts `LICENSE.txt` beside the executable; the zip
/// `LICENSE` and `README.md` (and the user may add a `portable` marker).
fn detect_windows(product: &Product, probe: &dyn Probe, exe: &Path) -> InstallMethod {
    let unrecognized = InstallMethod::Unknown(UnknownReason::Unrecognized);
    let expected = product.exe_name(Os::Windows);
    let named = exe
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case(&expected));
    let Some(dir) = exe.parent().filter(|_| named) else {
        return unrecognized;
    };
    if probe.is_file(&dir.join("LICENSE.txt")) && !probe.is_file(&dir.join("README.md")) {
        return InstallMethod::WindowsMsi {
            dir: dir.to_path_buf(),
        };
    }
    let zip_files = probe.is_file(&dir.join("LICENSE")) || probe.is_file(&dir.join("README.md"));
    if zip_files || probe.is_file(&dir.join(&product.portable_marker)) {
        return InstallMethod::WindowsPortable {
            dir: dir.to_path_buf(),
        };
    }
    unrecognized
}

/// `/usr/bin/<binary>` from a package, or `<root>/bin/<binary>` of the
/// tarball (with its desktop entry under `<root>/share`).
fn detect_linux(product: &Product, probe: &dyn Probe, exe: &Path) -> InstallMethod {
    if probe.env("APPIMAGE").is_some() {
        return InstallMethod::Unknown(UnknownReason::AppImage);
    }
    let packaged = Path::new("/usr/bin").join(&product.binary);
    if exe == packaged {
        return match probe.package_owner(exe, &product.app_id) {
            Some(PackageDb::Dpkg) => InstallMethod::LinuxDeb,
            Some(PackageDb::Rpm) => InstallMethod::LinuxRpm,
            None => InstallMethod::Unknown(UnknownReason::Unrecognized),
        };
    }
    let bin = exe
        .parent()
        .filter(|b| b.file_name().is_some_and(|n| n == "bin"));
    if let Some(root) = bin.and_then(Path::parent) {
        let entry = root
            .join("share/applications")
            .join(format!("{}.desktop", product.app_id));
        // Not a system prefix: those belong to a package manager.
        let system = [Path::new("/usr"), Path::new("/usr/local"), Path::new("/")];
        if probe.is_file(&entry) && !system.contains(&root) {
            return InstallMethod::LinuxTarball {
                root: root.to_path_buf(),
            };
        }
    }
    InstallMethod::Unknown(UnknownReason::Unrecognized)
}

/// The running system's facts.
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn os(&self) -> Os {
        Os::current()
    }

    fn exe(&self) -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        // Windows' canonical form is a `\\?\` path, which the rules (and
        // the installer's) do not use.
        if cfg!(windows) {
            Some(exe)
        } else {
            Some(std::fs::canonicalize(&exe).unwrap_or(exe))
        }
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn read_to_string(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }

    fn package_owner(&self, path: &Path, package: &str) -> Option<PackageDb> {
        // dpkg lists each package's files in its database folder.
        let list = Path::new("/var/lib/dpkg/info").join(format!("{package}.list"));
        if let Ok(files) = std::fs::read_to_string(list)
            && files.lines().any(|l| Path::new(l) == path)
        {
            return Some(PackageDb::Dpkg);
        }
        // rpm keeps a database only its own tool reads.
        let owner = std::process::Command::new("rpm")
            .args(["-qf", "--queryformat", "%{NAME}"])
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        (owner.status.success() && String::from_utf8_lossy(&owner.stdout).trim() == package)
            .then_some(PackageDb::Rpm)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;

    fn product() -> Product {
        Product::rexen_center()
    }

    #[derive(Default)]
    struct FakeProbe {
        os: Option<Os>,
        exe: Option<PathBuf>,
        files: HashSet<PathBuf>,
        contents: HashMap<PathBuf, String>,
        env: HashMap<String, String>,
        owner: Option<PackageDb>,
    }

    impl FakeProbe {
        fn new(os: Os, exe: &str) -> Self {
            Self {
                os: Some(os),
                exe: Some(exe.into()),
                ..Self::default()
            }
        }

        fn file(mut self, path: &str) -> Self {
            self.files.insert(path.into());
            self
        }

        fn text(mut self, path: &str, text: &str) -> Self {
            self.files.insert(path.into());
            self.contents.insert(path.into(), text.into());
            self
        }
    }

    impl Probe for FakeProbe {
        fn os(&self) -> Os {
            self.os.unwrap_or(Os::Other)
        }
        fn exe(&self) -> Option<PathBuf> {
            self.exe.clone()
        }
        fn is_file(&self, path: &Path) -> bool {
            self.files.contains(path)
        }
        fn read_to_string(&self, path: &Path) -> Option<String> {
            self.contents.get(path).cloned()
        }
        fn env(&self, name: &str) -> Option<String> {
            self.env.get(name).cloned()
        }
        fn package_owner(&self, path: &Path, package: &str) -> Option<PackageDb> {
            assert_eq!(package, "rexen-center");
            assert_eq!(path, Path::new("/usr/bin/rexencenter"));
            self.owner
        }
    }

    fn plist(id: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n    \
             <key>CFBundleExecutable</key>\n    <string>rexencenter</string>\n    \
             <key>CFBundleIdentifier</key>\n    <string>{id}</string>\n</dict>\n</plist>\n"
        )
    }

    const APP: &str = "/Applications/ReXen Center.app";

    #[test]
    fn macos_bundle() {
        let exe = format!("{APP}/Contents/MacOS/rexencenter");
        let probe = FakeProbe::new(Os::MacOs, &exe).text(
            &format!("{APP}/Contents/Info.plist"),
            &plist("local.rexen-center.console"),
        );
        assert_eq!(
            detect(&product(), &probe),
            InstallMethod::MacBundle { bundle: APP.into() }
        );
        // Another app's bundle, or no Info.plist.
        let other = FakeProbe::new(Os::MacOs, &exe).text(
            &format!("{APP}/Contents/Info.plist"),
            &plist("org.example.other"),
        );
        assert_eq!(
            detect(&product(), &other),
            InstallMethod::Unknown(UnknownReason::Unrecognized)
        );
        let bare = FakeProbe::new(Os::MacOs, &exe);
        assert!(!detect(&product(), &bare).is_known());
        // A bare binary.
        let loose = FakeProbe::new(Os::MacOs, "/usr/local/bin/rexencenter");
        assert!(!detect(&product(), &loose).is_known());
    }

    #[test]
    fn macos_translocated_and_development_builds() {
        let translocated = "/private/var/folders/xy/T/AppTranslocation/1234/d/ReXen Center.app";
        let probe = FakeProbe::new(
            Os::MacOs,
            &format!("{translocated}/Contents/MacOS/rexencenter"),
        )
        .text(
            &format!("{translocated}/Contents/Info.plist"),
            &plist("local.rexen-center.console"),
        );
        assert_eq!(
            detect(&product(), &probe),
            InstallMethod::Unknown(UnknownReason::Translocated)
        );
        for exe in [
            "/src/xenadmin-rs/target/release/rexencenter",
            "/src/xenadmin-rs/target/debug/rexencenter",
            "/src/xenadmin-rs/target/aarch64-apple-darwin/release/rexencenter",
        ] {
            assert_eq!(
                detect(&product(), &FakeProbe::new(Os::MacOs, exe)),
                InstallMethod::Unknown(UnknownReason::DevelopmentBuild),
                "{exe}"
            );
        }
    }

    #[test]
    fn windows_installer_and_portable_zip() {
        // Forward slashes: the rules run on every system's `Path`.
        let dir = "C:/Program Files/ReXen Center";
        let msi = FakeProbe::new(Os::Windows, &format!("{dir}/rexencenter.exe"))
            .file(&format!("{dir}/LICENSE.txt"));
        assert_eq!(
            detect(&product(), &msi),
            InstallMethod::WindowsMsi { dir: dir.into() }
        );
        let zip = "D:/Tools/rexen-center-0.1.1";
        let portable = FakeProbe::new(Os::Windows, &format!("{zip}/rexencenter.exe"))
            .file(&format!("{zip}/LICENSE"))
            .file(&format!("{zip}/README.md"));
        assert_eq!(
            detect(&product(), &portable),
            InstallMethod::WindowsPortable { dir: zip.into() }
        );
        // The exe alone with the user's portable marker.
        let marker = FakeProbe::new(Os::Windows, "E:/rx/RexenCenter.EXE").file("E:/rx/portable");
        assert_eq!(
            detect(&product(), &marker),
            InstallMethod::WindowsPortable {
                dir: "E:/rx".into()
            }
        );
        // A lone exe, or a renamed one.
        let lone = FakeProbe::new(Os::Windows, "E:/rx/rexencenter.exe");
        assert!(!detect(&product(), &lone).is_known());
        let renamed = FakeProbe::new(Os::Windows, &format!("{zip}/console.exe"))
            .file(&format!("{zip}/LICENSE"));
        assert!(!detect(&product(), &renamed).is_known());
    }

    #[test]
    fn linux_packages_tarball_and_appimage() {
        let mut deb = FakeProbe::new(Os::Linux, "/usr/bin/rexencenter");
        deb.owner = Some(PackageDb::Dpkg);
        assert_eq!(detect(&product(), &deb), InstallMethod::LinuxDeb);
        let mut rpm = FakeProbe::new(Os::Linux, "/usr/bin/rexencenter");
        rpm.owner = Some(PackageDb::Rpm);
        assert_eq!(detect(&product(), &rpm), InstallMethod::LinuxRpm);
        // In /usr/bin but no package owns it (copied there by hand).
        let unowned = FakeProbe::new(Os::Linux, "/usr/bin/rexencenter");
        assert!(!detect(&product(), &unowned).is_known());

        let root = "/home/ana/opt/rexen-center-0.1.1";
        let tarball = FakeProbe::new(Os::Linux, &format!("{root}/bin/rexencenter"))
            .file(&format!("{root}/share/applications/rexen-center.desktop"));
        assert_eq!(
            detect(&product(), &tarball),
            InstallMethod::LinuxTarball { root: root.into() }
        );
        // The tarball's layout under a system prefix is a package's.
        let local = FakeProbe::new(Os::Linux, "/usr/local/bin/rexencenter")
            .file("/usr/local/share/applications/rexen-center.desktop");
        assert!(!detect(&product(), &local).is_known());
        let no_entry = FakeProbe::new(Os::Linux, &format!("{root}/bin/rexencenter"));
        assert!(!detect(&product(), &no_entry).is_known());

        let mut appimage = FakeProbe::new(Os::Linux, "/tmp/.mount_rexenX/usr/bin/rexencenter");
        appimage
            .env
            .insert("APPIMAGE".into(), "/home/ana/rexen.AppImage".into());
        assert_eq!(
            detect(&product(), &appimage),
            InstallMethod::Unknown(UnknownReason::AppImage)
        );
    }

    #[test]
    fn other_systems_and_missing_executable() {
        assert_eq!(
            detect(&product(), &FakeProbe::new(Os::Other, "/bin/rexencenter")),
            InstallMethod::Unknown(UnknownReason::UnsupportedSystem)
        );
        let none = FakeProbe {
            os: Some(Os::Linux),
            ..FakeProbe::default()
        };
        assert_eq!(
            detect(&product(), &none),
            InstallMethod::Unknown(UnknownReason::NoExecutable)
        );
    }

    #[test]
    fn plist_values() {
        let text = plist("local.rexen-center.console");
        assert_eq!(bundle_identifier(&text), Some("local.rexen-center.console"));
        assert_eq!(
            plist_string(&text, "CFBundleExecutable"),
            Some("rexencenter")
        );
        assert_eq!(plist_string(&text, "CFBundleVersion"), None);
        assert_eq!(
            bundle_identifier("<key>CFBundleIdentifier</key><true/>"),
            None
        );
    }

    #[test]
    fn method_kinds() {
        assert!(InstallMethod::LinuxTarball { root: "/x".into() }.replaces_in_place());
        assert!(!InstallMethod::LinuxDeb.replaces_in_place());
        assert!(!InstallMethod::WindowsMsi { dir: "/x".into() }.replaces_in_place());
        assert!(InstallMethod::LinuxRpm.is_known());
        assert!(!InstallMethod::Unknown(UnknownReason::AppImage).is_known());
    }
}
