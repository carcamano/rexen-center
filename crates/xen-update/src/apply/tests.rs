use std::io::Write;

use super::*;

fn product() -> Product {
    Product::rexen_center()
}

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

/// A fresh folder for one test.
fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xen-update-apply-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn put(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A `.tar.gz` with these (name, contents, mode) entries; a `None`
/// contents is a folder.
fn tarball(path: &Path, entries: &[(&str, Option<&str>, u32)]) {
    let file = std::fs::File::create(path).unwrap();
    let gz = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    for (name, contents, mode) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(*mode);
        match contents {
            Some(text) => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(text.len() as u64);
                header.set_cksum();
                tar.append_data(&mut header, name, text.as_bytes()).unwrap();
            }
            None => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_cksum();
                tar.append_data(&mut header, name, std::io::empty())
                    .unwrap();
            }
        }
    }
    tar.into_inner().unwrap().finish().unwrap();
}

/// The release tarball of `version` (`packaging/linux/build.sh`'s layout).
fn release_tarball(path: &Path, version: &str) {
    let top = format!("rexen-center-{version}");
    tarball(
        path,
        &[
            (&format!("{top}/"), None, 0o755),
            (&format!("{top}/bin/"), None, 0o755),
            (
                &format!("{top}/bin/rexencenter"),
                Some(&format!("binary {version}")),
                0o755,
            ),
            (
                &format!("{top}/share/applications/rexen-center.desktop"),
                Some("[Desktop Entry]"),
                0o644,
            ),
            (
                &format!("{top}/share/doc/rexen-center/README.md"),
                Some(version),
                0o644,
            ),
        ],
    );
}

/// A `.zip` with these (name, contents) entries.
fn zip_file(path: &Path, entries: &[(&str, &str)]) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, contents) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(contents.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn package_paths_must_stay_inside() {
    let top = "rexen-center-0.2.0";
    assert_eq!(
        safe_relative("rexen-center-0.2.0/bin/rexencenter", top),
        Ok(Some(PathBuf::from("bin").join("rexencenter")))
    );
    assert_eq!(
        safe_relative("rexen-center-0.2.0\\rexencenter.exe", top),
        Ok(Some(PathBuf::from("rexencenter.exe")))
    );
    assert_eq!(
        safe_relative("./rexen-center-0.2.0/./x", top),
        Ok(Some(PathBuf::from("x")))
    );
    assert_eq!(safe_relative("rexen-center-0.2.0/", top), Ok(None));
    for unsafe_name in [
        "rexen-center-0.2.0/../evil",
        "rexen-center-0.2.0/bin/../../evil",
        "/etc/passwd",
        "\\Windows\\evil.dll",
        "rexen-center-0.2.0/C:evil",
        "rexen-center-0.2.0/a\0b",
    ] {
        assert!(
            matches!(
                safe_relative(unsafe_name, top),
                Err(ApplyError::UnsafePath(_))
            ),
            "{unsafe_name:?}"
        );
    }
    // Another version's folder, or none.
    for other in [
        "rexen-center-0.1.0/bin/rexencenter",
        "bin/rexencenter",
        "../x",
    ] {
        assert!(safe_relative(other, top).is_err(), "{other}");
    }
}

#[test]
fn tarballs_unpack_files_and_refuse_links_and_escapes() {
    let dir = temp("tarball");
    let archive = dir.join("p.tar.gz");
    release_tarball(&archive, "0.2.0");
    let out = dir.join("out");
    extract_tarball(&archive, "rexen-center-0.2.0", &out).unwrap();
    let bin = out.join("bin/rexencenter");
    assert_eq!(read(&bin), "binary 0.2.0");
    assert_eq!(read(&out.join("share/doc/rexen-center/README.md")), "0.2.0");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&bin), 0o755);
        assert_eq!(mode(&out.join("share/doc/rexen-center/README.md")), 0o644);
    }

    // A symbolic link.
    let link = dir.join("link.tar.gz");
    {
        let file = std::fs::File::create(&link).unwrap();
        let gz = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        tar.append_link(&mut header, "rexen-center-0.2.0/bin/rexencenter", "/bin/sh")
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap();
    }
    assert!(matches!(
        extract_tarball(&link, "rexen-center-0.2.0", &dir.join("out-link")),
        Err(ApplyError::UnsafePath(_))
    ));
    // Not a tarball at all.
    std::fs::write(dir.join("junk.tar.gz"), "<html>").unwrap();
    assert!(matches!(
        extract_tarball(
            &dir.join("junk.tar.gz"),
            "rexen-center-0.2.0",
            &dir.join("o")
        ),
        Err(ApplyError::WrongPackage(_))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zips_unpack_with_either_separator_and_refuse_escapes() {
    let dir = temp("zip");
    let archive = dir.join("p.zip");
    // Windows PowerShell's Compress-Archive may write backslashes.
    zip_file(
        &archive,
        &[
            ("rexen-center-0.2.0/rexencenter.exe", "exe 0.2.0"),
            ("rexen-center-0.2.0\\LICENSE", "licence"),
            ("rexen-center-0.2.0/README.md", "readme"),
        ],
    );
    let out = dir.join("out");
    extract_zip(&archive, "rexen-center-0.2.0", &out).unwrap();
    assert_eq!(read(&out.join("rexencenter.exe")), "exe 0.2.0");
    assert_eq!(read(&out.join("LICENSE")), "licence");

    let evil = dir.join("evil.zip");
    zip_file(&evil, &[("rexen-center-0.2.0/../../evil.exe", "x")]);
    assert!(matches!(
        extract_zip(&evil, "rexen-center-0.2.0", &dir.join("out-evil")),
        Err(ApplyError::UnsafePath(_))
    ));
    assert!(!dir.join("evil.exe").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// An installed tarball of 0.1.1 in `root`, with the user's portable
/// marker and settings beside the executable.
fn installed_tarball(root: &Path) {
    put(&root.join("bin/rexencenter"), "binary 0.1.1");
    put(&root.join("bin/portable"), "");
    put(&root.join("bin/settings/settings.toml"), "[preferences]");
    put(
        &root.join("share/applications/rexen-center.desktop"),
        "[Desktop Entry] old",
    );
    put(&root.join("share/doc/rexen-center/README.md"), "0.1.1");
}

#[test]
fn tarball_swap_keeps_user_files_and_rolls_back() {
    let dir = temp("swap");
    let root = dir.join("rexen-center-0.1.1");
    installed_tarball(&root);
    let archive = dir.join("rexen-center-0.2.0-linux-x86_64.tar.gz");
    release_tarball(&archive, "0.2.0");
    let method = InstallMethod::LinuxTarball { root: root.clone() };
    let Plan::Replace(prepared) =
        prepare(&product(), &method, &archive, &v("0.2.0"), &dir).unwrap()
    else {
        panic!("not an in-place plan");
    };
    assert_eq!(prepared.work, root.join(WORK_DIR));
    assert_eq!(prepared.relaunch, root.join("bin/rexencenter"));
    assert_eq!(prepared.moves.len(), 3, "{:?}", prepared.moves);
    // Nothing changed yet.
    assert_eq!(read(&root.join("bin/rexencenter")), "binary 0.1.1");

    let swapped = swap(prepared).unwrap();
    assert_eq!(read(&root.join("bin/rexencenter")), "binary 0.2.0");
    assert_eq!(
        read(&root.join("share/doc/rexen-center/README.md")),
        "0.2.0"
    );
    assert_eq!(
        read(&root.join("bin/settings/settings.toml")),
        "[preferences]"
    );
    assert!(root.join("bin/portable").is_file());
    assert_eq!(
        read(&root.join(WORK_DIR).join("old/bin/rexencenter")),
        "binary 0.1.1"
    );

    rollback(&swapped).unwrap();
    assert_eq!(read(&root.join("bin/rexencenter")), "binary 0.1.1");
    assert_eq!(
        read(&root.join("share/doc/rexen-center/README.md")),
        "0.1.1"
    );
    assert_eq!(
        read(&root.join("share/applications/rexen-center.desktop")),
        "[Desktop Entry] old"
    );
    assert!(root.join("bin/portable").is_file());

    // Again, committed this time: the old files go.
    let Plan::Replace(prepared) =
        prepare(&product(), &method, &archive, &v("0.2.0"), &dir).unwrap()
    else {
        panic!()
    };
    commit(swap(prepared).unwrap());
    assert_eq!(read(&root.join("bin/rexencenter")), "binary 0.2.0");
    assert!(!root.join(WORK_DIR).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_swap_is_undone() {
    let dir = temp("swap-fail");
    let root = dir.join("install");
    installed_tarball(&root);
    let work = root.join(WORK_DIR);
    put(&work.join("new/bin/rexencenter"), "binary 0.2.0");
    let prepared = Prepared {
        work: work.clone(),
        moves: vec![
            Move {
                staged: work.join("new/bin/rexencenter"),
                target: root.join("bin/rexencenter"),
                backup: work.join("old/bin/rexencenter"),
            },
            // Its new file is missing: the second move fails.
            Move {
                staged: work.join("new/share/missing"),
                target: root.join("share/doc/rexen-center/README.md"),
                backup: work.join("old/share/doc/rexen-center/README.md"),
            },
        ],
        relaunch: root.join("bin/rexencenter"),
        version: v("0.2.0"),
    };
    assert!(matches!(swap(prepared), Err(ApplyError::Io(_))));
    assert_eq!(read(&root.join("bin/rexencenter")), "binary 0.1.1");
    assert_eq!(
        read(&root.join("share/doc/rexen-center/README.md")),
        "0.1.1"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn portable_zip_is_replaced_beside_the_settings() {
    let dir = temp("portable");
    let install = dir.join("rexen-center-0.1.1");
    put(&install.join("rexencenter.exe"), "exe 0.1.1");
    put(&install.join("LICENSE"), "licence");
    put(&install.join("portable"), "");
    put(&install.join("settings/settings.toml"), "[preferences]");
    let archive = dir.join("rexen-center-0.2.0-windows-x64.zip");
    zip_file(
        &archive,
        &[
            ("rexen-center-0.2.0/rexencenter.exe", "exe 0.2.0"),
            ("rexen-center-0.2.0/LICENSE", "licence 2"),
            ("rexen-center-0.2.0/README.md", "readme"),
        ],
    );
    let method = InstallMethod::WindowsPortable {
        dir: install.clone(),
    };
    let Plan::Replace(prepared) =
        prepare(&product(), &method, &archive, &v("0.2.0"), &dir).unwrap()
    else {
        panic!()
    };
    assert_eq!(prepared.relaunch, install.join("rexencenter.exe"));
    commit(swap(prepared).unwrap());
    assert_eq!(read(&install.join("rexencenter.exe")), "exe 0.2.0");
    assert_eq!(read(&install.join("README.md")), "readme");
    assert_eq!(
        read(&install.join("settings/settings.toml")),
        "[preferences]"
    );
    assert!(install.join("portable").is_file());

    // A zip without the executable is refused before anything moves.
    let wrong = dir.join("wrong.zip");
    zip_file(&wrong, &[("rexen-center-0.2.0/README.md", "readme")]);
    assert!(matches!(
        prepare(&product(), &method, &wrong, &v("0.2.0"), &dir),
        Err(ApplyError::WrongPackage(_))
    ));
    // Another version's zip too.
    assert!(matches!(
        prepare(&product(), &method, &archive, &v("0.3.0"), &dir),
        Err(ApplyError::WrongPackage(_))
    ));
    assert_eq!(read(&install.join("rexencenter.exe")), "exe 0.2.0");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A disk image that is a folder: attaching copies it to the mount point.
struct FolderImage;

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

impl DiskImage for FolderImage {
    fn attach(&self, image: &Path, mountpoint: &Path) -> Result<(), ApplyError> {
        copy_tree(image, mountpoint).map_err(|e| ApplyError::DiskImage(e.to_string()))
    }
    fn detach(&self, mountpoint: &Path) {
        let _ = std::fs::remove_dir_all(mountpoint);
    }
    fn copy_bundle(&self, from: &Path, to: &Path) -> Result<(), ApplyError> {
        copy_tree(from, to).map_err(|e| ApplyError::Io(e.to_string()))
    }
}

fn info_plist(id: &str, version: &str) -> String {
    format!(
        "<plist version=\"1.0\"><dict>\n<key>CFBundleIdentifier</key>\n<string>{id}</string>\n\
         <key>CFBundleShortVersionString</key>\n<string>{version}</string>\n</dict></plist>\n"
    )
}

fn bundle(at: &Path, id: &str, version: &str) {
    put(&at.join("Contents/Info.plist"), &info_plist(id, version));
    put(
        &at.join("Contents/MacOS/rexencenter"),
        &format!("binary {version}"),
    );
}

#[test]
fn macos_bundle_from_the_disk_image() {
    let dir = temp("bundle");
    let apps = dir.join("Applications");
    let installed = apps.join("ReXen Center.app");
    bundle(&installed, "local.rexen-center.console", "0.1.1");
    let image = dir.join("image");
    bundle(
        &image.join("ReXen Center.app"),
        "local.rexen-center.console",
        "0.2.0",
    );
    let scratch = dir.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let prepared = prepare_bundle(
        &product(),
        &installed,
        &image,
        &v("0.2.0"),
        &scratch,
        &FolderImage,
    )
    .unwrap();
    assert!(!scratch.join("image").exists(), "detached");
    assert_eq!(
        prepared.relaunch,
        installed.join("Contents/MacOS/rexencenter")
    );
    let swapped = swap(prepared).unwrap();
    assert_eq!(
        read(&installed.join("Contents/MacOS/rexencenter")),
        "binary 0.2.0"
    );
    rollback(&swapped).unwrap();
    assert_eq!(
        read(&installed.join("Contents/MacOS/rexencenter")),
        "binary 0.1.1"
    );

    // Another app's bundle, or another version, is refused.
    for (id, version) in [
        ("org.example.other", "0.2.0"),
        ("local.rexen-center.console", "0.1.9"),
    ] {
        let image = dir.join(format!("image-{id}-{version}"));
        bundle(&image.join("ReXen Center.app"), id, version);
        let refused = prepare_bundle(
            &product(),
            &installed,
            &image,
            &v("0.2.0"),
            &scratch,
            &FolderImage,
        );
        assert!(
            matches!(refused, Err(ApplyError::WrongPackage(_))),
            "{id} {version}: {refused:?}"
        );
        assert!(!scratch.join("image").exists(), "detached after a refusal");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// This Mac's own tools: a real disk image made with `hdiutil`, mounted,
/// its bundle copied with `ditto` and swapped in.
#[cfg(target_os = "macos")]
#[test]
fn macos_bundle_from_a_real_disk_image() {
    let dir = temp("hdiutil");
    let installed = dir.join("Applications/ReXen Center.app");
    bundle(&installed, "local.rexen-center.console", "0.1.1");
    let stage = dir.join("stage");
    bundle(
        &stage.join("ReXen Center.app"),
        "local.rexen-center.console",
        "0.2.0",
    );
    let dmg = dir.join("rexen-center-0.2.0-macos-arm64.dmg");
    let status = Command::new("/usr/bin/hdiutil")
        .args([
            "create",
            "-quiet",
            "-fs",
            "HFS+",
            "-format",
            "UDZO",
            "-srcfolder",
        ])
        .arg(&stage)
        .arg(&dmg)
        .status()
        .unwrap();
    assert!(status.success());
    let scratch = dir.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let method = InstallMethod::MacBundle {
        bundle: installed.clone(),
    };
    let Plan::Replace(prepared) =
        prepare(&product(), &method, &dmg, &v("0.2.0"), &scratch).unwrap()
    else {
        panic!()
    };
    commit(swap(prepared).unwrap());
    assert_eq!(
        read(&installed.join("Contents/MacOS/rexencenter")),
        "binary 0.2.0"
    );
    assert!(!dir.join("Applications").join(WORK_DIR).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
mod relaunch {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A shell script standing in for the program.
    fn script(path: &Path, body: &str) {
        put(path, &format!("#!/bin/sh\n{body}\n"));
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// An installation whose new version runs `new_body` and whose old one
    /// writes `restarted` when started again.
    fn swapped(dir: &Path, new_body: &str) -> (PathBuf, Swapped) {
        let root = dir.join("install");
        script(
            &root.join("bin/rexencenter"),
            &format!("echo old > '{}'", dir.join("restarted").display()),
        );
        let work = root.join(WORK_DIR);
        script(&work.join("new/bin/rexencenter"), new_body);
        let prepared = Prepared {
            work: work.clone(),
            moves: file_moves(&work, &work.join("new"), &root).unwrap(),
            relaunch: root.join("bin/rexencenter"),
            version: v("0.2.0"),
        };
        (root, swap(prepared).unwrap())
    }

    fn wait_for(path: &Path) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn the_old_copy_goes_once_the_new_version_reports() {
        let dir = temp("relaunch-ok");
        // The new version reports through the file it is given.
        let (root, swapped) = swapped(&dir, "echo up > \"$REXENCENTER_UPDATE_ACK\"; sleep 1");
        assert_eq!(
            finish(swapped, &dir, Duration::from_secs(10)),
            Outcome::Started
        );
        assert!(!root.join(WORK_DIR).exists(), "the old copy is deleted");
        assert!(read(&root.join("bin/rexencenter")).contains("REXENCENTER_UPDATE_ACK"));
        assert!(!dir.join("restarted").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_version_that_exits_is_rolled_back() {
        let dir = temp("relaunch-exit");
        let (root, swapped) = swapped(&dir, "exit 3");
        let outcome = finish(swapped, &dir, Duration::from_secs(10));
        let Outcome::RolledBack(reason) = outcome else {
            panic!("{outcome:?}")
        };
        assert!(reason.contains("exited"), "{reason}");
        assert!(read(&root.join("bin/rexencenter")).contains("restarted"));
        // The old version was started again.
        assert!(wait_for(&dir.join("restarted")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_version_that_hangs_is_stopped_and_rolled_back() {
        let dir = temp("relaunch-hang");
        let (root, swapped) = swapped(&dir, "sleep 30");
        let started = Instant::now();
        let outcome = finish(swapped, &dir, Duration::from_millis(500));
        assert!(started.elapsed() < Duration::from_secs(10));
        let Outcome::RolledBack(reason) = outcome else {
            panic!("{outcome:?}")
        };
        assert!(reason.contains("did not open its window"), "{reason}");
        assert!(read(&root.join("bin/rexencenter")).contains("restarted"));
        assert!(wait_for(&dir.join("restarted")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn download_folders_are_private() {
        let dir = private_dir("xen-update-test").unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert!(
            dir.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("xen-update-test-")
        );
        let other = private_dir("xen-update-test").unwrap();
        assert_ne!(dir, other);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&other);
    }
}

#[test]
fn linux_packages_get_the_exact_command() {
    let package = Path::new("/tmp/rexen-center-update-ab12/rexen-center_0.2.0-1_amd64.deb");
    let all = |_: &str| true;
    let none = |_: &str| false;
    assert_eq!(
        install_command(&InstallMethod::LinuxDeb, package, &all).as_deref(),
        Some("sudo apt install '/tmp/rexen-center-update-ab12/rexen-center_0.2.0-1_amd64.deb'")
    );
    assert_eq!(
        install_command(&InstallMethod::LinuxDeb, package, &none).as_deref(),
        Some("sudo dpkg -i '/tmp/rexen-center-update-ab12/rexen-center_0.2.0-1_amd64.deb'")
    );
    let rpm = Path::new("/tmp/x/rexen-center-0.2.0-1.x86_64.rpm");
    assert_eq!(
        install_command(&InstallMethod::LinuxRpm, rpm, &|t| t == "dnf").as_deref(),
        Some("sudo dnf install '/tmp/x/rexen-center-0.2.0-1.x86_64.rpm'")
    );
    assert_eq!(
        install_command(&InstallMethod::LinuxRpm, rpm, &|t| t == "zypper").as_deref(),
        Some("sudo zypper install '/tmp/x/rexen-center-0.2.0-1.x86_64.rpm'")
    );
    assert_eq!(
        install_command(&InstallMethod::LinuxRpm, rpm, &none).as_deref(),
        Some("sudo rpm -U '/tmp/x/rexen-center-0.2.0-1.x86_64.rpm'")
    );
    assert_eq!(
        install_command(
            &InstallMethod::LinuxTarball { root: "/x".into() },
            rpm,
            &all
        ),
        None
    );
    assert_eq!(shell_quote("it's"), r"'it'\''s'");
    // Installers are handed over, not unpacked.
    let dir = temp("handover");
    let msi = dir.join("rexen-center-0.2.0-windows-x64.msi");
    std::fs::write(&msi, "msi").unwrap();
    let plan = prepare(
        &product(),
        &InstallMethod::WindowsMsi { dir: "C:/x".into() },
        &msi,
        &v("0.2.0"),
        &dir,
    )
    .unwrap();
    assert!(matches!(plan, Plan::RunInstaller { package } if package == msi));
    assert_eq!(
        prepare(
            &product(),
            &InstallMethod::Unknown(crate::UnknownReason::AppImage),
            &msi,
            &v("0.2.0"),
            &dir
        )
        .unwrap_err(),
        ApplyError::Unsupported
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unwritable_installation_is_reported_before_anything_moves() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp("readonly");
        let root = dir.join("install");
        installed_tarball(&root);
        let archive = dir.join("p.tar.gz");
        release_tarball(&archive, "0.2.0");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = prepare(
            &product(),
            &InstallMethod::LinuxTarball { root: root.clone() },
            &archive,
            &v("0.2.0"),
            &dir,
        );
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Root may write anywhere; the check only means something for users.
        if !matches!(result, Ok(Plan::Replace(_))) {
            assert!(
                matches!(result, Err(ApplyError::NotWritable { .. })),
                "{result:?}"
            );
        }
        assert_eq!(read(&root.join("bin/rexencenter")), "binary 0.1.1");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
