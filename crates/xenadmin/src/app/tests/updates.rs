//! Updates in the window, against a fake release server on the local
//! machine (no real network): the automatic check puts the update button at
//! the top of the window; it opens the release notes; Later, Skip This
//! Version, Help > Check for Updates…; the download, verified, and Restart
//! Now, which leaves the update for `main` and closes the window as Quit
//! does. Also the macOS application menu's item and the Options setting,
//! and a release whose signature is missing or wrong: only its page.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use egui::ViewportCommand;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use xen_core::Store;
use xen_update::testing::{
    FakeRelease, FakeReleaseServer, Signing, TestKey, install_tarball, release_tarball,
};
use xen_update::updater::{Config, PostExit, Updater};
use xen_update::{Arch, InstallMethod, Version};

use super::*;
use crate::update_strings::update_strings;
use crate::update_ui::{PostExitSlot, UpdateUi};

fn temp(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rexencenter-updates-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The 0.2.0 release with the Linux tarball (and its checksums).
fn publish(server: &FakeReleaseServer) {
    let product = branding::update_product();
    server.publish(
        &product,
        &FakeRelease::new("0.2.0").asset(
            "rexen-center-0.2.0-linux-x86_64.tar.gz",
            release_tarball(&product, "0.2.0"),
        ),
    );
}

fn updater(
    runtime: &tokio::runtime::Runtime,
    server: &FakeReleaseServer,
    method: InstallMethod,
    state_file: Option<PathBuf>,
) -> Updater {
    // The fake server signs with the tests' key.
    Updater::with_keys(
        Config {
            product: branding::update_product(),
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

fn harness(
    runtime: &tokio::runtime::Runtime,
    store: Arc<Store>,
    updater: Updater,
    slot: PostExitSlot,
) -> Harness<'static, XenAdminApp> {
    let mut h = app_harness_wrapped(runtime, store, move |app| {
        app.with_updates(UpdateUi::new(updater, slot))
    });
    // The connect dialog opens on start; it is not part of this.
    let s = strings();
    step_until(&mut h, "connect dialog", |h| {
        h.query_by_label(s.connect_title).is_some()
    });
    h.state_mut().connect.open = false;
    h.step();
    h
}

/// Clicks `label` once the window it is in has settled (egui ignores
/// clicks in a window's first, sizing, frame).
fn click(h: &mut Harness<'_, XenAdminApp>, label: &str) {
    settle(h);
    h.get_by_label(label).click();
    h.step();
}

fn commands(h: &Harness<'_, XenAdminApp>) -> Vec<ViewportCommand> {
    h.output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

#[test]
fn the_update_button_the_dialog_and_restart() {
    let runtime = runtime();
    let server = FakeReleaseServer::start();
    publish(&server);
    let dir = temp("flow");
    let method = install_tarball(&branding::update_product(), &dir, "0.1.1");
    let InstallMethod::LinuxTarball { root } = method.clone() else {
        unreachable!()
    };
    let state_file = dir.join("update.toml");
    let slot = PostExitSlot::default();
    let updater = updater(&runtime, &server, method, Some(state_file.clone()));
    let mut h = harness(&runtime, Arc::new(Store::default()), updater, slot.clone());
    let u = update_strings();
    let button = fill(u.button, &[("version", "0.2.0")]);

    // The automatic check (on by default) shows the button at the top.
    step_until(&mut h, "update button", |h| {
        h.query_by_label(&button).is_some()
    });
    click(&mut h, &button);
    step_until(&mut h, "dialog", |h| {
        h.query_by_label(u.notes_heading).is_some()
    });
    let available = fill(
        u.available,
        &[
            ("product", branding::PRODUCT_NAME),
            ("version", "0.2.0"),
            ("running", "0.1.1"),
        ],
    );
    h.get_by_label(&available);
    h.get_by_label("## What's Changed\n* Release 0.2.0");
    h.get_by_label(&fill(u.installed_as, &[("method", u.method_tarball)]));
    h.get_by_label(u.integrity_note);
    assert!(is_enabled(&h, u.install_restart));

    // Later: the dialog and the button go.
    click(&mut h, u.later);
    step_until(&mut h, "button gone", |h| {
        h.query_by_label(&button).is_none() && h.query_by_label(u.notes_heading).is_none()
    });

    // Help > Check for Updates… brings it back; Skip This Version is
    // remembered.
    click(&mut h, strings().menu_help);
    step_until(&mut h, "help menu", |h| {
        h.query_by_label(u.menu_check).is_some()
    });
    click(&mut h, u.menu_check);
    step_until(&mut h, "offer again", |h| {
        h.query_by_label(u.skip).is_some()
    });
    click(&mut h, u.skip);
    step_until(&mut h, "skipped", |h| {
        h.query_by_label(&button).is_none() && h.query_by_label(u.skip).is_none()
    });
    assert_eq!(
        xen_update::state::UpdateState::load(&state_file)
            .skipped_version
            .as_deref(),
        Some("0.2.0")
    );

    // Asked again, it is offered all the same; install it.
    click(&mut h, strings().menu_help);
    step_until(&mut h, "help menu", |h| {
        h.query_by_label(u.menu_check).is_some()
    });
    click(&mut h, u.menu_check);
    step_until(&mut h, "offer", |h| {
        h.query_by_label(u.install_restart).is_some()
    });
    click(&mut h, u.install_restart);
    step_until(&mut h, "ready", |h| {
        h.query_by_label(u.restart_now).is_some()
    });
    h.get_by_label(&fill(
        u.ready_restart,
        &[("product", branding::PRODUCT_NAME)],
    ));
    // Nothing is replaced while the window is open.
    assert_eq!(
        std::fs::read_to_string(root.join("bin/rexencenter")).unwrap(),
        "binary 0.1.1"
    );
    click(&mut h, u.restart_now);
    // The window closes as File > Quit does; the update waits for `main`.
    assert!(commands(&h).contains(&ViewportCommand::Close));
    let action = slot.lock().unwrap().take();
    let Some(PostExit::Replace { prepared, scratch }) = action else {
        panic!("no update left for main")
    };
    assert_eq!(prepared.relaunch, root.join("bin/rexencenter"));
    assert_eq!(prepared.version.to_string(), "0.2.0");
    drop(h);
    let _ = std::fs::remove_dir_all(scratch);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_tampered_download_is_refused_and_the_page_offered() {
    let runtime = runtime();
    let server = FakeReleaseServer::start();
    let product = branding::update_product();
    let tarball = "rexen-center-0.2.0-linux-x86_64.tar.gz";
    server.publish(
        &product,
        &FakeRelease {
            checksums: xen_update::testing::Checksums::WrongFor(tarball.into()),
            ..FakeRelease::new("0.2.0").asset(tarball, release_tarball(&product, "0.2.0"))
        },
    );
    let dir = temp("tampered");
    let method = install_tarball(&product, &dir, "0.1.1");
    let updater = updater(&runtime, &server, method, None);
    let mut h = harness(
        &runtime,
        Arc::new(Store::default()),
        updater,
        PostExitSlot::default(),
    );
    let u = update_strings();
    let button = fill(u.button, &[("version", "0.2.0")]);
    step_until(&mut h, "update button", |h| {
        h.query_by_label(&button).is_some()
    });
    click(&mut h, &button);
    step_until(&mut h, "offer", |h| {
        h.query_by_label(u.install_restart).is_some()
    });
    click(&mut h, u.install_restart);
    step_until(&mut h, "refusal", |h| {
        h.query_by_label_contains("does not match its checksum")
            .is_some()
    });
    h.get_by_label(u.release_page);
    assert!(h.query_by_label(u.restart_now).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unsigned_or_badly_signed_release_offers_only_its_page() {
    let runtime = runtime();
    let product = branding::update_product();
    let tarball = "rexen-center-0.2.0-linux-x86_64.tar.gz";
    let u = update_strings();
    for (signing, reason) in [
        (Signing::Missing, u.no_signature.to_owned()),
        (
            Signing::TamperedSignature,
            fill(
                u.bad_signature,
                &[("error", "the signature does not match the checksum file")],
            ),
        ),
    ] {
        let server = FakeReleaseServer::start();
        server.publish(
            &product,
            &FakeRelease {
                signing,
                ..FakeRelease::new("0.2.0").asset(tarball, release_tarball(&product, "0.2.0"))
            },
        );
        let dir = temp("unsigned");
        let method = install_tarball(&product, &dir, "0.1.1");
        let updater = updater(&runtime, &server, method, None);
        let mut h = harness(
            &runtime,
            Arc::new(Store::default()),
            updater,
            PostExitSlot::default(),
        );
        let button = fill(u.button, &[("version", "0.2.0")]);
        step_until(&mut h, "update button", |h| {
            h.query_by_label(&button).is_some()
        });
        click(&mut h, &button);
        step_until(&mut h, "dialog", |h| h.query_by_label(&reason).is_some());
        // The page, as the main button and as the link; no install.
        assert_eq!(h.query_all_by_label(u.release_page).count(), 2);
        assert!(h.query_by_label(u.install_restart).is_none());
        assert!(h.query_by_label(u.integrity_note).is_none());
        assert!(!server.requests().iter().any(|r| r.path.ends_with(tarball)));
        drop(h);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn the_reasons_a_release_is_not_trusted() {
    use crate::update_ui::untrusted_text;
    use xen_update::signature::SignatureError;
    use xen_update::updater::Untrusted;
    assert_eq!(
        untrusted_text(&Untrusted::NoChecksumFile),
        "This release has no checksum file, so its download cannot be verified. Download the new version from the release page."
    );
    assert_eq!(
        untrusted_text(&Untrusted::NoSignature),
        "This release is not signed with the project's release key, so it is not installed from here. Download the new version from the release page."
    );
    assert_eq!(
        untrusted_text(&Untrusted::BadSignature(SignatureError::UnknownKey(
            "0123456789ABCDEF".into()
        ))),
        "This release's signature could not be verified (the signature was made with key 0123456789ABCDEF, which this version does not trust), so it is not installed from here. It may have been changed after it was published: check the release page before downloading anything."
    );
    assert!(
        untrusted_text(&Untrusted::BadSignature(SignatureError::Prehashed))
            .contains("pre-hashed mode")
    );
}

#[test]
fn turned_off_in_options_and_failures_are_quiet() {
    let runtime = runtime();
    let server = FakeReleaseServer::start();
    publish(&server);
    let store = Arc::new(Store::default());
    store.update_settings(|s| s.preferences.check_for_updates = false);
    let dir = temp("off");
    let method = install_tarball(&branding::update_product(), &dir, "0.1.1");
    let updater = updater(&runtime, &server, method.clone(), None);
    let mut h = harness(&runtime, store, updater, PostExitSlot::default());
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(server.requests().is_empty(), "no automatic check");

    // Options shows the setting; turning it on checks.
    let u = update_strings();
    h.state_mut().options_open = true;
    step_until(&mut h, "options", |h| {
        h.query_by_label(u.options_check).is_some()
    });
    click(&mut h, u.options_check);
    let button = fill(u.button, &[("version", "0.2.0")]);
    step_until(&mut h, "update button", |h| {
        h.query_by_label(&button).is_some()
    });
    assert!(
        h.state()
            .backend
            .state()
            .settings
            .preferences
            .check_for_updates
    );

    // A server that fails: the automatic check says nothing, the manual
    // one says why.
    let failing = FakeReleaseServer::start();
    let product = branding::update_product();
    failing.set(&FakeReleaseServer::latest_path(&product), 503, "down");
    let updater = self::updater(&runtime, &failing, method, None);
    let mut h = harness(
        &runtime,
        Arc::new(Store::default()),
        updater,
        PostExitSlot::default(),
    );
    step_until(&mut h, "automatic check", |_| {
        !failing.requests().is_empty()
    });
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(h.query_by_label(u.dialog_title).is_none());
    assert!(h.state().errors.is_empty(), "{:?}", h.state().errors);
    h.state_mut().update.check_now();
    let failed = fill(
        u.check_failed,
        &[("error", "the release server answered 503")],
    );
    step_until(&mut h, "failure shown", |h| {
        h.query_by_label(&failed).is_some()
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn without_an_updater_the_menu_item_is_disabled() {
    let runtime = runtime();
    let mut h = app_harness(&runtime);
    let u = update_strings();
    click(&mut h, strings().menu_help);
    step_until(&mut h, "help menu", |h| {
        h.query_by_label(u.menu_check).is_some()
    });
    assert!(!is_enabled(&h, u.menu_check));
}

#[test]
fn installation_methods_are_described() {
    use crate::update_ui::method_label;
    use xen_update::UnknownReason;
    let u = update_strings();
    assert_eq!(
        method_label(
            &InstallMethod::MacBundle {
                bundle: "/Applications/ReXen Center.app".into()
            },
            Arch::Aarch64
        ),
        "macOS app (Apple silicon)"
    );
    assert_eq!(
        method_label(
            &InstallMethod::MacBundle {
                bundle: "/Applications/ReXen Center.app".into()
            },
            Arch::X86_64
        ),
        "macOS app (Intel)"
    );
    assert_eq!(
        method_label(&InstallMethod::LinuxRpm, Arch::X86_64),
        u.method_rpm
    );
    assert_eq!(
        method_label(
            &InstallMethod::Unknown(UnknownReason::DevelopmentBuild),
            Arch::X86_64
        ),
        "not recognised (a build from source)"
    );
    // The product the updater looks for is this one.
    let product = branding::update_product();
    assert_eq!(product.repository, "carcamano/rexen-center");
    assert_eq!(product.app_id, "rexen-center");
    assert_eq!(product.binary, "rexencenter");
    assert_eq!(product.bundle_name, "ReXen Center");
    // packaging/macos/Info.plist: local.@ID@.console
    assert_eq!(product.bundle_id, "local.rexen-center.console");
    let plist = include_str!("../../../../../packaging/macos/Info.plist");
    assert!(plist.contains("<string>local.@ID@.console</string>"));
}
