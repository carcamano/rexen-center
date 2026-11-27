//! Installing a verified package, by the safest route for each method:
//!
//! * **macOS app bundle, Windows portable zip, Linux tarball**: replaced in
//!   place. [`prepare`] unpacks the package into a work folder beside the
//!   installation (same file system, so every move is a rename) and checks
//!   it; after the window has closed (its sessions logged out as File >
//!   Quit does), [`swap`] moves each old file aside and the new one in,
//!   and [`finish`] starts the new version and waits for it to report that
//!   its window is up ([`acknowledge_start`]). Only then is the old copy
//!   deleted; if the new one exits or does not report in time, it is
//!   stopped, the old files are moved back and the old version started
//!   again.
//! * **Windows installer (MSI)**: run by Windows Installer
//!   (`msiexec /i`) after the window has closed.
//! * **Linux .deb and .rpm**: never installed with root rights behind the
//!   user's back. The package is handed to the desktop's software installer
//!   on request, and the exact command to install it is shown.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use crate::method::{InstallMethod, Os, plist_string};
use crate::version::Version;
use crate::{Product, method};

/// The work folder's name, beside the installation.
pub const WORK_DIR: &str = ".rexen-center-update";

/// The variable that tells a version started by an update where to report
/// that it is up.
pub const ACK_ENV: &str = "REXENCENTER_UPDATE_ACK";

/// How long the new version has to report that it is up.
pub const START_TIMEOUT: Duration = Duration::from_secs(60);

/// What went wrong while installing.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ApplyError {
    #[error("cannot write to {path}: {error}")]
    NotWritable { path: String, error: String },
    #[error("{0}")]
    Io(String),
    #[error("the package holds an unsafe path: {0}")]
    UnsafePath(String),
    #[error("the package is not the expected one: {0}")]
    WrongPackage(String),
    #[error("cannot open the disk image: {0}")]
    DiskImage(String),
    #[error("this installation cannot be updated in place")]
    Unsupported,
}

fn io_error(context: &str, path: &Path, e: io::Error) -> ApplyError {
    ApplyError::Io(format!("{context} {}: {e}", path.display()))
}

// ---- private folders --------------------------------------------------------

/// A new folder only this user can read, in the system's temporary folder,
/// for the downloads (`<prefix>-<random>`).
pub fn private_dir(prefix: &str) -> io::Result<PathBuf> {
    let base = std::env::temp_dir();
    for _ in 0..8 {
        let dir = base.join(format!("{prefix}-{}", random_suffix()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("cannot create a private folder"))
}

fn random_suffix() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 8];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        // Only uniqueness matters here; `create` refuses existing folders.
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes = (nanos as u64 ^ u64::from(std::process::id())).to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Removes this program's download folders older than a day from the
/// temporary folder (an installer handed over keeps its folder).
pub fn remove_stale_downloads(prefix: &str) {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let wanted = format!("{prefix}-");
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(&wanted) && older_than(&entry.path(), DAY) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn older_than(path: &Path, age: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|elapsed| elapsed > age)
}

// ---- plans ----------------------------------------------------------------

/// How a verified package gets installed.
#[derive(Debug)]
pub enum Plan {
    /// Replace the installation in place, then start the new version.
    Replace(Prepared),
    /// Run the Windows installer once the window has closed.
    RunInstaller { package: PathBuf },
    /// Hand a Linux package to the system's installer.
    SystemPackage {
        package: PathBuf,
        /// The command that installs it, to show (and copy).
        command: String,
    },
}

/// A package unpacked and checked beside the installation, ready to swap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// The work folder (`.rexen-center-update` beside the installation).
    pub work: PathBuf,
    /// Each new file or bundle and the place it goes.
    pub moves: Vec<Move>,
    /// The executable to start afterwards.
    pub relaunch: PathBuf,
    pub version: Version,
}

/// One file (or the macOS bundle) of a swap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub staged: PathBuf,
    pub target: PathBuf,
    pub backup: PathBuf,
}

/// The work folder of an installation replaced in place.
pub fn work_dir(method: &InstallMethod) -> Option<PathBuf> {
    match method {
        InstallMethod::MacBundle { bundle } => bundle.parent().map(|p| p.join(WORK_DIR)),
        InstallMethod::WindowsPortable { dir } => Some(dir.join(WORK_DIR)),
        InstallMethod::LinuxTarball { root } => Some(root.join(WORK_DIR)),
        _ => None,
    }
}

/// Unpacks and checks the verified `package` of `version` for `method`,
/// using `scratch` (a private folder) for anything temporary.
pub fn prepare(
    product: &Product,
    method: &InstallMethod,
    package: &Path,
    version: &Version,
    scratch: &Path,
) -> Result<Plan, ApplyError> {
    match method {
        InstallMethod::MacBundle { bundle } => {
            prepare_bundle(product, bundle, package, version, scratch, &Hdiutil).map(Plan::Replace)
        }
        InstallMethod::WindowsPortable { dir } => {
            prepare_zip(product, dir, package, version).map(Plan::Replace)
        }
        InstallMethod::LinuxTarball { root } => {
            prepare_tarball(product, root, package, version).map(Plan::Replace)
        }
        InstallMethod::WindowsMsi { .. } => Ok(Plan::RunInstaller {
            package: package.to_path_buf(),
        }),
        InstallMethod::LinuxDeb | InstallMethod::LinuxRpm => Ok(Plan::SystemPackage {
            package: package.to_path_buf(),
            command: install_command(method, package, &|tool| {
                ["/usr/bin", "/bin", "/usr/sbin"]
                    .iter()
                    .any(|dir| Path::new(dir).join(tool).is_file())
            })
            .ok_or(ApplyError::Unsupported)?,
        }),
        InstallMethod::Unknown(_) => Err(ApplyError::Unsupported),
    }
}

/// A fresh work folder: creating it is also the check that the
/// installation's folder is writable.
fn fresh_work_dir(work: &Path) -> Result<PathBuf, ApplyError> {
    if work.exists() {
        std::fs::remove_dir_all(work).map_err(|e| ApplyError::NotWritable {
            path: work.display().to_string(),
            error: e.to_string(),
        })?;
    }
    let new = work.join("new");
    std::fs::create_dir_all(&new).map_err(|e| ApplyError::NotWritable {
        path: work
            .parent()
            .map_or_else(|| work.display().to_string(), |p| p.display().to_string()),
        error: e.to_string(),
    })?;
    Ok(new)
}

/// The moves for every file under `new`, into `root`.
fn file_moves(work: &Path, new: &Path, root: &Path) -> Result<Vec<Move>, ApplyError> {
    let mut moves = Vec::new();
    let mut stack = vec![new.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| io_error("cannot read", &dir, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| io_error("cannot read", &dir, e))?;
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(|e| io_error("cannot read", &path, e))?;
            if kind.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(new)
                    .map_err(|_| ApplyError::UnsafePath(path.display().to_string()))?
                    .to_path_buf();
                moves.push(Move {
                    target: root.join(&rel),
                    backup: work.join("old").join(&rel),
                    staged: path,
                });
            }
        }
    }
    moves.sort_by(|a, b| a.target.cmp(&b.target));
    Ok(moves)
}

// ---- archives -----------------------------------------------------------------

/// A path inside a package, made relative to its top folder `top`: only
/// plain names, no `..`, no root, no drive (backslashes taken as
/// separators, as Windows' zip tools write them).
pub fn safe_relative(name: &str, top: &str) -> Result<Option<PathBuf>, ApplyError> {
    let unsafe_path = || ApplyError::UnsafePath(name.to_owned());
    let normalized = name.replace('\\', "/");
    if normalized.starts_with('/') || normalized.contains('\0') {
        return Err(unsafe_path());
    }
    let mut parts = normalized.split('/').filter(|p| !p.is_empty() && *p != ".");
    if parts.next() != Some(top) {
        return Err(ApplyError::WrongPackage(format!(
            "{name} is not in the folder {top}"
        )));
    }
    let mut rel = PathBuf::new();
    for part in parts {
        if part == ".." || part.contains(':') {
            return Err(unsafe_path());
        }
        rel.push(part);
    }
    // Every part must stay a plain name on this system too.
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(unsafe_path());
    }
    Ok((!rel.as_os_str().is_empty()).then_some(rel))
}

/// The folder the packages put everything in: `rexen-center-<version>`.
fn top_folder(product: &Product, version: &Version) -> String {
    format!("{}-{version}", product.app_id)
}

fn write_file(dest: &Path, reader: &mut dyn io::Read, executable: bool) -> Result<(), ApplyError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_error("cannot create", parent, e))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, if executable { 0o755 } else { 0o644 });
    #[cfg(not(unix))]
    let _ = executable;
    let mut file = options
        .open(dest)
        .map_err(|e| io_error("cannot create", dest, e))?;
    io::copy(reader, &mut file).map_err(|e| io_error("cannot write", dest, e))?;
    file.sync_all()
        .map_err(|e| io_error("cannot write", dest, e))?;
    Ok(())
}

/// Unpacks a `.tar.gz` package's top folder into `dest`. Only folders and
/// regular files are accepted.
pub fn extract_tarball(archive: &Path, top: &str, dest: &Path) -> Result<(), ApplyError> {
    let file = std::fs::File::open(archive).map_err(|e| io_error("cannot open", archive, e))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let bad = |e: io::Error| ApplyError::WrongPackage(format!("not a readable tarball: {e}"));
    for entry in tar.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let kind = entry.header().entry_type();
        if matches!(
            kind,
            tar::EntryType::XGlobalHeader | tar::EntryType::XHeader
        ) {
            continue;
        }
        let Some(rel) = safe_relative(&name, top)? else {
            continue;
        };
        let target = dest.join(&rel);
        match kind {
            tar::EntryType::Directory => {
                std::fs::create_dir_all(&target)
                    .map_err(|e| io_error("cannot create", &target, e))?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let executable = entry.header().mode().is_ok_and(|m| m & 0o111 != 0);
                write_file(&target, &mut entry, executable)?;
            }
            other => {
                return Err(ApplyError::UnsafePath(format!("{name} ({other:?})")));
            }
        }
    }
    Ok(())
}

/// Unpacks a `.zip` package's top folder into `dest`. Symbolic links are
/// refused.
pub fn extract_zip(archive: &Path, top: &str, dest: &Path) -> Result<(), ApplyError> {
    let file = std::fs::File::open(archive).map_err(|e| io_error("cannot open", archive, e))?;
    let bad =
        |e: zip::result::ZipError| ApplyError::WrongPackage(format!("not a readable zip: {e}"));
    let mut zip = zip::ZipArchive::new(file).map_err(bad)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(bad)?;
        let name = entry.name().to_owned();
        if entry.is_symlink() {
            return Err(ApplyError::UnsafePath(name));
        }
        let Some(rel) = safe_relative(&name, top)? else {
            continue;
        };
        let target = dest.join(&rel);
        if entry.is_dir() || name.ends_with('/') || name.ends_with('\\') {
            std::fs::create_dir_all(&target).map_err(|e| io_error("cannot create", &target, e))?;
        } else {
            let executable = entry.unix_mode().is_some_and(|m| m & 0o111 != 0)
                || rel
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
            write_file(&target, &mut entry, executable)?;
        }
    }
    Ok(())
}

fn prepare_zip(
    product: &Product,
    dir: &Path,
    package: &Path,
    version: &Version,
) -> Result<Prepared, ApplyError> {
    let work = dir.join(WORK_DIR);
    let new = fresh_work_dir(&work)?;
    extract_zip(package, &top_folder(product, version), &new)?;
    let exe = product.exe_name(Os::Windows);
    if !new.join(&exe).is_file() {
        return Err(ApplyError::WrongPackage(format!("no {exe} in the zip")));
    }
    Ok(Prepared {
        moves: file_moves(&work, &new, dir)?,
        relaunch: dir.join(exe),
        work,
        version: version.clone(),
    })
}

fn prepare_tarball(
    product: &Product,
    root: &Path,
    package: &Path,
    version: &Version,
) -> Result<Prepared, ApplyError> {
    let work = root.join(WORK_DIR);
    let new = fresh_work_dir(&work)?;
    extract_tarball(package, &top_folder(product, version), &new)?;
    let exe = Path::new("bin").join(&product.binary);
    if !new.join(&exe).is_file() {
        return Err(ApplyError::WrongPackage(format!(
            "no {} in the tarball",
            exe.display()
        )));
    }
    Ok(Prepared {
        moves: file_moves(&work, &new, root)?,
        relaunch: root.join(exe),
        work,
        version: version.clone(),
    })
}

// ---- macOS disk image ---------------------------------------------------------

/// Mounting a disk image and copying a bundle out of it.
pub trait DiskImage {
    fn attach(&self, image: &Path, mountpoint: &Path) -> Result<(), ApplyError>;
    fn detach(&self, mountpoint: &Path);
    fn copy_bundle(&self, from: &Path, to: &Path) -> Result<(), ApplyError>;
}

/// macOS' `hdiutil` and `ditto`.
pub struct Hdiutil;

impl DiskImage for Hdiutil {
    fn attach(&self, image: &Path, mountpoint: &Path) -> Result<(), ApplyError> {
        std::fs::create_dir_all(mountpoint)
            .map_err(|e| io_error("cannot create", mountpoint, e))?;
        // Read only, not shown in the Finder, no window opened.
        run(Command::new("/usr/bin/hdiutil")
            .args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-quiet"])
            .arg("-mountpoint")
            .arg(mountpoint)
            .arg(image))
        .map_err(ApplyError::DiskImage)
    }

    fn detach(&self, mountpoint: &Path) {
        let detach = |force: bool| {
            let mut cmd = Command::new("/usr/bin/hdiutil");
            cmd.args(["detach", "-quiet"]).arg(mountpoint);
            if force {
                cmd.arg("-force");
            }
            run(&mut cmd)
        };
        if let Err(e) = detach(false).or_else(|_| detach(true)) {
            tracing::warn!(mountpoint = %mountpoint.display(), error = %e, "cannot detach the disk image");
        }
    }

    fn copy_bundle(&self, from: &Path, to: &Path) -> Result<(), ApplyError> {
        // `ditto` keeps the bundle's symbolic links, permissions and
        // extended attributes.
        run(Command::new("/usr/bin/ditto").arg(from).arg(to)).map_err(ApplyError::Io)
    }
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let output = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} failed ({}): {}",
            cmd.get_program().to_string_lossy(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Checks that `bundle` is `product`'s bundle of `version`.
fn check_bundle(product: &Product, bundle: &Path, version: &Version) -> Result<(), ApplyError> {
    let wrong = |what: String| ApplyError::WrongPackage(what);
    let plist = std::fs::read_to_string(bundle.join("Contents/Info.plist"))
        .map_err(|_| wrong(format!("no Info.plist in {}", bundle.display())))?;
    let id = method::bundle_identifier(&plist);
    if id != Some(product.bundle_id.as_str()) {
        return Err(wrong(format!("bundle identifier {id:?}")));
    }
    let shown = plist_string(&plist, "CFBundleShortVersionString").and_then(Version::parse);
    if shown.as_ref() != Some(version) {
        return Err(wrong(format!(
            "bundle version {:?}, expected {version}",
            shown.map(|v| v.to_string())
        )));
    }
    let exe = bundle.join("Contents/MacOS").join(&product.binary);
    if !exe.is_file() {
        return Err(wrong(format!("no {}", exe.display())));
    }
    Ok(())
}

/// Copies the new bundle out of the disk image `image` beside `bundle`.
pub fn prepare_bundle(
    product: &Product,
    bundle: &Path,
    image: &Path,
    version: &Version,
    scratch: &Path,
    disk: &dyn DiskImage,
) -> Result<Prepared, ApplyError> {
    let parent = bundle.parent().ok_or(ApplyError::Unsupported)?;
    let name = bundle.file_name().ok_or(ApplyError::Unsupported)?;
    let work = parent.join(WORK_DIR);
    let new = fresh_work_dir(&work)?;
    let mountpoint = scratch.join("image");
    disk.attach(image, &mountpoint)?;
    let copied = (|| {
        let source = mountpoint.join(format!("{}.app", product.bundle_name));
        check_bundle(product, &source, version)?;
        disk.copy_bundle(&source, &new.join(name))
    })();
    disk.detach(&mountpoint);
    copied?;
    let staged = new.join(name);
    check_bundle(product, &staged, version)?;
    Ok(Prepared {
        moves: vec![Move {
            staged,
            target: bundle.to_path_buf(),
            backup: work.join("old").join(name),
        }],
        relaunch: bundle.join("Contents/MacOS").join(&product.binary),
        work,
        version: version.clone(),
    })
}

// ---- the swap -------------------------------------------------------------------

/// A swap done: the old files are in the work folder's `old`.
#[derive(Debug)]
pub struct Swapped {
    pub prepared: Prepared,
    /// For each move done, whether there was an old file to put back.
    done: Vec<(Move, bool)>,
}

/// Moves the old files aside and the new ones in. On any failure, what was
/// moved is moved back and the error returned.
pub fn swap(prepared: Prepared) -> Result<Swapped, ApplyError> {
    let mut done = Vec::new();
    for m in &prepared.moves {
        match swap_one(m) {
            Ok(had_old) => done.push((m.clone(), had_old)),
            Err(e) => {
                let partial = Swapped {
                    prepared: prepared.clone(),
                    done,
                };
                if let Err(undo) = rollback(&partial) {
                    tracing::error!(error = %undo, "cannot undo a partial update");
                }
                return Err(e);
            }
        }
    }
    Ok(Swapped { prepared, done })
}

fn swap_one(m: &Move) -> Result<bool, ApplyError> {
    let had_old = std::fs::symlink_metadata(&m.target).is_ok();
    if had_old {
        if let Some(parent) = m.backup.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_error("cannot create", parent, e))?;
        }
        std::fs::rename(&m.target, &m.backup).map_err(|e| io_error("cannot move", &m.target, e))?;
    }
    if let Some(parent) = m.target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_error("cannot create", parent, e))?;
    }
    if let Err(e) = std::fs::rename(&m.staged, &m.target) {
        if had_old {
            let _ = std::fs::rename(&m.backup, &m.target);
        }
        return Err(io_error("cannot move", &m.staged, e));
    }
    Ok(had_old)
}

/// Puts the old files back (the new ones go back to the work folder).
pub fn rollback(swapped: &Swapped) -> Result<(), ApplyError> {
    let mut first_error = None;
    for (m, had_old) in swapped.done.iter().rev() {
        let step = || -> Result<(), ApplyError> {
            std::fs::rename(&m.target, &m.staged)
                .map_err(|e| io_error("cannot move back", &m.target, e))?;
            if *had_old {
                std::fs::rename(&m.backup, &m.target)
                    .map_err(|e| io_error("cannot restore", &m.backup, e))?;
            }
            Ok(())
        };
        if let Err(e) = step() {
            first_error.get_or_insert(e);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Deletes the work folder with the old files (best effort: on Windows the
/// old executable cannot be deleted while it runs; the new version deletes
/// it, see [`cleanup_after_start`]).
pub fn commit(swapped: Swapped) {
    if let Err(e) = std::fs::remove_dir_all(&swapped.prepared.work) {
        tracing::debug!(error = %e, "work folder left for the new version to delete");
    }
}

// ---- starting the new version ----------------------------------------------------

/// How [`finish`] ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The new version is up; the old files are gone.
    Started,
    /// The new version did not start; the old one was put back and started
    /// again.
    RolledBack(String),
}

/// Starts `swapped`'s new version and waits up to `timeout` for it to
/// report, through a file in `ack_dir`, that its window is up. Then the old
/// files are deleted; otherwise the new version is stopped, the old files
/// are put back and the old version is started again.
pub fn finish(swapped: Swapped, ack_dir: &Path, timeout: Duration) -> Outcome {
    let ack = ack_dir.join("started");
    let _ = std::fs::remove_file(&ack);
    let reason = match start(&swapped.prepared.relaunch, Some(&ack)) {
        Ok(mut child) => match wait_for_ack(&mut child, &ack, timeout) {
            Ok(()) => {
                commit(swapped);
                return Outcome::Started;
            }
            Err(reason) => {
                let _ = child.kill();
                let _ = child.wait();
                reason
            }
        },
        Err(e) => format!("cannot start {}: {e}", swapped.prepared.relaunch.display()),
    };
    tracing::error!(%reason, "the new version did not start; rolling back");
    let reason = match rollback(&swapped) {
        Ok(()) => reason,
        Err(e) => format!("{reason}; the rollback failed too: {e}"),
    };
    if let Err(e) = start(&swapped.prepared.relaunch, None) {
        tracing::error!(error = %e, "cannot start the previous version again");
    }
    Outcome::RolledBack(reason)
}

fn wait_for_ack(child: &mut Child, ack: &Path, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if ack.exists() {
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // It may have reported just before exiting.
                return if ack.exists() {
                    Ok(())
                } else {
                    Err(format!("the new version exited ({status})"))
                };
            }
            Ok(None) => {}
            Err(e) => return Err(format!("cannot watch the new version: {e}")),
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the new version did not open its window within {} s",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Starts `exe` detached from this process' standard streams.
fn start(exe: &Path, ack: Option<&Path>) -> io::Result<Child> {
    let mut cmd = Command::new(exe);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    match ack {
        Some(ack) => cmd.env(ACK_ENV, ack),
        None => cmd.env_remove(ACK_ENV),
    };
    cmd.spawn()
}

/// Called by a version started by an update once its window is up: tells
/// the previous version, which waits for it, that the update worked.
/// Returns whether this process was started by an update.
pub fn acknowledge_start() -> bool {
    let Some(ack) = std::env::var_os(ACK_ENV).filter(|v| !v.is_empty()) else {
        return false;
    };
    if let Err(e) = std::fs::write(&ack, b"started\n") {
        tracing::warn!(error = %e, "cannot report the start to the updater");
    }
    true
}

/// After an update: deletes the work folder of `method`'s installation once
/// the previous version has let go of it (retrying for a minute, on a
/// thread of its own); otherwise only a work folder left a day ago.
pub fn cleanup_after_start(method: &InstallMethod, started_by_update: bool) {
    let Some(work) = work_dir(method) else {
        return;
    };
    if !work.exists() {
        return;
    }
    if !started_by_update {
        if older_than(&work, DAY) {
            let _ = std::fs::remove_dir_all(&work);
        }
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("update-cleanup".into())
        .spawn(move || {
            for _ in 0..60 {
                if !work.exists() || std::fs::remove_dir_all(&work).is_ok() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            tracing::warn!(dir = %work.display(), "cannot delete the previous version");
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "cannot start the update cleanup");
    }
}

// ---- installers ---------------------------------------------------------------

/// Runs the Windows installer for `package` (`msiexec /i`); it asks for
/// the administrator's consent itself.
pub fn run_windows_installer(package: &Path) -> io::Result<()> {
    Command::new("msiexec")
        .arg("/i")
        .arg(package)
        .stdin(Stdio::null())
        .spawn()
        .map(drop)
}

/// Opens a web page (`https://` only, or the local machine's for the
/// tests) or a file with the system's default program.
pub fn open_external(target: &std::ffi::OsStr) -> io::Result<()> {
    let text = target.to_string_lossy();
    let is_url = text.contains("://");
    if is_url && !crate::client::allowed_url(&text) {
        return Err(io::Error::other(format!("not opening {text}")));
    }
    let mut cmd = if cfg!(target_os = "macos") {
        Command::new("/usr/bin/open")
    } else if cfg!(windows) {
        // Explorer hands a URL to the default browser and a file to its
        // program, without a shell to interpret it.
        Command::new("explorer.exe")
    } else {
        Command::new("xdg-open")
    };
    cmd.arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

/// The command that installs a Linux package, for the package tools this
/// system has (`has("apt")`…).
pub fn install_command(
    method: &InstallMethod,
    package: &Path,
    has: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let path = shell_quote(&package.to_string_lossy());
    let tool = match method {
        InstallMethod::LinuxDeb if has("apt") => "apt install",
        InstallMethod::LinuxDeb => "dpkg -i",
        InstallMethod::LinuxRpm if has("dnf") => "dnf install",
        InstallMethod::LinuxRpm if has("zypper") => "zypper install",
        InstallMethod::LinuxRpm => "rpm -U",
        _ => return None,
    };
    Some(format!("sudo {tool} {path}"))
}

/// `text` in single quotes for a POSIX shell.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests;
