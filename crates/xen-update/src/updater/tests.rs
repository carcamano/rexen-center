//! The flow against the fake release server on the local machine.

use std::time::{Duration, Instant};

use super::*;
use crate::testing::{Checksums, FakeRelease, FakeReleaseServer, Signing, TestKey};

fn product() -> Product {
    Product::rexen_center()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xen-update-flow-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn updater(
    runtime: &tokio::runtime::Runtime,
    server: &FakeReleaseServer,
    method: InstallMethod,
    state_file: Option<PathBuf>,
) -> Updater {
    Updater::with_keys(
        Config {
            product: product(),
            running: Version::parse("0.1.1").unwrap(),
            method,
            arch: Arch::X86_64,
            api: server.url().to_owned(),
            state_file,
            start_delay: Duration::ZERO,
        },
        vec![TestKey::trusted().public()],
        runtime.handle().clone(),
        || {},
    )
}

fn wait_for(updater: &Updater, what: &str, done: impl Fn(&Phase) -> bool) -> Phase {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let phase = updater.view().phase;
        if done(&phase) {
            return phase;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; at {phase:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn settled(phase: &Phase) -> bool {
    !matches!(
        phase,
        Phase::Checking { .. } | Phase::Downloading { .. } | Phase::Preparing(_)
    )
}

fn tarball(version: &str) -> Vec<u8> {
    crate::testing::release_tarball(&product(), version)
}

const TARBALL: &str = "rexen-center-0.2.0-linux-x86_64.tar.gz";

fn release_020() -> FakeRelease {
    FakeRelease::new("0.2.0")
        .asset(TARBALL, tarball("0.2.0"))
        .asset("rexen-center_0.2.0-1_amd64.deb", b"deb".to_vec())
}

/// A tarball installation of 0.1.1.
fn installed(dir: &Path) -> (PathBuf, InstallMethod) {
    let method = crate::testing::install_tarball(&product(), dir, "0.1.1");
    let InstallMethod::LinuxTarball { root } = &method else {
        unreachable!()
    };
    (root.clone(), method)
}

#[test]
fn the_automatic_check_offers_a_newer_release_and_sends_nothing_about_the_user() {
    let rt = runtime();
    let server = FakeReleaseServer::start();
    server.publish(&product(), &release_020());
    let dir = temp("auto");
    let state_file = dir.join("update.toml");
    let u = updater(
        &rt,
        &server,
        InstallMethod::LinuxDeb,
        Some(state_file.clone()),
    );
    assert_eq!(u.tick(true), None);
    let Phase::Available(offer) = wait_for(&u, "the offer", settled) else {
        panic!("{:?}", u.view())
    };
    assert_eq!(offer.version.to_string(), "0.2.0");
    assert_eq!(
        offer.package.as_ref().map(|p| p.name.as_str()),
        Some("rexen-center_0.2.0-1_amd64.deb")
    );
    assert!(offer.installable());
    assert!(
        matches!(&offer.trust, Trust::Signed { key_id, .. } if *key_id == TestKey::trusted().public().id())
    );
    assert!(offer.notes().contains("Release 0.2.0"));
    assert_eq!(
        u.view().button.map(|v| v.to_string()).as_deref(),
        Some("0.2.0")
    );
    // The attempt is remembered: no second check today, even after a
    // restart.
    assert!(UpdateState::load(&state_file).last_check.is_some());
    // The release, then its checksum file and signature (to tell whether
    // it can be installed); not the package.
    let requests = server.requests();
    let paths: Vec<&str> = requests.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/repos/carcamano/rexen-center/releases/latest",
            "/download/v0.2.0/SHA256SUMS",
            "/download/v0.2.0/SHA256SUMS.minisig",
        ]
    );
    for request in &requests {
        for (name, value) in &request.headers {
            assert!(
                [
                    "host",
                    "user-agent",
                    "accept",
                    "x-github-api-version",
                    "accept-encoding"
                ]
                .contains(&name.as_str()),
                "unexpected header {name}: {value}"
            );
        }
        let ua = request
            .headers
            .iter()
            .find(|(n, _)| n == "user-agent")
            .unwrap();
        assert_eq!(ua.1, "rexen-center-updater");
    }

    let again = updater(&rt, &server, InstallMethod::LinuxDeb, Some(state_file));
    again.tick(true);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(again.view().phase, Phase::Idle);
    assert_eq!(server.requests().len(), 3, "throttled to once a day");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_automatic_check_waits_and_can_be_turned_off() {
    let rt = runtime();
    let server = FakeReleaseServer::start();
    server.publish(&product(), &release_020());
    let off = updater(&rt, &server, InstallMethod::LinuxDeb, None);
    assert_eq!(off.tick(false), None);
    let mut delayed = updater(&rt, &server, InstallMethod::LinuxDeb, None);
    Arc::get_mut(&mut delayed.inner).unwrap().start_delay = Duration::from_secs(3600);
    assert!(
        delayed
            .tick(true)
            .is_some_and(|d| d > Duration::from_secs(3000))
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(server.requests().is_empty());
    assert_eq!(off.view().phase, Phase::Idle);
}

#[test]
fn skip_later_and_the_manual_check() {
    let rt = runtime();
    let server = FakeReleaseServer::start();
    server.publish(&product(), &release_020());
    let dir = temp("skip");
    let state_file = dir.join("update.toml");
    let u = updater(
        &rt,
        &server,
        InstallMethod::LinuxDeb,
        Some(state_file.clone()),
    );
    u.check(false);
    assert!(matches!(
        wait_for(&u, "offer", settled),
        Phase::Available(_)
    ));
    // Later: the button goes until the next check.
    u.later();
    assert_eq!(u.view().button, None);
    assert!(matches!(u.view().phase, Phase::Available(_)));
    u.undismiss();
    assert!(u.view().button.is_some());
    // Skip: remembered, and the automatic check stays quiet about it.
    u.skip();
    assert_eq!(u.view().phase, Phase::Idle);
    assert_eq!(
        UpdateState::load(&state_file).skipped_version.as_deref(),
        Some("0.2.0")
    );
    u.check(false);
    assert_eq!(wait_for(&u, "quiet check", settled), Phase::Idle);
    // The manual check shows it all the same.
    u.check(true);
    assert!(matches!(
        wait_for(&u, "offer", settled),
        Phase::Available(_)
    ));
    // A newer release than the skipped one is offered again.
    server.publish(&product(), &FakeRelease::new("0.2.1"));
    u.check(false);
    let Phase::Available(offer) = wait_for(&u, "offer", settled) else {
        panic!()
    };
    assert_eq!(offer.version.to_string(), "0.2.1");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nothing_newer_and_failures() {
    let rt = runtime();
    let server = FakeReleaseServer::start();
    let u = updater(&rt, &server, InstallMethod::LinuxDeb, None);
    // No release yet (404): up to date.
    u.check(true);
    assert_eq!(wait_for(&u, "result", settled), Phase::UpToDate);
    // The same version, an older one, a pre-release: nothing to offer.
    for release in [
        FakeRelease::new("0.1.1"),
        FakeRelease::new("0.1.0"),
        FakeRelease {
            prerelease: true,
            ..FakeRelease::new("0.3.0")
        },
    ] {
        server.publish(&product(), &release);
        u.check(true);
        assert_eq!(wait_for(&u, "result", settled), Phase::UpToDate);
    }
    // A server error: silent in the background, shown when asked.
    server.set(&FakeReleaseServer::latest_path(&product()), 500, "oops");
    u.check(false);
    assert_eq!(wait_for(&u, "result", settled), Phase::Idle);
    u.check(true);
    let Phase::Failed { message, offer } = wait_for(&u, "result", settled) else {
        panic!()
    };
    assert!(message.contains("500"), "{message}");
    assert!(offer.is_none());
    // Garbage instead of JSON.
    server.set(&FakeReleaseServer::latest_path(&product()), 200, "<html>");
    u.check(true);
    assert!(matches!(
        wait_for(&u, "result", settled),
        Phase::Failed { .. }
    ));
    // Nobody listening.
    drop(server);
    let gone = Updater::new(
        Config {
            api: "http://127.0.0.1:9".into(),
            ..config_for(InstallMethod::LinuxDeb)
        },
        rt.handle().clone(),
        || {},
    );
    gone.check(true);
    assert!(matches!(
        wait_for(&gone, "result", settled),
        Phase::Failed { .. }
    ));
}

fn config_for(method: InstallMethod) -> Config {
    Config {
        product: product(),
        running: Version::parse("0.1.1").unwrap(),
        method,
        arch: Arch::X86_64,
        api: String::new(),
        state_file: None,
        start_delay: Duration::ZERO,
    }
}

#[test]
fn install_downloads_verifies_and_replaces_in_place() {
    let rt = runtime();
    let server = FakeReleaseServer::start();
    server.publish(&product(), &release_020());
    let dir = temp("install");
    let (root, method) = installed(&dir);
    let u = updater(&rt, &server, method, None);
    u.check(true);
    assert!(matches!(
        wait_for(&u, "offer", settled),
        Phase::Available(_)
    ));
    u.install();
    let phase = wait_for(&u, "ready", settled);
    assert!(
        matches!(
            &phase,
            Phase::Ready {
                kind: ReadyKind::Restart,
                ..
            }
        ),
        "{phase:?}"
    );
    // Still the old version until the window has closed.
    assert_eq!(
        std::fs::read_to_string(root.join("bin/rexencenter")).unwrap(),
        "binary 0.1.1"
    );
    let Some(PostExit::Replace { prepared, scratch }) = u.take_post_exit() else {
        panic!("no restart plan")
    };
    assert!(scratch.join(TARBALL).is_file(), "the verified download");
    apply::commit(apply::swap(prepared).unwrap());
    assert_eq!(
        std::fs::read_to_string(root.join("bin/rexencenter")).unwrap(),
        "binary 0.2.0"
    );
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_checksum_mismatch_is_refused_and_nothing_is_kept() {
    let rt = runtime();
    let server = FakeReleaseServer::start();
    server.publish(
        &product(),
        &FakeRelease {
            checksums: Checksums::WrongFor(TARBALL.into()),
            ..release_020()
        },
    );
    let dir = temp("mismatch");
    let (root, method) = installed(&dir);
    let u = updater(&rt, &server, method, None);
    u.check(true);
    wait_for(&u, "offer", settled);
    u.install();
    let Phase::Failed { message, offer } = wait_for(&u, "failure", settled) else {
        panic!("{:?}", u.view())
    };
    assert!(message.contains("does not match"), "{message}");
    // The offer stands: the button stays, and the release page is one
    // click away.
    assert!(u.view().button.is_some());
    assert!(offer.is_some_and(|o| o.page().ends_with("/releases/tag/v0.2.0")));
    assert!(u.take_post_exit().is_none());
    assert!(!root.join(apply::WORK_DIR).exists());
    assert_eq!(
        std::fs::read_to_string(root.join("bin/rexencenter")).unwrap(),
        "binary 0.1.1"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn without_a_checksum_file_only_the_release_page_is_offered() {
    let rt = runtime();
    let server = FakeReleaseServer::start();
    server.publish(
        &product(),
        &FakeRelease {
            checksums: Checksums::Missing,
            ..release_020()
        },
    );
    let dir = temp("nosums");
    let (_, method) = installed(&dir);
    let u = updater(&rt, &server, method, None);
    u.check(true);
    let Phase::Available(offer) = wait_for(&u, "offer", settled) else {
        panic!()
    };
    assert_eq!(offer.untrusted(), Some(&Untrusted::NoChecksumFile));
    assert!(!offer.installable());
    u.install();
    assert!(matches!(u.view().phase, Phase::Available(_)), "no download");
    let downloads = server
        .requests()
        .iter()
        .filter(|r| r.path.contains("/download/"))
        .count();
    assert_eq!(downloads, 0);
    // A signed listing that lacks the package's line is refused before the
    // download, too.
    server.publish(
        &product(),
        &FakeRelease {
            checksums: Checksums::Without(TARBALL.into()),
            ..release_020()
        },
    );
    u.check(true);
    wait_for(&u, "offer", settled);
    u.install();
    let Phase::Failed { message, .. } = wait_for(&u, "failure", settled) else {
        panic!()
    };
    assert!(message.contains("no entry for"), "{message}");
    assert!(!server.requests().iter().any(|r| r.path.ends_with(TARBALL)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_download_shows_progress_and_can_be_cancelled() {
    let rt = runtime();
    let server = FakeReleaseServer::start();
    server.publish(&product(), &release_020());
    let path = FakeReleaseServer::asset_path("0.2.0", TARBALL);
    server.throttle(&path, 16, Duration::from_millis(50));
    let dir = temp("cancel");
    let (root, method) = installed(&dir);
    let u = updater(&rt, &server, method, None);
    u.check(true);
    wait_for(&u, "offer", settled);
    u.install();
    let phase = wait_for(
        &u,
        "progress",
        |p| matches!(p, Phase::Downloading { done, .. } if *done > 0),
    );
    let Phase::Downloading { done, total, .. } = phase else {
        unreachable!()
    };
    assert!(total.is_some_and(|t| t > done), "{done} of {total:?}");
    u.cancel();
    assert!(matches!(u.view().phase, Phase::Available(_)));
    std::thread::sleep(Duration::from_millis(300));
    assert!(matches!(u.view().phase, Phase::Available(_)), "stays put");
    assert!(u.take_post_exit().is_none());
    assert!(!root.join(apply::WORK_DIR).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn installers_and_unknown_installations() {
    let rt = runtime();
    let server = FakeReleaseServer::start();
    server.publish(
        &product(),
        &release_020().asset("rexen-center-0.2.0-windows-x64.msi", b"msi".to_vec()),
    );
    // Windows installer: run once the window has closed.
    let msi = updater(
        &rt,
        &server,
        InstallMethod::WindowsMsi {
            dir: "C:/Program Files/ReXen Center".into(),
        },
        None,
    );
    msi.check(true);
    wait_for(&msi, "offer", settled);
    msi.install();
    assert!(matches!(
        wait_for(&msi, "ready", settled),
        Phase::Ready {
            kind: ReadyKind::Installer,
            ..
        }
    ));
    let Some(PostExit::RunInstaller { package }) = msi.take_post_exit() else {
        panic!()
    };
    assert_eq!(std::fs::read(&package).unwrap(), b"msi");
    let _ = std::fs::remove_dir_all(package.parent().unwrap());

    // .deb: the command and the package, never run behind the user's back.
    let deb = updater(&rt, &server, InstallMethod::LinuxDeb, None);
    deb.check(true);
    wait_for(&deb, "offer", settled);
    deb.install();
    let Phase::Ready {
        kind: ReadyKind::SystemPackage { package, command },
        ..
    } = wait_for(&deb, "ready", settled)
    else {
        panic!("{:?}", deb.view())
    };
    assert!(command.starts_with("sudo "), "{command}");
    assert!(command.contains(&*package.to_string_lossy()), "{command}");
    assert!(deb.take_post_exit().is_none(), "nothing runs on exit");
    deb.cancel();
    assert!(!package.exists(), "dropped with the plan");

    // Unknown installation: the offer has no package (release page only).
    let unknown = updater(
        &rt,
        &server,
        InstallMethod::Unknown(crate::UnknownReason::DevelopmentBuild),
        None,
    );
    unknown.check(true);
    let Phase::Available(offer) = wait_for(&unknown, "offer", settled) else {
        panic!()
    };
    assert!(offer.package.is_none());
    assert!(!offer.installable());
}

/// An offer of `release` to a tarball installation, by `u`.
fn offer_of(u: &Updater) -> Offer {
    u.check(true);
    let Phase::Available(offer) = wait_for(u, "offer", settled) else {
        panic!("{:?}", u.view())
    };
    offer
}

#[test]
fn only_a_release_signed_by_a_trusted_key_can_be_installed() {
    let rt = runtime();
    let dir = temp("signing");
    let (root, method) = installed(&dir);
    let cases = [
        (Signing::Missing, Untrusted::NoSignature),
        (
            Signing::OtherKey(99),
            Untrusted::BadSignature(SignatureError::UnknownKey(TestKey::new(99).public().id())),
        ),
        (
            Signing::TamperedChecksums,
            Untrusted::BadSignature(SignatureError::Invalid),
        ),
        (
            Signing::TamperedSignature,
            Untrusted::BadSignature(SignatureError::Invalid),
        ),
    ];
    for (signing, why) in cases {
        let server = FakeReleaseServer::start();
        server.publish(
            &product(),
            &FakeRelease {
                signing: signing.clone(),
                ..release_020()
            },
        );
        let u = updater(&rt, &server, method.clone(), None);
        let offer = offer_of(&u);
        assert_eq!(offer.untrusted(), Some(&why), "{signing:?}");
        assert!(!offer.installable(), "{signing:?}");
        assert_eq!(offer.signed_checksums(), None);
        // The button still tells of the new version (its page can be
        // opened), but Install does nothing.
        assert!(u.view().button.is_some());
        u.install();
        assert!(matches!(u.view().phase, Phase::Available(_)), "{signing:?}");
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !server.requests().iter().any(|r| r.path.ends_with(TARBALL)),
            "{signing:?}: the package was downloaded"
        );
        assert!(!root.join(apply::WORK_DIR).exists());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_real_build_does_not_trust_the_tests_key() {
    // `Updater::new` trusts only the built-in release keys, whatever server
    // it is pointed at (REXENCENTER_UPDATE_API): the fake server's release,
    // signed with the tests' key, is refused.
    let rt = runtime();
    let server = FakeReleaseServer::start();
    server.publish(&product(), &release_020());
    let u = Updater::new(
        Config {
            api: server.url().to_owned(),
            ..config_for(InstallMethod::LinuxDeb)
        },
        rt.handle().clone(),
        || {},
    );
    let offer = offer_of(&u);
    assert_eq!(
        offer.untrusted(),
        Some(&Untrusted::BadSignature(SignatureError::UnknownKey(
            TestKey::trusted().public().id()
        )))
    );
    assert!(!offer.installable());
    // The reason, as the dialog shows it.
    assert_eq!(
        offer.untrusted().unwrap().to_string(),
        format!(
            "the signature was made with key {}, which this version does not trust",
            TestKey::trusted().public().id()
        )
    );
    // No package for this installation: nothing is fetched beyond the
    // release itself, and nothing is said about trust.
    let server = FakeReleaseServer::start();
    server.publish(&product(), &FakeRelease::new("0.2.0"));
    let u = updater(&rt, &server, InstallMethod::LinuxDeb, None);
    let offer = offer_of(&u);
    assert_eq!(offer.trust, Trust::Unchecked);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_rollback_is_reported_once() {
    let dir = temp("rollback");
    let state_file = dir.join("update.toml");
    UpdateState {
        rolled_back: Some(RolledBack {
            version: "0.2.0".into(),
            reason: "the new version exited".into(),
        }),
        ..UpdateState::default()
    }
    .save(&state_file)
    .unwrap();
    let rt = runtime();
    let server = FakeReleaseServer::start();
    let u = updater(
        &rt,
        &server,
        InstallMethod::LinuxDeb,
        Some(state_file.clone()),
    );
    assert_eq!(
        u.view().rolled_back.map(|r| r.version).as_deref(),
        Some("0.2.0")
    );
    u.clear_rolled_back();
    assert_eq!(u.view().rolled_back, None);
    assert_eq!(UpdateState::load(&state_file).rolled_back, None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// End to end on this Mac with the real program (ignored: it needs the
/// app built twice by `packaging/macos/build.sh`): the old version, run as
/// it is, checks the fake release server on its own; then the flow
/// downloads the new disk image, verifies it, copies the new bundle out with
/// `hdiutil` and `ditto`, swaps it in and starts it, and the new version
/// reports that its window is up.
///
/// ```sh
/// E2E_APP=".../Applications/ReXen Center.app" E2E_DMG=.../rexen-center-0.1.1-macos-arm64.dmg \
/// E2E_RUNNING=0.1.0 E2E_CONFIG=<empty folder> \
/// cargo test -p xen-update real_macos_app_update -- --ignored --nocapture
/// ```
#[cfg(target_os = "macos")]
#[test]
#[ignore = "needs two builds of the macOS app (see the doc comment)"]
fn real_macos_app_update() {
    use crate::method::{Probe, SystemProbe};

    let env = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
    let app = PathBuf::from(env("E2E_APP"));
    let dmg = PathBuf::from(env("E2E_DMG"));
    let running = Version::parse(&env("E2E_RUNNING")).unwrap();
    let config = PathBuf::from(env("E2E_CONFIG"));
    let name = dmg.file_name().unwrap().to_string_lossy().into_owned();
    let new_version = name
        .strip_prefix("rexen-center-")
        .and_then(|n| n.split('-').next())
        .and_then(Version::parse)
        .expect("a release disk image name");
    let exe = app.join("Contents/MacOS/rexencenter");
    let server = FakeReleaseServer::start();
    server.publish(
        &product(),
        &FakeRelease::new(&new_version.to_string()).asset(&name, std::fs::read(&dmg).unwrap()),
    );
    let latest = FakeReleaseServer::latest_path(&product());
    let pids = |pattern: &str| -> Vec<String> {
        let out = std::process::Command::new("/usr/bin/pgrep")
            .args(["-f", pattern])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    };

    // 1. The old version, started as it is, checks on its own after its
    //    start delay.
    let mut old = std::process::Command::new(&exe)
        .env("REXENCENTER_UPDATE_API", server.url())
        .env("REXENCENTER_CONFIG_DIR", &config)
        .env("REXENCENTER_KEYRING_SERVICE", "rexen-center-e2e")
        .env_remove(apply::ACK_ENV)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let check = loop {
        if let Some(r) = server.requests().into_iter().find(|r| r.path == latest) {
            break r;
        }
        assert!(Instant::now() < deadline, "the app did not check");
        std::thread::sleep(Duration::from_millis(200));
    };
    println!(
        "the running app checked: GET {} {:?}",
        check.path, check.headers
    );
    // E2E_HOLD: seconds to leave it open (to look at its update button).
    let hold = std::env::var("E2E_HOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    std::thread::sleep(Duration::from_secs(hold));
    let _ = old.kill();
    let _ = old.wait();

    // 2. The update, as the window drives it and `main` completes it.
    struct BundleProbe(PathBuf);
    impl Probe for BundleProbe {
        fn os(&self) -> crate::Os {
            SystemProbe.os()
        }
        fn exe(&self) -> Option<PathBuf> {
            Some(self.0.clone())
        }
        fn is_file(&self, path: &Path) -> bool {
            SystemProbe.is_file(path)
        }
        fn read_to_string(&self, path: &Path) -> Option<String> {
            SystemProbe.read_to_string(path)
        }
        fn env(&self, name: &str) -> Option<String> {
            SystemProbe.env(name)
        }
        fn package_owner(&self, path: &Path, package: &str) -> Option<crate::method::PackageDb> {
            SystemProbe.package_owner(path, package)
        }
    }
    let method = crate::method::detect(&product(), &BundleProbe(exe.clone()));
    println!("detected: {method:?}");
    assert_eq!(
        method,
        InstallMethod::MacBundle {
            bundle: app.clone()
        }
    );
    let rt = runtime();
    let state_file = config.join("update.toml");
    // The fake server's release is signed with the tests' key.
    let u = Updater::with_keys(
        Config {
            product: product(),
            running: running.clone(),
            method,
            arch: Arch::current(),
            api: server.url().to_owned(),
            state_file: Some(state_file.clone()),
            start_delay: Duration::ZERO,
        },
        vec![TestKey::trusted().public()],
        rt.handle().clone(),
        || {},
    );
    u.check(true);
    let Phase::Available(offer) = wait_for(&u, "offer", settled) else {
        panic!("{:?}", u.view())
    };
    println!(
        "offered {} with {:?}",
        offer.version,
        offer.package.map(|p| p.name)
    );
    u.install();
    let phase = wait_for(&u, "ready", settled);
    assert!(
        matches!(
            phase,
            Phase::Ready {
                kind: ReadyKind::Restart,
                ..
            }
        ),
        "{phase:?}"
    );
    let action = u.take_post_exit().expect("an update to complete");
    let started = Instant::now();
    run_post_exit(action, Some(&state_file));
    println!(
        "swapped, started and acknowledged in {:?}",
        started.elapsed()
    );
    let plist = std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
    let installed = crate::method::plist_string(&plist, "CFBundleShortVersionString");
    println!("installed version now {installed:?}");
    assert_eq!(installed, Some(new_version.to_string().as_str()));
    assert_eq!(UpdateState::load(&state_file).rolled_back, None);
    assert!(!app.parent().unwrap().join(apply::WORK_DIR).exists());
    let running_now = pids(&exe.to_string_lossy());
    println!("new version running as {running_now:?}");
    assert!(!running_now.is_empty(), "the new version is running");
    for pid in running_now {
        let _ = std::process::Command::new("/bin/kill").arg(pid).status();
    }
    let downloads = server
        .requests()
        .into_iter()
        .map(|r| r.path)
        .collect::<Vec<_>>();
    println!("requests: {downloads:?}");
}
