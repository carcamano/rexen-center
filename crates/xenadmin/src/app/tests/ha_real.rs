//! F10b on a real host through the real window code (an ignored test, run
//! with the `XENADMIN_REAL_*` variables of the other real-host tests and
//! `XENADMIN_REAL_LOG` outside the repository), rehearsed on the mock.
//!
//! **Strictly read only.** Turning HA on or off, its settings, joining or
//! leaving a domain, subjects, roles and sessions all change the pool, so
//! on the real host the test only looks and cancels: the pool's HA tab and
//! its Pool menu entries, the Configure HA wizard (its heartbeat SR scan,
//! `SR.assert_can_host_ha_statefile`; the VMs' agility, `VM.assert_agile`;
//! the planner, `pool.ha_compute_hypothetical_max_host_failures_to_tolerate`
//! — checks that change nothing) cancelled before Finish, and the Users tab
//! (who is logged in, `session.get_all_subject_identifiers`) with its Join
//! Domain prompt cancelled. The rehearsal checks that the only calls made
//! are reads and these checks, and that the pool, host and subject records
//! are unchanged.

use egui_kittest::Harness;

use super::provisioning::{press, select, shows, tab};
use super::*;
use crate::ha_strings::ha_strings;
use crate::provision_ui::DialogRequest;

fn note(log: &mut String, line: String) {
    println!("{line}");
    log.push_str(&line);
    log.push('\n');
}

#[test]
#[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables); opens and cancels dialogs only"]
fn real_f10b_ha_and_users_read_only() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let runtime = runtime();
    let mut h = real_host_harness(&runtime);
    let log = f10b_run(&mut h);
    std::fs::write(&log_path, log).expect("write the log");
}

/// The same run against the mock (the sanitized capture of the real host).
#[test]
fn f10b_real_run_on_the_mock() {
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
    let records = |s: &MockServer| {
        ["pool", "host", "subject", "role"]
            .iter()
            .map(|c| {
                (
                    c,
                    s.refs(c).iter().map(|r| s.record(c, r)).collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    let before = records(&server);
    let log = f10b_run(&mut h);
    assert!(log.contains("all dialogs cancelled"), "{log}");
    assert!(log.contains("planner"), "{log}");
    // Only reads and the HA checks.
    for call in server.calls() {
        let c = call.trim_start_matches("Async.");
        let read = c.contains(".get_")
            || c.starts_with("event.")
            || c.starts_with("session.")
            || matches!(
                c,
                "SR.assert_can_host_ha_statefile"
                    | "VM.assert_agile"
                    | "pool.ha_compute_hypothetical_max_host_failures_to_tolerate"
            );
        assert!(read, "a call that is not a read: {call}");
        assert!(!call.starts_with("Async."), "a task: {call}");
    }
    assert_eq!(records(&server), before);
}

fn f10b_run(h: &mut Harness<'static, XenAdminApp>) -> String {
    let mut log = String::new();
    let hs = ha_strings();
    let n = crate::net_strings::net_strings();
    let state = h.state().backend.state();
    let conn = state.connections.first().expect("connected").clone();
    let c = conn.id;
    let m = conn.model.clone();
    let (pool_ref, pool) = m.pool().map(|(r, p)| (r.clone(), p.clone())).unwrap();
    let name = crate::pool_ui::connection_name(&conn);
    let coordinator = m.coordinator().unwrap().clone();
    note(
        &mut log,
        format!(
            "pool {:?} ({} host(s)): HA {}, ntol {}, plan {}, statefiles {}; AD {:?} {:?}; {} subject(s); session superuser {}",
            pool.name_label,
            m.hosts.len(),
            if pool.ha_enabled { "on" } else { "off" },
            pool.ha_host_failures_to_tolerate,
            pool.ha_plan_exists_for,
            pool.ha_statefiles.len(),
            coordinator.external_auth_type,
            coordinator.external_auth_service_name,
            m.subjects.len(),
            conn.rbac.as_ref().is_some_and(|r| r.is_local_superuser),
        ),
    );

    // The HA tab, as the cache says.
    select(h, Selection::Pool(c, pool_ref.clone()));
    tab(h, hs.tab_ha);
    let blurb = fill(
        if pool.ha_enabled {
            hs.ha_tab_configured_blurb
        } else {
            hs.ha_panel_blurb
        },
        &[("pool", &name)],
    );
    assert!(shows(h, &blurb), "HA tab blurb");
    assert_eq!(shows(h, hs.disable_ha), pool.ha_enabled);
    assert_eq!(shows(h, hs.heartbeating_status), pool.ha_enabled);
    note(&mut log, format!("HA tab: {blurb:?}"));
    if pool.ha_enabled {
        for row in xen_core::ha::heartbeat_rows(&m, &pool, true) {
            note(
                &mut log,
                format!("  heartbeat {:?}: {:?}", row.name, row.network),
            );
        }
    }

    // The Pool menu's High Availability entries.
    let entries = crate::ha_ui::pool_entries(&state, &conn);
    if let Some(crate::ops_ui::Entry::Submenu {
        enabled, entries, ..
    }) = entries.first()
    {
        let items: Vec<String> = entries
            .iter()
            .filter_map(|e| match e {
                crate::ops_ui::Entry::Named {
                    label,
                    enabled,
                    hint,
                    ..
                } => Some(format!(
                    "{label} {} {:?}",
                    if *enabled { "on" } else { "off" },
                    hint
                )),
                _ => None,
            })
            .collect();
        note(
            &mut log,
            format!(
                "Pool menu High Availability ({}): {items:?}",
                if *enabled { "on" } else { "off" }
            ),
        );
    }

    // Configure HA, opened and cancelled.
    if pool.ha_enabled {
        note(
            &mut log,
            "HA is on: the settings dialog is not opened (it would only read)".into(),
        );
    } else {
        h.state_mut()
            .ops
            .dialog_requests
            .push((c, DialogRequest::ConfigureHa));
        settle(h);
        settle(h);
        let broken = xen_core::ha::broken_shared_srs(&m);
        if !broken.is_empty() {
            note(&mut log, format!("broken SR warning: {broken:?}"));
            assert!(shows(
                h,
                &fill(hs.srs_broken_warning, &[("srs", &broken.join("\n"))])
            ));
            press(h, hs.ok);
        }
        assert!(shows(h, hs.intro_heading));
        press(h, n.next);
        step_until(h, "SR scan", |h| h.state().ha.wizard_srs().is_some());
        settle(h);
        let srs = h.state().ha.wizard_srs().unwrap();
        for (name, usable, comment) in &srs {
            note(
                &mut log,
                format!(
                    "  heartbeat SR {name:?}: {}{}",
                    if *usable { "usable" } else { "not usable" },
                    if comment.is_empty() {
                        String::new()
                    } else {
                        format!(" ({comment})")
                    }
                ),
            );
        }
        if let Some((sr, _, _)) = srs.iter().find(|s| s.1) {
            press(h, sr);
            press(h, n.next);
            step_until(h, "agility and planner", |h| {
                h.state()
                    .ha
                    .wizard_plan()
                    .is_some_and(|p| p.calculating == 0 && p.max.is_some())
            });
            let plan = h.state().ha.wizard_plan().unwrap();
            note(
                &mut log,
                format!(
                    "  priorities page: {} VM(s), {} agile, {} not agile; planner {:?}, failures tolerated {}",
                    plan.vms, plan.agile, plan.not_agile, plan.max, plan.ntol
                ),
            );
        } else {
            note(
                &mut log,
                "  no usable heartbeat SR: the wizard stops here".into(),
            );
        }
        press(h, n.cancel);
        assert!(!h.state().ha.any_open(), "wizard cancelled");
    }

    // The Users tab, as the cache says.
    tab(h, hs.tab_users);
    match xen_core::ad::state(&m, false) {
        xen_core::ad::AdState::Disabled => {
            assert!(shows(
                h,
                &fill(hs.ad_not_configured_blurb, &[("pool", &name)])
            ));
            assert!(shows(h, hs.ad_disabled));
            note(
                &mut log,
                "Users tab: AD not configured; the grid is disabled".into(),
            );
        }
        xen_core::ad::AdState::Enabled { domain } => {
            assert!(shows(
                h,
                &fill(
                    hs.ad_configured_blurb,
                    &[("pool", &name), ("domain", &domain)]
                )
            ));
            assert!(shows(h, hs.local_root));
            note(
                &mut log,
                format!(
                    "Users tab: domain {domain:?}, {} subject(s)",
                    m.subjects.len()
                ),
            );
        }
        other => note(&mut log, format!("Users tab: {other:?}")),
    }
    // Join / Leave Domain opened and cancelled.
    let joined = matches!(
        xen_core::ad::state(&m, false),
        xen_core::ad::AdState::Enabled { .. }
    );
    press(
        h,
        if joined {
            hs.leave_domain
        } else {
            hs.join_domain
        },
    );
    if joined {
        press(h, crate::ops_strings::ops_strings().no);
    } else {
        assert!(shows(h, hs.join_blurb_short));
        press(h, hs.cancel);
    }
    assert!(!h.state().ha.ad.dialog_open(), "prompt cancelled");
    note(
        &mut log,
        format!(
            "{} prompt opened and cancelled",
            if joined {
                "Leave Domain"
            } else {
                "Join Domain"
            }
        ),
    );

    // Nothing ran.
    let state = h.state().backend.state();
    let ran: Vec<String> = state
        .operations
        .iter()
        .filter(|o| !matches!(o.kind, xen_core::OperationKind::Connect))
        .map(|o| i18n::operation_title(&o.kind, &o.server))
        .collect();
    assert!(ran.is_empty(), "operations ran: {ran:?}");
    note(
        &mut log,
        "all dialogs cancelled; no operation in the history".into(),
    );
    log
}
