//! F14b on a real host through the real window code (an ignored test, run
//! with the `XENADMIN_REAL_*` variables of the other real-host tests and
//! `XENADMIN_REAL_LOG` outside the repository), rehearsed on the mock.
//!
//! **Strictly read only.** The test changes only this console's own
//! preferences (in memory): it draws the main window, the pool's General
//! and Memory tabs and a server's Performance graphs in the light and the
//! dark theme, then in the test-only pseudo-language, and checks that the
//! catalogue's strings are the ones shown next to the real data. No menu
//! entry or button that acts on the host is clicked. The rehearsal checks
//! that the only calls made are reads. Only counts are logged (no names).

use std::time::{Duration, Instant};

use egui_kittest::Harness;
use xen_core::{Command, Preferences, ThemeSetting};

use super::provisioning::{select, shows};
use super::*;
use crate::perf_strings::perf_strings;
use crate::tab_strings::tab_strings;

fn note(log: &mut String, line: String) {
    println!("{line}");
    log.push_str(&line);
    log.push('\n');
}

#[test]
#[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables); reads only"]
fn real_f14b_themes_and_language_read_only() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let runtime = runtime();
    let mut h = real_host_harness(&runtime);
    let log = f14b_run(&mut h);
    h.state().backend.shutdown_blocking(Duration::from_secs(5));
    std::fs::write(&log_path, log).expect("write the log");
}

/// The same run against the mock (the sanitized capture of the real host).
#[test]
fn f14b_real_run_on_the_mock() {
    let runtime = runtime();
    let server = runtime.block_on(MockServer::start_with_records(
        true,
        xapi_mock::capture_records(),
    ));
    let mut h = app_harness(&runtime);
    connect_through_dialog(&mut h, &server.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "certificate prompt", |h| {
        h.query_by_label(strings().cert_trust).is_some()
    });
    h.get_by_label(strings().cert_trust).click();
    step_until(&mut h, "connected", |h| {
        first_conn(h).is_some_and(|c| c.status.is_connected() && c.rbac.is_some())
    });
    let log = f14b_run(&mut h);
    assert!(log.contains("back to English"), "{log}");
    for call in server.calls() {
        let c = call.trim_start_matches("Async.");
        let read = c.contains(".get_")
            || c.starts_with("event.")
            || c.starts_with("session.")
            || c.starts_with("http/")
            || c == "task.destroy";
        assert!(read, "a call that is not a read: {call}");
    }
}

fn set(h: &mut Harness<'_, XenAdminApp>, change: impl FnOnce(&mut Preferences)) {
    let mut p = h.state().backend.state().settings.preferences.clone();
    change(&mut p);
    let wanted = p.clone();
    h.state().backend.send(Command::SetPreferences(p));
    step_until(h, "preferences applied", |h| {
        h.state().backend.state().settings.preferences == wanted
    });
    settle(h);
}

/// Steps for `d` (graphs fill in, the event loop keeps running).
fn run_for(h: &mut Harness<'_, XenAdminApp>, d: Duration) {
    let until = Instant::now() + d;
    while Instant::now() < until {
        h.step();
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn f14b_run(h: &mut Harness<'_, XenAdminApp>) -> String {
    let mut log = String::new();
    let s = strings();
    let t = tab_strings();
    let conn = first_conn(h).expect("connected");
    let (pool_ref, _) = conn.model.pool().expect("a pool");
    let pool_ref = pool_ref.clone();
    let host_ref = conn
        .model
        .hosts
        .iter()
        .find(|(_, host)| xen_core::objects::host_is_live(&conn.model, host))
        .map(|(r, _)| r.clone())
        .expect("a live host");
    note(
        &mut log,
        format!(
            "connected: {} host(s), {} VM records",
            conn.model.hosts.len(),
            conn.model.vms.len()
        ),
    );

    for theme in [ThemeSetting::Light, ThemeSetting::Dark] {
        set(h, |p| p.theme = theme);
        let dark = h.ctx.theme() == egui::Theme::Dark;
        assert_eq!(dark, theme == ThemeSetting::Dark);
        select(h, Selection::Pool(conn.id, pool_ref.clone()));
        click_tab(h, t.tab_general);
        assert!(shows(h, t.tab_memory));
        click_tab(h, t.tab_memory);
        settle(h);
        select(h, Selection::Host(conn.id, host_ref.clone()));
        click_tab(h, perf_strings().tab_performance);
        run_for(h, Duration::from_secs(4));
        // Main window strings from the catalogue, beside the real data.
        for label in [s.menu_file, s.menu_view, s.menu_tools, s.events_title] {
            assert!(shows(h, label), "{label}");
        }
        note(
            &mut log,
            format!(
                "theme {theme:?}: dark_mode={dark}; pool General and Memory, server Performance drawn; menus and Events labelled from the catalogue"
            ),
        );
    }

    // The pseudo-language: every catalogue string is wrapped in [[…]].
    set(h, |p| p.language = "en-XA".into());
    select(h, Selection::Pool(conn.id, pool_ref.clone()));
    settle(h);
    for label in ["[[File]]", "[[View]]", "[[General]]", "[[Events]]"] {
        assert!(shows(h, label), "{label}");
    }
    assert!(!shows(h, s.menu_file));
    let pseudo = h.query_all_by_label_contains("[[").count();
    note(
        &mut log,
        format!("pseudo-language: {pseudo} labels from the catalogue on the pool's page"),
    );
    set(h, |p| {
        p.language = String::new();
        p.theme = ThemeSetting::System;
    });
    assert!(shows(h, s.menu_file));
    note(&mut log, "back to English and the system theme".into());
    log
}
