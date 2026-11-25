//! F6: the Performance tab, the Alerts view and the performance alert
//! settings through the real window code (egui_kittest): against the mock
//! always, and against a real host when asked (ignored test
//! `real_performance_fluxer`).

use std::collections::HashSet;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use xapi::{Message, Ref, Vm, VmPowerState};
use xapi_mock::MockServer;
use xen_core::PerfTarget;
use xen_core::perfmon::{self, PerfmonConfig};
use xen_core::performance::PerfStatus;
use xen_rrd::Interval;

use super::*;
use crate::perf_view::{PerfKey, Zoom};

const GUEST: &str = "OpaqueRef:vm-0001";
const HOST: &str = "OpaqueRef:host-0001";

fn mock_harness(runtime: &tokio::runtime::Runtime) -> (MockServer, Harness<'static, XenAdminApp>) {
    let server = runtime.block_on(MockServer::start_with_records(
        true,
        xapi_mock::capture_records(),
    ));
    let mut harness = app_harness(runtime);
    connect_through_dialog(&mut harness, &server.address(), xapi_mock::PASSWORD);
    step_until(&mut harness, "certificate prompt", |h| {
        h.query_by_label(strings().cert_trust).is_some()
    });
    harness.get_by_label(strings().cert_trust).click();
    step_until(&mut harness, "connected", |h| {
        first_conn(h).is_some_and(|c| c.status.is_connected() && c.rbac.is_some())
    });
    (server, harness)
}

/// The data of an open feed (the tab opens it; this never does).
fn perf_data(h: &mut Harness<'_, XenAdminApp>, key: &PerfKey) -> Option<xen_core::PerfData> {
    let app = h.state_mut();
    if !app.performance.is_open(key) {
        return None;
    }
    Some(app.performance.handle(&app.backend, key).data())
}

/// Scrolls the node labelled `label` into view and clicks it.
fn click_visible(h: &mut Harness<'_, XenAdminApp>, label: &str) {
    h.get_by_label(label).scroll_to_me();
    settle(h);
    h.get_by_label(label).click();
    settle(h);
}

fn select(h: &mut Harness<'_, XenAdminApp>, selection: Selection) {
    h.state_mut().selection = Some(selection);
    settle(h);
}

/// Shows the Performance tab of the selection.
fn performance_tab(h: &mut Harness<'_, XenAdminApp>) {
    click_first(h, perf_strings().tab_performance);
    settle(h);
    assert_eq!(h.state().tab, Tab::Performance);
}

fn last5(h: &mut Harness<'_, XenAdminApp>, key: &PerfKey, name: &str) -> Option<i64> {
    perf_data(h, key)?
        .archives
        .series(Interval::FiveSeconds, name)
        .last()
        .map(|p| p.0)
}

/// The Performance tab of a host and of a VM against the mock: the
/// default graphs, live updates, the zoom, and the feed closing when
/// another object is selected.
#[test]
fn the_performance_tab_draws_live_graphs() {
    let runtime = runtime();
    let (server, mut harness) = mock_harness(&runtime);
    let p = perf_strings();
    let conn = first_conn(&harness).unwrap().id;
    let host_key: PerfKey = (conn, PerfTarget::Host(Ref::new(HOST)));

    select(&mut harness, Selection::Host(conn, Ref::new(HOST)));
    performance_tab(&mut harness);
    step_until(&mut harness, "host graphs live", |h| {
        h.query_by_label(p.graph_loading).is_none() && h.state().performance.is_open(&host_key)
    });
    let live = |h: &mut Harness<'_, XenAdminApp>| {
        perf_data(h, &host_key).is_some_and(|d| d.status == PerfStatus::Live)
    };
    let deadline = Instant::now() + WAIT;
    while !live(&mut harness) {
        assert!(Instant::now() < deadline, "host graphs not live");
        harness.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    for title in [p.graph_cpu, p.graph_memory, p.graph_network] {
        harness.get_by_label(title);
    }
    assert!(
        harness.query_by_label(p.graph_disk).is_none(),
        "hosts have no disk graph"
    );
    // Legend entries with the original's names.
    for name in ["CPU 0", "Used Memory", "NIC 0 Receive"] {
        assert!(harness.query_all_by_label(name).next().is_some(), "{name}");
    }
    // New rows arrive.
    let before = last5(&mut harness, &host_key, "cpu0").unwrap();
    server.advance_rrd_clock(25);
    let deadline = Instant::now() + WAIT;
    while last5(&mut harness, &host_key, "cpu0").is_none_or(|t| t < before + 25) {
        assert!(Instant::now() < deadline, "no new rows");
        harness.step();
        std::thread::sleep(Duration::from_millis(20));
    }

    // Zoom: a week.
    click_first(&mut harness, p.zoom);
    settle(&mut harness);
    click_first(&mut harness, p.zoom_week);
    settle(&mut harness);
    assert_eq!(harness.state().performance.zoom, Zoom::Week);
    harness.get_by_label(&crate::i18n::fill(p.showing, &[("range", p.zoom_week)]));

    // A VM: four graphs. The host's feed closes.
    select(&mut harness, Selection::Vm(conn, Ref::new(GUEST)));
    assert_eq!(harness.state().tab, Tab::Performance, "the tab is kept");
    let vm_key: PerfKey = (conn, PerfTarget::Vm(Ref::new(GUEST)));
    let deadline = Instant::now() + WAIT;
    while !perf_data(&mut harness, &vm_key).is_some_and(|d| d.status == PerfStatus::Live) {
        assert!(Instant::now() < deadline, "VM graphs not live");
        harness.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    settle(&mut harness);
    assert!(!harness.state().performance.is_open(&host_key));
    for title in [p.graph_cpu, p.graph_memory, p.graph_network, p.graph_disk] {
        harness.get_by_label(title);
    }
    assert!(harness.query_all_by_label("Disk 0 Read").next().is_some());
    // Selecting something without graphs closes the VM's feed too.
    select(
        &mut harness,
        Selection::Pool(conn, Ref::new("OpaqueRef:pool-0001")),
    );
    assert!(!harness.state().performance.is_open(&vm_key));
    assert!(server.call_count("http/host_rrds") == 1);
}

/// Alerts from messages in the notifications area: a perfmon alarm and an
/// ordinary message, their titles and descriptions, Dismiss Selected and
/// Dismiss All with their questions, and Alarm Settings.
#[test]
fn the_alerts_view_lists_and_dismisses_alerts() {
    let runtime = runtime();
    let (server, mut harness) = mock_harness(&runtime);
    let p = perf_strings();
    let conn = first_conn(&harness).unwrap().id;
    let vm_uuid = vm_rec(&harness, GUEST).uuid.clone();
    let alarm = server.create_message(
        "ALARM",
        3,
        "VM",
        &vm_uuid,
        "value: 0.953\nconfig:\n<variable>\n<name value=\"cpu_usage\"/>\n<alarm_trigger_level value=\"0.9\"/>\n<alarm_trigger_period value=\"60\"/>\n</variable>",
    );
    let other = server.create_message("XENADMIN_RS_TEST", 5, "VM", &vm_uuid, "a test message");
    let has = |h: &Harness<'_, XenAdminApp>, r: &str| {
        first_conn(h).is_some_and(|c| c.model.messages.contains_key(&Ref::<Message>::new(r)))
    };
    step_until(&mut harness, "messages", |h| {
        has(h, &alarm) && has(h, &other)
    });
    let total = AlertsView::count(&harness.state().backend.state());
    // The capture's own alert plus ours (lifecycle messages are not alerts).
    assert_eq!(total, 3);
    let tab = crate::i18n::fill(p.alerts_unread, &[("count", &total.to_string())]);
    step_until(&mut harness, "the Alerts tab's count", |h| {
        h.query_by_label(&tab).is_some()
    });
    click_first(&mut harness, &tab);
    settle(&mut harness);
    harness.get_by_label("CPU Usage Alarm");
    harness.get_by_label("CPU usage on guest-10 has been on average 95% for the last 1 minute(s).");
    harness.get_by_label("guest-10: XENADMIN_RS_TEST");
    harness.get_by_label("a test message");

    // Select the test message only and dismiss it.
    click_visible(&mut harness, "guest-10: XENADMIN_RS_TEST");
    assert_eq!(
        harness.state().alerts.selected(),
        [(conn, Ref::<Message>::new(&other))]
    );
    click_first(&mut harness, p.alerts_dismiss_selected);
    settle(&mut harness);
    assert_eq!(harness.state().alerts.asking(), Some(false));
    harness.get_by_label(p.alerts_confirm_selected);
    click_first(&mut harness, p.alerts_confirm_yes);
    step_until(&mut harness, "dismissed", |h| !has(h, &other));
    assert!(has(&harness, &alarm), "only the selected alert goes");
    assert_eq!(server.call_count("message.destroy"), 1);
    step_until(&mut harness, "the history entry", |h| {
        op_done(h, "Dismissing alert 'XENADMIN_RS_TEST'")
    });

    // Dismiss All asks first; No keeps everything.
    click_first(&mut harness, p.alerts_dismiss_all);
    settle(&mut harness);
    assert_eq!(harness.state().alerts.asking(), Some(true));
    harness.get_by_label(p.alerts_confirm_all);
    click_first(&mut harness, p.alerts_confirm_no);
    settle(&mut harness);
    assert_eq!(harness.state().alerts.asking(), None);
    assert_eq!(server.call_count("message.destroy"), 1);

    // Alarm Settings opens the VM's alert settings.
    click_visible(&mut harness, p.alerts_alarm_action);
    assert!(
        harness.state().perfmon.is_some(),
        "Alarm Settings opened nothing"
    );
    harness.get_by_label(p.pm_cpu_check);
    click_first(&mut harness, p.pm_cancel);
    settle(&mut harness);
    assert!(harness.state().perfmon.is_none());

    // Dismiss All, Yes.
    click_first(&mut harness, p.alerts_dismiss_all);
    settle(&mut harness);
    click_first(&mut harness, p.alerts_confirm_yes_all);
    step_until(&mut harness, "all dismissed", |h| {
        AlertsView::count(&h.state().backend.state()) == 0
    });
    settle(&mut harness);
    harness.get_by_label(p.alerts_empty);
    // Lifecycle messages stay (they belong to the graphs).
    assert!(!first_conn(&harness).unwrap().model.messages.is_empty());
}

/// The alert settings of a VM from its Performance tab: CPU alerts on,
/// saved as the original writes them, perfmon refreshed; then off again,
/// which removes the key.
#[test]
fn alert_settings_are_saved_from_the_performance_tab() {
    let runtime = runtime();
    let (server, mut harness) = mock_harness(&runtime);
    let p = perf_strings();
    let conn = first_conn(&harness).unwrap().id;
    select(&mut harness, Selection::Vm(conn, Ref::new(GUEST)));
    performance_tab(&mut harness);
    click_first(&mut harness, p.alert_settings);
    settle(&mut harness);
    harness.get_by_label(p.pm_cpu_check).click();
    settle(&mut harness);
    harness.get_by_label("When CPU usage exceeds 50% for 1 min(s)");
    click_first(&mut harness, p.pm_ok);
    let perfmon_of = |h: &Harness<'_, XenAdminApp>| {
        vm_rec(h, GUEST)
            .other_config
            .get(perfmon::PERFMON_KEY)
            .cloned()
    };
    step_until(&mut harness, "saved", |h| perfmon_of(h).is_some());
    let saved = PerfmonConfig::parse(&perfmon_of(&harness).unwrap());
    let cpu = saved.get(perfmon::CPU_USAGE).unwrap();
    assert_eq!(
        (cpu.level, cpu.period, cpu.inhibit),
        (Some(0.5), Some(60.0), Some(3600.0))
    );
    step_until(&mut harness, "perfmon refreshed", |_| {
        server
            .plugin_calls()
            .contains(&("perfmon".into(), "refresh".into()))
    });
    step_until(&mut harness, "the history entry", |h| {
        op_done(h, "Saving alerts for 'guest-10'")
    });

    // Reopened: filled from the saved settings; off removes the key.
    click_first(&mut harness, p.alert_settings);
    settle(&mut harness);
    assert!(
        harness
            .state()
            .perfmon
            .as_ref()
            .unwrap()
            .form
            .setting(perfmon::AlertGroup::Cpu)
            .unwrap()
            .enabled
    );
    harness.get_by_label(p.pm_cpu_check).click();
    settle(&mut harness);
    click_first(&mut harness, p.pm_ok);
    step_until(&mut harness, "removed", |h| perfmon_of(h).is_none());

    // OK with nothing changed sends nothing.
    let calls = server.call_count("VM.remove_from_other_config");
    click_first(&mut harness, p.alert_settings);
    settle(&mut harness);
    click_first(&mut harness, p.pm_ok);
    settle(&mut harness);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(server.call_count("VM.remove_from_other_config"), calls);
}

fn note(log: &mut String, line: String) {
    println!("{line}");
    let _ = writeln!(log, "{line}");
}

/// The newest points of a series, for the log: `(t, value)` scaled.
fn tail(d: &xen_core::PerfData, name: &str, n: usize) -> Vec<(i64, String)> {
    let s = d.archives.series(Interval::FiveSeconds, name);
    let (unit, factor) = xen_core::performance::unit_of(name, &d.sources);
    s[s.len().saturating_sub(n)..]
        .iter()
        .map(|(t, v)| (*t, crate::perf_view::value_text(unit, v * factor)))
        .collect()
}

/// The server's Performance tab on a real host, read-only: live graphs
/// for a minute, with the request count and the format logged. Ignored
/// unless asked for (same variables as `real_performance_fluxer`):
/// `cargo test -p xenadmin real_performance_host -- --ignored --nocapture`.
#[test]
#[ignore = "reads a real host's performance data (XENADMIN_REAL_* variables)"]
fn real_performance_host() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let host_addr = std::env::var("XENADMIN_REAL_HOST").expect("set XENADMIN_REAL_HOST");
    let runtime = runtime();
    let mut harness = real_host_harness(&runtime);
    let state = harness.state().backend.state();
    let conn = state.connections[0].clone();
    let host_ref = conn.model.pool().unwrap().1.master.clone();
    let key: PerfKey = (conn.id, PerfTarget::Host(host_ref.clone()));
    let mut log = String::new();
    select(&mut harness, Selection::Host(conn.id, host_ref));
    performance_tab(&mut harness);
    let started = Instant::now();
    let live = step_for(&mut harness, Duration::from_secs(120), |h| {
        h.state().performance.is_open(&key)
    }) && loop {
        harness.step();
        if perf_data(&mut harness, &key).is_some_and(|d| d.status == PerfStatus::Live) {
            break true;
        }
        if started.elapsed() > Duration::from_secs(120) {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    note(
        &mut log,
        format!(
            "live after {:.1} s: {live}",
            started.elapsed().as_secs_f64()
        ),
    );
    let first = last5(&mut harness, &key, "cpu0");
    step_for(&mut harness, Duration::from_secs(60), |_| false);
    let d = perf_data(&mut harness, &key).unwrap();
    let last = last5(&mut harness, &key, "cpu0");
    note(
        &mut log,
        format!(
            "after a minute: status {:?}, format {:?}, updates {}, newest 5 s point {first:?} -> {last:?}, cpu0 {:?}",
            d.status,
            d.format,
            d.updates,
            tail(&d, "cpu0", 3)
        ),
    );
    harness
        .state()
        .backend
        .shutdown_blocking(Duration::from_secs(5));
    note(&mut log, "logged out".into());
    std::fs::write(&log_path, log.replace(&host_addr, "<host>")).unwrap();
    assert!(
        live && last > first && d.updates >= 8,
        "the host graphs did not follow"
    );
}

/// F6 acceptance on the real host, through the real window. Ignored unless
/// asked for (same variables as `real_console_fluxer`, plus
/// `XENADMIN_REAL_LOG`):
///
/// ```text
/// cargo test -p xenadmin real_performance_fluxer -- --ignored --nocapture
/// ```
///
/// Read-only for everything but the VM named exactly "09 - Ubuntu Fluxer"
/// (refused unless exactly one VM has that name and it is halted):
///
/// 1. the host's Performance tab: live graphs (JSON or XML is logged);
/// 2. Fluxer's graphs while halted (its archive), then Start from the
///    toolbar and the graphs moving during the boot;
/// 3. a test message created on Fluxer (`message.create`, a separate
///    session), shown in the Alerts view and dismissed with Dismiss
///    Selected, nothing else selected; every other message is still there;
/// 4. Fluxer's alert settings: CPU alerts on (saved), then off again; the
///    key ends as it was found (absent, or restored to its value). The
///    perfmon refresh plugin is not called (`perfmon_refresh` off);
/// 5. Shut Down from the toolbar, leaving it halted.
#[test]
#[ignore = "operates the real VM named \"09 - Ubuntu Fluxer\" (XENADMIN_REAL_* variables)"]
fn real_performance_fluxer() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let host_addr = std::env::var("XENADMIN_REAL_HOST").expect("set XENADMIN_REAL_HOST");
    let password = std::env::var("XENADMIN_REAL_PASSWORD").expect("set XENADMIN_REAL_PASSWORD");
    let fingerprint =
        std::env::var("XENADMIN_REAL_FINGERPRINT").expect("set XENADMIN_REAL_FINGERPRINT");
    let runtime = runtime();
    let mut harness = real_host_harness_with(&runtime, |o| BackendOptions {
        perfmon_refresh: false,
        ..o
    });
    let s = strings();
    let o = ops_strings();
    let p = perf_strings();
    let state = harness.state().backend.state();
    let conn = state.connections[0].clone();
    let named: Vec<Ref<Vm>> = conn
        .model
        .vms
        .iter()
        .filter(|(_, v)| v.name_label == FLUXER && !v.is_a_template && !v.is_control_domain)
        .map(|(r, _)| r.clone())
        .collect();
    assert_eq!(
        named.len(),
        1,
        "refusing: {} VMs are named {FLUXER:?}",
        named.len()
    );
    let vm = named[0].clone();
    let fluxer = conn.model.vms[&vm].clone();
    assert_eq!(
        fluxer.power_state,
        VmPowerState::Halted,
        "refusing: {FLUXER:?} is not halted"
    );
    let original_perfmon = fluxer.other_config.get(perfmon::PERFMON_KEY).cloned();
    let messages_before: HashSet<Ref<Message>> = conn.model.messages.keys().cloned().collect();
    let host_ref = conn.model.pool().unwrap().1.master.clone();
    let mut log = String::new();
    note(
        &mut log,
        format!("VM {FLUXER:?} found once, halted; perfmon before: {original_perfmon:?}"),
    );
    note(
        &mut log,
        format!(
            "messages before: {} ({} alerts)",
            messages_before.len(),
            xen_core::alerts::alerts(&conn.model).len()
        ),
    );
    let power = |h: &Harness<'_, XenAdminApp>| vm_rec(h, vm.as_str()).power_state.clone();
    let perfmon_now = |h: &Harness<'_, XenAdminApp>| {
        vm_rec(h, vm.as_str())
            .other_config
            .get(perfmon::PERFMON_KEY)
            .cloned()
    };
    // The message this test creates (dismissed by it, or in the cleanup).
    let mut created: Option<Ref<Message>> = None;
    let login = || {
        let endpoint = xapi::Endpoint::parse(&host_addr).unwrap();
        let trust = xapi::TrustStore::new();
        trust.trust(
            &endpoint.authority(),
            xapi::Fingerprint::parse(&fingerprint).unwrap(),
        );
        let client =
            xapi::JsonRpcClient::new(&endpoint, &trust, &xapi::ClientOptions::default()).unwrap();
        runtime.block_on(xapi::Session::login_with_password(
            std::sync::Arc::new(client),
            "root",
            &password,
            "xenadmin-rs-f6-test",
        ))
    };

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // 1. The host's graphs.
        let host_key: PerfKey = (conn.id, PerfTarget::Host(host_ref.clone()));
        select(&mut harness, Selection::Host(conn.id, host_ref.clone()));
        performance_tab(&mut harness);
        let live = step_for(&mut harness, Duration::from_secs(120), |h| {
            h.state().performance.is_open(&host_key)
        }) && {
            let started = Instant::now();
            loop {
                harness.step();
                if perf_data(&mut harness, &host_key)
                    .is_some_and(|d| d.status == PerfStatus::Live && d.updates > 0)
                {
                    break true;
                }
                if started.elapsed() > Duration::from_secs(120) {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        let d = perf_data(&mut harness, &host_key).unwrap();
        note(
            &mut log,
            format!(
                "host graphs: status {:?}, format {:?}, data sources {}, 5 s cpu0 points {}, day points {}",
                d.status,
                d.format,
                d.sources.len(),
                d.archives.series(Interval::FiveSeconds, "cpu0").len(),
                d.archives.series(Interval::OneDay, "cpu0").len()
            ),
        );
        assert!(live, "the host graphs did not go live");
        let t0 = last5(&mut harness, &host_key, "cpu0").unwrap();
        step_for(&mut harness, Duration::from_secs(30), |_| false);
        let t1 = last5(&mut harness, &host_key, "cpu0").unwrap();
        let d = perf_data(&mut harness, &host_key).unwrap();
        note(
            &mut log,
            format!(
                "host cpu0 newest point moved {t0} -> {t1} (+{} s); last values {:?}",
                t1 - t0,
                tail(&d, "cpu0", 4)
            ),
        );
        note(
            &mut log,
            format!(
                "host used memory (last) {:?}",
                xen_core::performance::plot_points(
                    &d.archives,
                    &d.sources,
                    "memory_free_kib",
                    t1 - 30,
                    t1
                )
                .last()
                .map(|p| crate::perf_view::value_text(xen_core::performance::Unit::Bytes, p.1))
            ),
        );
        assert!(t1 > t0, "the host graphs did not move");
        for title in [p.graph_cpu, p.graph_memory, p.graph_network] {
            harness.get_by_label(title);
        }

        // 2. Fluxer: its archive while halted, then the boot.
        let vm_key: PerfKey = (conn.id, PerfTarget::Vm(vm.clone()));
        select(&mut harness, Selection::Vm(conn.id, vm.clone()));
        performance_tab(&mut harness);
        step_for(&mut harness, Duration::from_secs(60), |h| {
            h.state().performance.is_open(&vm_key)
        });
        step_for(&mut harness, Duration::from_secs(60), |h| {
            h.query_by_label(perf_strings().graph_loading).is_none()
        });
        step_for(&mut harness, Duration::from_secs(5), |_| false);
        let d = perf_data(&mut harness, &vm_key).unwrap();
        note(
            &mut log,
            format!(
                "Fluxer halted: status {:?}, archive points (5 s / 1 h / 1 day) {} / {} / {}",
                d.status,
                d.archives.series(Interval::FiveSeconds, "cpu0").len(),
                d.archives.series(Interval::OneHour, "cpu0").len(),
                d.archives.series(Interval::OneDay, "cpu0").len()
            ),
        );
        assert!(
            step_for(&mut harness, Duration::from_secs(30), |h| h
                .query_all_by_label(s.action_start)
                .next()
                .is_some_and(|n| !n.accesskit_node().is_disabled())),
            "Start not offered"
        );
        settle(&mut harness);
        let started = Instant::now();
        harness
            .get_all_by_label(s.action_start)
            .next()
            .unwrap()
            .click();
        assert!(
            step_for(&mut harness, Duration::from_secs(120), |h| power(h)
                == VmPowerState::Running),
            "Fluxer did not start"
        );
        note(
            &mut log,
            format!(
                "Start from the toolbar: running after {:.1} s",
                started.elapsed().as_secs_f64()
            ),
        );
        // The window switches to the Console tab on start; back to the graphs.
        step_for(&mut harness, Duration::from_secs(10), |h| {
            h.state().tab == Tab::Console
        });
        performance_tab(&mut harness);
        let moving = step_for(&mut harness, Duration::from_secs(90), |h| {
            let st = h.state();
            st.performance.is_open(&vm_key)
        });
        let mut samples = Vec::new();
        let boot_watch = Instant::now();
        while boot_watch.elapsed() < Duration::from_secs(75) {
            step_for(&mut harness, Duration::from_secs(15), |_| false);
            if let Some(d) = perf_data(&mut harness, &vm_key) {
                let last = last5(&mut harness, &vm_key, "cpu0");
                note(
                    &mut log,
                    format!(
                        "  +{:>3.0} s: status {:?}, updates {}, newest 5 s point {:?}, cpu0 {:?}, memory {:?}",
                        boot_watch.elapsed().as_secs_f64(),
                        d.status,
                        d.updates,
                        last,
                        tail(&d, "cpu0", 3),
                        tail(&d, "memory", 1)
                    ),
                );
                samples.push((last, d.updates));
            }
        }
        let first = samples.first().and_then(|s| s.0);
        let last = samples.last().and_then(|s| s.0);
        note(
            &mut log,
            format!("Fluxer graphs during the boot: newest point {first:?} -> {last:?}"),
        );
        assert!(
            moving && first.is_some() && last > first,
            "Fluxer's graphs did not move during its boot"
        );
        for title in [p.graph_cpu, p.graph_memory, p.graph_network, p.graph_disk] {
            harness.get_by_label(title);
        }

        // 3. A test alert on Fluxer: created, shown, dismissed.
        let session = login().expect("login for message.create");
        let made = runtime.block_on(xapi::message::create(
            &session,
            "XENADMIN_RS_F6_TEST",
            3,
            &xapi::Cls::Vm,
            &fluxer.uuid,
            "xenadmin-rs F6 test alert; created and dismissed by the test",
        ));
        let _ = runtime.block_on(session.logout());
        let made = made.expect("message.create");
        created = Some(made.clone());
        note(
            &mut log,
            "test message created on Fluxer (message.create)".into(),
        );
        let shown = step_for(&mut harness, Duration::from_secs(60), |h| {
            first_conn(h).is_some_and(|c| c.model.messages.contains_key(&made))
        });
        assert!(shown, "the test message did not arrive through event.from");
        harness.state_mut().notifications = Notifications::Alerts;
        settle(&mut harness);
        let title = format!("{FLUXER}: XENADMIN_RS_F6_TEST");
        click_visible(&mut harness, &title);
        let selected = harness.state().alerts.selected();
        assert_eq!(
            selected,
            [(conn.id, made.clone())],
            "refusing: something else is selected"
        );
        note(
            &mut log,
            format!("Alerts view shows {title:?}; selected it alone"),
        );
        click_first(&mut harness, p.alerts_dismiss_selected);
        settle(&mut harness);
        assert_eq!(harness.state().alerts.asking(), Some(false));
        click_first(&mut harness, p.alerts_confirm_yes);
        let gone = step_for(&mut harness, Duration::from_secs(60), |h| {
            first_conn(h).is_some_and(|c| !c.model.messages.contains_key(&made))
        });
        note(&mut log, format!("dismissed through the window: {gone}"));
        assert!(gone, "the test alert was not dismissed");
        created = None;
        let after: HashSet<Ref<Message>> = first_conn(&harness)
            .unwrap()
            .model
            .messages
            .keys()
            .cloned()
            .collect();
        let missing: Vec<&Ref<Message>> = messages_before.difference(&after).collect();
        note(
            &mut log,
            format!(
                "pre-existing messages still present: {} of {} (missing {})",
                messages_before.len() - missing.len(),
                messages_before.len(),
                missing.len()
            ),
        );
        let ours = harness
            .state()
            .backend
            .state()
            .operations
            .iter()
            .filter(|op| matches!(op.kind, xen_core::OperationKind::DismissAlerts { .. }))
            .map(|op| format!("{:?}", op.kind))
            .collect::<Vec<_>>();
        note(
            &mut log,
            format!("dismiss operations run by this window: {ours:?}"),
        );

        // 4. Fluxer's alert settings: CPU on, then off.
        harness.state_mut().notifications = Notifications::Events;
        select(&mut harness, Selection::Vm(conn.id, vm.clone()));
        performance_tab(&mut harness);
        click_first(&mut harness, p.alert_settings);
        settle(&mut harness);
        harness.get_by_label(p.pm_cpu_check).click();
        settle(&mut harness);
        click_first(&mut harness, p.pm_ok);
        let saved = step_for(&mut harness, Duration::from_secs(60), |h| {
            perfmon_now(h).is_some_and(|x| x.contains("cpu_usage"))
        });
        note(
            &mut log,
            format!("CPU alerts on: saved {saved}: {:?}", perfmon_now(&harness)),
        );
        assert!(saved, "the alert settings were not saved");
        click_first(&mut harness, p.alert_settings);
        settle(&mut harness);
        harness.get_by_label(p.pm_cpu_check).click();
        settle(&mut harness);
        click_first(&mut harness, p.pm_ok);
        let restored = step_for(&mut harness, Duration::from_secs(60), |h| {
            perfmon_now(h) == original_perfmon
        });
        note(
            &mut log,
            format!(
                "CPU alerts off: perfmon back to {:?}: {restored}",
                perfmon_now(&harness)
            ),
        );

        // 5. Shut down from the toolbar.
        let offered = step_for(&mut harness, Duration::from_secs(60), |h| {
            h.query_all_by_label(s.action_clean_shutdown)
                .next()
                .is_some_and(|n| !n.accesskit_node().is_disabled())
        });
        assert!(offered, "Shut Down not offered");
        settle(&mut harness);
        harness
            .get_all_by_label(s.action_clean_shutdown)
            .next()
            .unwrap()
            .click();
        settle(&mut harness);
        click_first(&mut harness, o.yes);
        let halted = step_for(&mut harness, Duration::from_secs(180), |h| {
            power(h) == VmPowerState::Halted
        });
        note(
            &mut log,
            format!("Shut Down from the toolbar: halted {halted}"),
        );
        let d = perf_data(&mut harness, &vm_key);
        note(
            &mut log,
            format!(
                "Fluxer graphs after shutdown: status {:?}",
                d.map(|d| d.status)
            ),
        );
    }));

    // Cleanup, whatever happened: our message, the perfmon key, the VM.
    if let Some(made) = created.take()
        && let Ok(session) = login()
    {
        let r = runtime.block_on(xapi::message::destroy(&session, &made));
        note(
            &mut log,
            format!("cleanup: destroyed our test message: {:?}", r.is_ok()),
        );
        let _ = runtime.block_on(session.logout());
    }
    if perfmon_now(&harness) != original_perfmon
        && let Ok(session) = login()
    {
        let restore = runtime.block_on(async {
            xapi::vm::remove_from_other_config(&session, &vm, perfmon::PERFMON_KEY).await?;
            if let Some(v) = &original_perfmon {
                xapi::vm::add_to_other_config(&session, &vm, perfmon::PERFMON_KEY, v).await?;
            }
            Ok::<(), xapi::Error>(())
        });
        note(
            &mut log,
            format!("cleanup: perfmon restored: {:?}", restore.is_ok()),
        );
        let _ = runtime.block_on(session.logout());
    }
    if power(&harness) != VmPowerState::Halted {
        note(&mut log, "cleanup: forcing Fluxer off".into());
        harness.state().backend.send(xen_core::Command::Vm {
            connection: conn.id,
            vm: vm.clone(),
            action: xen_core::VmAction::HardShutdown,
        });
        step_for(&mut harness, Duration::from_secs(120), |h| {
            power(h) == VmPowerState::Halted
        });
    }
    note(
        &mut log,
        format!(
            "end state: power {:?}, perfmon {:?} (was {original_perfmon:?})",
            power(&harness),
            perfmon_now(&harness)
        ),
    );
    harness
        .state()
        .backend
        .shutdown_blocking(Duration::from_secs(5));
    note(&mut log, "logged out".into());
    let log = log.replace(&host_addr, "<host>");
    std::fs::write(&log_path, &log).unwrap();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
