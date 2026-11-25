//! F10a on a real host through the real window code (an ignored test, run
//! with the `XENADMIN_REAL_*` variables of the other real-host tests and
//! `XENADMIN_REAL_LOG` outside the repository), rehearsed on the mock.
//!
//! **Strictly read only.** Every F10a operation changes a pool or a
//! server, so on the real host the test only opens dialogs and cancels
//! them: the pool and server Properties dialogs (each page's values are
//! compared with the cache's records; NRPE, the cluster's network and the
//! power-on passwords are read as the dialogs do), the New Pool dialog
//! (the join reasons it lists), Change Server Password, Control Domain
//! Memory (its value against the control domain's), Install Certificate,
//! Back Up and Restore From Backup. OK, Create Pool, Install, Back Up and
//! Restore are never pressed, and no menu entry that runs an operation is
//! clicked. The rehearsal checks that the only calls made are reads.

use egui_kittest::Harness;
use xen_core::properties::{self, Page, PropertiesTarget};

use super::provisioning::{press, shows};
use super::*;
use crate::pool_strings::pool_strings;
use crate::props_ui::NrpeState;
use crate::provision_ui::DialogRequest;

fn note(log: &mut String, line: String) {
    println!("{line}");
    log.push_str(&line);
    log.push('\n');
}

#[test]
#[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables); opens and cancels dialogs only"]
fn real_f10a_properties_read_only() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let runtime = runtime();
    let mut h = real_host_harness(&runtime);
    let log = f10a_run(&mut h);
    std::fs::write(&log_path, log).expect("write the log");
}

/// The same run against the mock (the sanitized capture of the real host).
#[test]
fn f10a_real_run_on_the_mock() {
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
    let pool_before = server.record("pool", "OpaqueRef:pool-0001");
    let host_before = server.record("host", "OpaqueRef:host-0001");
    let log = f10a_run(&mut h);
    assert!(log.contains("all dialogs cancelled"), "{log}");
    // Only reads: getters, events, the session, and the plugin and secret
    // reads the dialogs make.
    for call in server.calls() {
        let c = call.trim_start_matches("Async.");
        let read = c.contains(".get_")
            || c.starts_with("event.")
            || c.starts_with("session.")
            || c == "host.call_plugin"
            || c.starts_with("http/")
            || c == "task.destroy";
        assert!(read, "a call that is not a read: {call}");
    }
    assert!(
        server
            .plugin_calls()
            .iter()
            .all(|(p, f)| p == "nrpe"
                && matches!(f.as_str(), "status" | "get-config" | "get-threshold")),
        "{:?}",
        server.plugin_calls()
    );
    assert_eq!(server.record("pool", "OpaqueRef:pool-0001"), pool_before);
    assert_eq!(server.record("host", "OpaqueRef:host-0001"), host_before);
}

fn open(h: &mut Harness<'static, XenAdminApp>, conn: ConnectionId, request: DialogRequest) {
    h.state_mut().ops.dialog_requests.push((conn, request));
    settle(h);
    settle(h);
}

fn wait_nrpe(h: &mut Harness<'static, XenAdminApp>) -> String {
    step_until(h, "NRPE read", |h| {
        h.state()
            .pool
            .properties
            .as_ref()
            .is_some_and(|d| !matches!(d.nrpe_state, NrpeState::Retrieving(_)))
    });
    match &h.state().pool.properties.as_ref().unwrap().nrpe_state {
        NrpeState::Loaded(c) => format!(
            "NRPE {} (monitoring servers {:?}, debug {}, SSL logging {}, {} thresholds)",
            if c.enabled { "enabled" } else { "disabled" },
            c.allow_hosts,
            c.debug,
            c.ssl_logging,
            c.thresholds.len()
        ),
        NrpeState::Failed => "NRPE could not be read".to_owned(),
        NrpeState::Retrieving(_) => unreachable!(),
    }
}

fn f10a_run(h: &mut Harness<'static, XenAdminApp>) -> String {
    let mut log = String::new();
    let p = pool_strings();
    let state = h.state().backend.state();
    let conn = state.connections.first().expect("connected").clone();
    let c = conn.id;
    let m = conn.model.clone();
    let (pool_ref, pool) = m.pool().map(|(r, p)| (r.clone(), p.clone())).unwrap();
    let coordinator = pool.master.clone();
    let host = m.hosts.get(&coordinator).unwrap().clone();
    note(
        &mut log,
        format!(
            "pool {:?} ({} host(s)), coordinator {:?}, platform {:?}, xapi {:?}",
            pool.name_label,
            m.hosts.len(),
            host.name_label,
            xen_core::pool::platform_version(&host),
            xen_core::pool::xapi_version(&host)
        ),
    );

    // Pool Properties: the pages and their values, then Cancel.
    open(
        h,
        c,
        DialogRequest::Properties(PropertiesTarget::Pool(pool_ref.clone())),
    );
    let nrpe = wait_nrpe(h);
    {
        let d = h.state_mut().pool.props().expect("pool properties open");
        note(&mut log, format!("pool pages: {:?}", d.pages));
        assert_eq!(d.name, pool.name_label);
        assert_eq!(d.description, pool.name_description);
        assert_eq!(properties::parse_tags(&d.tags), pool.tags);
        assert_eq!(d.email, properties::EmailForm::of(&pool));
        assert_eq!(d.igmp, pool.igmp_snooping_enabled);
        assert_eq!(d.compression, pool.migration_compression);
        assert_eq!(d.live_patching_disabled, pool.live_patching_disabled);
        assert_eq!(d.clustering, !m.clusters.is_empty());
        assert!(d.changes(&conn).is_empty(), "nothing changed by opening");
        note(
            &mut log,
            format!(
                "pool: name {:?}, email notifications {}, IGMP snooping {}, migration compression {}, live patching disabled {}, clustering {}",
                d.name,
                d.email.enabled,
                d.igmp,
                d.compression,
                d.live_patching_disabled,
                d.clustering
            ),
        );
        d.page = Page::PowerOn;
    }
    note(&mut log, format!("pool {nrpe}"));
    settle(h);
    step_until(h, "power on read", |h| {
        h.state()
            .pool
            .properties
            .as_ref()
            .is_some_and(|d| d.power.as_ref().is_some_and(|p| p.pending.is_none()))
    });
    {
        let d = h.state_mut().pool.props().unwrap();
        for (r, name, read, edited) in &d.power.as_ref().unwrap().hosts {
            let rec = m.hosts.get(r).unwrap();
            assert_eq!(
                read.mode_name(),
                if rec.power_on_mode == "iLO" {
                    ""
                } else {
                    rec.power_on_mode.as_str()
                }
            );
            assert_eq!(read, edited);
            note(
                &mut log,
                format!(
                    "power on of {name:?}: {:?}",
                    crate::props_ui::mode_label(read)
                ),
            );
        }
        assert!(d.changes(&conn).is_empty());
    }
    for page in h.state_mut().pool.props().unwrap().pages.clone() {
        h.state_mut().pool.props().unwrap().page = page;
        settle(h);
    }
    press(h, p.cancel);
    assert!(h.state().pool.properties.is_none());

    // Server Properties.
    open(
        h,
        c,
        DialogRequest::Properties(PropertiesTarget::Host(coordinator.clone())),
    );
    let host_pages = h.state_mut().pool.props().unwrap().pages.clone();
    let nrpe = if host_pages.contains(&Page::Nrpe) {
        wait_nrpe(h)
    } else {
        "NRPE: on the pool's page".to_owned()
    };
    {
        let d = h.state_mut().pool.props().unwrap();
        note(&mut log, format!("server pages: {:?}", d.pages));
        assert_eq!(d.name, host.name_label);
        assert_eq!(d.description, host.name_description);
        assert_eq!(d.iqn, properties::host_iqn(&host));
        assert_eq!(d.multipath, properties::multipath_enabled(&host));
        assert_eq!(
            d.log_remote.then(|| d.log_server.clone()),
            properties::syslog_destination(&host).map(str::to_owned)
        );
        assert_eq!(d.autostart, properties::autostart_enabled(&pool));
        assert!(d.changes(&conn).is_empty(), "nothing changed by opening");
        note(
            &mut log,
            format!(
                "server: name {:?}, IQN set {}, multipathing {}, remote log {}, VM autostart {}, alerts {:?}",
                d.name,
                !d.iqn.is_empty(),
                d.multipath,
                d.log_remote,
                d.autostart,
                d.subtext(Page::Alerts)
            ),
        );
    }
    note(&mut log, format!("server {nrpe}"));
    for page in host_pages {
        h.state_mut().pool.props().unwrap().page = page;
        settle(h);
    }
    press(h, p.cancel);
    assert!(h.state().pool.properties.is_none());

    // New Pool: the reasons listed, then Cancel.
    open(h, c, DialogRequest::NewPool { servers: vec![c] });
    {
        let state = h.state().backend.state();
        let d = h.state_mut().pool.new_pool.as_ref().expect("New Pool open");
        for (v, reason) in d.rows(&state) {
            note(
                &mut log,
                format!(
                    "New Pool lists {:?}: {:?}",
                    crate::pool_ui::connection_name(v),
                    crate::pool_ui::reason_text(reason)
                ),
            );
        }
    }
    press(h, p.cancel);
    assert!(h.state().pool.new_pool.is_none());

    // The server dialogs: opened and cancelled.
    open(h, c, DialogRequest::ChangePassword);
    assert!(h.state().pool.password.is_some());
    press(h, p.cancel);
    open(h, c, DialogRequest::Dom0Memory(coordinator.clone()));
    if let Some(d) = h.state().pool.dom0.as_ref() {
        let dom0 = xen_core::objects::control_domain(&m, &host).unwrap();
        assert_eq!(d.memory, dom0.memory_dynamic_min / (1024 * 1024));
        note(
            &mut log,
            format!(
                "control domain memory shown: {} MB (editable only in maintenance mode: {})",
                d.memory,
                shows(h, p.dom0_maintenance)
            ),
        );
    }
    press(h, p.cancel);
    open(h, c, DialogRequest::InstallCertificate(coordinator.clone()));
    assert!(h.state().pool.install.is_some());
    press(h, p.cancel);
    open(h, c, DialogRequest::Backup(coordinator.clone()));
    assert!(h.state().pool.transfer.is_some());
    press(h, p.cancel);
    open(h, c, DialogRequest::Restore(coordinator.clone()));
    assert!(h.state().pool.transfer.is_some());
    press(h, p.cancel);
    assert!(
        h.state().pool.password.is_none()
            && h.state().pool.dom0.is_none()
            && h.state().pool.install.is_none()
            && h.state().pool.transfer.is_none()
    );
    // No operation ran.
    let ops: Vec<String> = h
        .state()
        .backend
        .state()
        .operations
        .iter()
        .filter(|o| {
            !matches!(
                o.kind,
                xen_core::OperationKind::Connect | xen_core::OperationKind::Reconnect
            )
        })
        .map(|o| i18n::operation_title(&o.kind, &o.server))
        .collect();
    assert!(ops.is_empty(), "operations ran: {ops:?}");
    note(
        &mut log,
        "all dialogs cancelled; no operation ran".to_owned(),
    );
    log
}
