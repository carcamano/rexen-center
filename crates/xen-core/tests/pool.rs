//! F10a: pools and server settings against the mock (backend only, no
//! UI). Two standalone lab servers (two mock servers) are made into a pool
//! by New Pool, a server is removed and comes back standalone, Add Server
//! joins it again, the coordinator hands the pool over; failed joins
//! change nothing (and a New Pool nobody could join is undone); the pool
//! and server Properties pages, Control Domain Memory, Change Server
//! Password, the pool secret, certificate verification, certificates and
//! backup / restore.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::{memory_store, spawn};
use serde_json::{Value, json};
use xapi::Ref;
use xapi_mock::MockServer;
use xen_core::pool::{self, JoinReason, RemoveProblem};
use xen_core::pool_run::errors;
use xen_core::properties::{
    self, ClusteringChange, EmailSettings, NrpeConfig, Page, PowerOnMode, PropertiesTarget,
    PropertyChanges,
};
use xen_core::{
    ActionRequest, AppState, BackendOptions, Command, ConnectionId, ConnectionView, Operation,
    OperationStatus, Password,
};

const HOST_A: &str = "OpaqueRef:host-0001-a";
const HOST_B: &str = "OpaqueRef:host-0001-b";
const POOL_A: &str = "OpaqueRef:pool-0001-a";
const POOL_B: &str = "OpaqueRef:pool-0001-b";
const LOCAL_A: &str = "OpaqueRef:sr-local-01-a";
const DOM0_A: &str = "OpaqueRef:vm-dom0-01-a";
const NET_ETH1_A: &str = "OpaqueRef:net-eth1-a";

fn r<T>(s: &str) -> Ref<T> {
    Ref::new(s)
}

fn harness() -> common::Harness {
    spawn(BackendOptions {
        join_wait: Duration::from_secs(5),
        eject_reconnect_delay: Duration::from_millis(50),
        eject_reconnect_poll: Duration::from_millis(50),
        eject_reconnect_attempts: 20,
        certificate_wait: Duration::from_secs(5),
        ..common::fast_options(memory_store().0)
    })
}

/// Connects to `server` and returns the connection once loaded.
async fn connect(h: &common::Harness, server: &MockServer) -> ConnectionId {
    h.connect(server, xapi_mock::PASSWORD);
    let address = server.address();
    let state = h
        .wait_for("connected", |s| {
            s.connections
                .iter()
                .any(|c| c.server == address && c.status.is_connected() && c.rbac.is_some())
        })
        .await;
    state
        .connections
        .iter()
        .find(|c| c.server == address)
        .unwrap()
        .id
}

fn view(state: &AppState, id: ConnectionId) -> &ConnectionView {
    state.connection(id).expect("connection")
}

/// Waits for the history entry after `before` that ends.
async fn ended_after(h: &common::Harness, before: u64) -> Operation {
    let state = h
        .wait_for("operation ended", |s| {
            s.operations
                .iter()
                .any(|o| o.id > before && !o.is_running())
        })
        .await;
    (**state
        .operations
        .iter()
        .find(|o| o.id > before && !o.is_running())
        .unwrap())
    .clone()
}

fn last_id(h: &common::Harness) -> u64 {
    h.backend
        .state()
        .operations
        .iter()
        .map(|o| o.id)
        .max()
        .unwrap_or(0)
}

async fn run(h: &common::Harness, conn: ConnectionId, request: ActionRequest) -> Operation {
    let before = last_id(h);
    h.backend.send(Command::Action {
        connection: conn,
        request,
    });
    ended_after(h, before).await
}

fn ok(op: &Operation) {
    assert!(
        matches!(op.status, OperationStatus::Succeeded),
        "{:?}: {:?}",
        op.kind,
        op.status
    );
}

fn failed_with(op: &Operation, code: &str) {
    match &op.status {
        OperationStatus::Failed(xen_core::OperationFailure::Xapi(e)) => {
            assert!(e.is_api(code), "{:?}: expected {code}, got {e}", op.kind)
        }
        other => panic!("{:?}: expected failure {code}, got {other:?}", op.kind),
    }
}

fn field(server: &MockServer, class: &str, r: &str, f: &str) -> Value {
    server.field(class, r, f).unwrap_or(Value::Null)
}

fn temp_path(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("xen-f10a-{}-{n}-{name}", std::process::id()))
}

/// Two standalone lab servers, both connected.
async fn lab(h: &common::Harness) -> (MockServer, MockServer, ConnectionId, ConnectionId) {
    let a = MockServer::start_lab(false, "a", 1).await;
    let b = MockServer::start_lab(false, "b", 2).await;
    a.set_task_timing(2, Duration::from_millis(20));
    b.set_task_timing(2, Duration::from_millis(20));
    let ca = connect(h, &a).await;
    let cb = connect(h, &b).await;
    (a, b, ca, cb)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn join_rules_list_why_a_server_cannot_join() {
    let h = harness();
    let (a, b, ca, cb) = lab(&h).await;
    let state = h.backend.state();
    let (va, vb) = (view(&state, ca), view(&state, cb));
    assert_eq!(
        pool::can_join_pool(Some(vb), Some(va), true, true, 1),
        JoinReason::Allowed
    );
    assert_eq!(
        pool::can_join_pool(Some(va), Some(va), true, true, 1),
        JoinReason::WillBeCoordinator
    );
    assert_eq!(
        pool::can_join_pool(None, Some(va), true, true, 1),
        JoinReason::NotConnected
    );
    assert_eq!(
        pool::can_join_pool(Some(vb), None, true, true, 1),
        JoinReason::CoordinatorNotConnected
    );
    // A named pool is a pool: it cannot join another.
    b.set_name_label("pool", POOL_B, "Other pool");
    // HA on the coordinator, certificate checking on one side only.
    a.set_field("pool", POOL_A, "ha_enabled", json!(true));
    let state = h
        .wait_for("changes seen", |s| {
            s.connection(cb)
                .is_some_and(|c| pool::visible_pool(&c.model).is_some())
                && s.connection(ca)
                    .is_some_and(|c| c.model.pool().is_some_and(|(_, p)| p.ha_enabled))
        })
        .await;
    assert_eq!(
        pool::can_join_pool(
            Some(view(&state, cb)),
            Some(view(&state, ca)),
            true,
            true,
            1
        ),
        JoinReason::IsAPool
    );
    b.set_name_label("pool", POOL_B, "");
    let state = h
        .wait_for("pool of one again", |s| {
            s.connection(cb)
                .is_some_and(|c| pool::visible_pool(&c.model).is_none())
        })
        .await;
    assert_eq!(
        pool::can_join_pool(
            Some(view(&state, cb)),
            Some(view(&state, ca)),
            true,
            true,
            1
        ),
        JoinReason::CoordinatorHasHa
    );
    a.set_field("pool", POOL_A, "ha_enabled", json!(false));
    b.set_field("pool", POOL_B, "tls_verification_enabled", json!(false));
    let state = h
        .wait_for("tls off on b", |s| {
            s.connection(cb).is_some_and(|c| {
                c.model
                    .pool()
                    .is_some_and(|(_, p)| !p.tls_verification_enabled)
            }) && s
                .connection(ca)
                .is_some_and(|c| c.model.pool().is_some_and(|(_, p)| !p.ha_enabled))
        })
        .await;
    assert_eq!(
        pool::can_join_pool(
            Some(view(&state, cb)),
            Some(view(&state, ca)),
            true,
            true,
            1
        ),
        JoinReason::TlsVerificationOnlyOnCoordinator
    );
    // A different version, a running VM.
    b.set_field("pool", POOL_B, "tls_verification_enabled", json!(true));
    let mut sv = field(&b, "host", HOST_B, "software_version");
    sv["platform_version"] = json!("3.2.0");
    b.set_field("host", HOST_B, "software_version", sv);
    let state = h
        .wait_for("version changed", |s| {
            s.connection(cb).is_some_and(|c| {
                c.model
                    .coordinator()
                    .is_some_and(|h| pool::platform_version(h) == Some("3.2.0"))
            })
        })
        .await;
    assert_eq!(
        pool::can_join_pool(
            Some(view(&state, cb)),
            Some(view(&state, ca)),
            true,
            true,
            1
        ),
        JoinReason::DifferentServerVersion
    );
    // The coordinator's warnings: none between two equal lab servers.
    assert!(pool::join_warnings(&[view(&state, ca)], view(&state, ca)).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_two_host_pool_is_made_split_and_joined_again() {
    let h = harness();
    let (a, b, ca, cb) = lab(&h).await;

    // New Pool: a coordinates, b joins.
    let before = last_id(&h);
    h.backend.send(Command::NewPool {
        coordinator: ca,
        members: vec![cb],
        name: "F10a pool".into(),
        description: "made by the tests".into(),
        ad: None,
    });
    let op = ended_after(&h, before).await;
    ok(&op);
    assert_eq!(field(&a, "pool", POOL_A, "name_label"), json!("F10a pool"));
    assert_eq!(
        field(&a, "pool", POOL_A, "name_description"),
        json!("made by the tests")
    );
    // Local storage cannot be a pool's default SR (`ClearNonSharedSrs`).
    assert_eq!(
        field(&a, "pool", POOL_A, "default_SR"),
        json!("OpaqueRef:NULL")
    );
    assert!(b.is_pool_member());
    let state = h
        .wait_for("two hosts, b's connection gone", |s| {
            s.connection(ca).is_some_and(|c| c.model.hosts.len() == 2) && s.connection(cb).is_none()
        })
        .await;
    let m = &view(&state, ca).model;
    let joined = m.hosts.get(&r(HOST_B)).expect("b in the pool");
    assert_eq!(joined.name_label, "lab-host-b");
    // b's NICs are on the pool's networks (same bridge).
    let b_eth0 = m
        .pifs
        .values()
        .find(|p| p.host == r(HOST_B) && p.device == "eth0")
        .unwrap();
    assert_eq!(b_eth0.network, r("OpaqueRef:net-eth0-a"));
    assert!(m.srs.contains_key(&r("OpaqueRef:sr-local-01-b")));
    // b is no longer a saved server of its own.
    assert!(state.settings.server(&b.address()).is_none());
    // Remove Server is offered for b only.
    assert_eq!(
        pool::remove_problem(m, &r(HOST_A)),
        Some(RemoveProblem::Coordinator)
    );
    assert_eq!(pool::remove_problem(m, &r(HOST_B)), None);

    // Remove Server: b leaves, comes back standalone and is connected.
    let op = run(&h, ca, ActionRequest::EjectHost { host: r(HOST_B) }).await;
    ok(&op);
    assert!(!b.is_pool_member());
    let state = h
        .wait_for("b reconnected on its own", |s| {
            s.connection(ca).is_some_and(|c| c.model.hosts.len() == 1)
                && s.connections.iter().any(|c| {
                    c.server == b.address()
                        && c.status.is_connected()
                        && c.model.hosts.contains_key(&r(HOST_B))
                })
        })
        .await;
    let cb2 = state
        .connections
        .iter()
        .find(|c| c.server == b.address())
        .unwrap()
        .id;
    assert!(pool::visible_pool(&view(&state, cb2).model).is_none());
    assert!(
        !view(&state, ca)
            .model
            .srs
            .contains_key(&r("OpaqueRef:sr-local-01-b"))
    );

    // Add Server: b joins the named pool again.
    let before = last_id(&h);
    h.backend.send(Command::JoinPool {
        pool: ca,
        hosts: vec![cb2],
        ad: None,
    });
    let op = ended_after(&h, before).await;
    ok(&op);
    h.wait_for("two hosts again", |s| {
        s.connection(ca).is_some_and(|c| c.model.hosts.len() == 2) && s.connection(cb2).is_none()
    })
    .await;

    // The coordinator enters maintenance mode and hands the pool to b.
    let op = run(
        &h,
        ca,
        ActionRequest::EvacuateCoordinator {
            host: r(HOST_A),
            new_coordinator: r(HOST_B),
        },
    )
    .await;
    ok(&op);
    let state = h
        .wait_for("the connection follows the new coordinator", |s| {
            s.connection(ca).is_some_and(|c| {
                c.status.is_connected()
                    && c.server == b.address()
                    && c.model.pool().is_some_and(|(_, p)| p.master == r(HOST_B))
            })
        })
        .await;
    let m = &view(&state, ca).model;
    assert_eq!(m.hosts.len(), 2);
    assert!(xen_core::objects::host_maintenance_flag(
        m.hosts.get(&r(HOST_A)).unwrap()
    ));
    assert!(a.is_pool_member());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_joins_change_nothing_and_a_failed_new_pool_is_undone() {
    let h = harness();
    let (a, b, ca, cb) = lab(&h).await;
    let default_sr = field(&a, "pool", POOL_A, "default_SR");
    assert_eq!(default_sr, json!(LOCAL_A));

    // The joining server refuses: the pool is a standalone server again,
    // with its SRs back.
    b.fail_method(
        "pool.join",
        Some(("JOINING_HOST_CANNOT_HAVE_RUNNING_VMS", &[])),
    );
    let before = last_id(&h);
    h.backend.send(Command::NewPool {
        coordinator: ca,
        members: vec![cb],
        name: "Doomed".into(),
        description: "".into(),
        ad: None,
    });
    let op = ended_after(&h, before).await;
    failed_with(&op, "JOINING_HOST_CANNOT_HAVE_RUNNING_VMS");
    assert_eq!(field(&a, "pool", POOL_A, "name_label"), json!(""));
    assert_eq!(field(&a, "pool", POOL_A, "default_SR"), default_sr);
    assert_eq!(field(&a, "pool", POOL_A, "crash_dump_SR"), default_sr);
    assert!(!b.is_pool_member());
    assert_eq!(a.refs("host").len(), 1);
    // The operation ends once the server is undone; the cache learns of it
    // from the next event batch, which may come a little later.
    let state = h
        .wait_for("the cache shows a standalone server again", |s| {
            s.connection(ca)
                .is_some_and(|c| pool::visible_pool(&c.model).is_none())
        })
        .await;
    assert!(view(&state, cb).status.is_connected());
    b.fail_method("pool.join", None);

    // The coordinator refuses (HA on): Add Server fails, nothing moves.
    a.set_name_label("pool", POOL_A, "Named");
    a.set_field("pool", POOL_A, "ha_enabled", json!(true));
    let before = last_id(&h);
    h.backend.send(Command::JoinPool {
        pool: ca,
        hosts: vec![cb],
        ad: None,
    });
    let op = ended_after(&h, before).await;
    failed_with(&op, "HA_IS_ENABLED");
    assert!(!b.is_pool_member());
    assert_eq!(a.refs("host").len(), 1);
    assert_eq!(b.refs("host").len(), 1);
    a.set_field("pool", POOL_A, "ha_enabled", json!(false));

    // A server with shared storage is refused by the server itself.
    b.add_object(
        "sr",
        "OpaqueRef:sr-shared-b",
        json!({"uuid": "5e5e5e5e-0000-4000-8000-0000000000bb", "name_label": "NFS b",
               "type": "nfs", "content_type": "user", "shared": true, "PBDs": [], "VDIs": [],
               "other_config": {}, "sm_config": {}, "tags": []}),
    );
    let before = last_id(&h);
    h.backend.send(Command::JoinPool {
        pool: ca,
        hosts: vec![cb],
        ad: None,
    });
    let op = ended_after(&h, before).await;
    failed_with(&op, "JOINING_HOST_CANNOT_CONTAIN_SHARED_SRS");
    assert!(!b.is_pool_member());
    assert_eq!(a.refs("host").len(), 1);

    // Make into standalone server.
    let op = run(&h, ca, ActionRequest::DestroyPool { pool: r(POOL_A) }).await;
    ok(&op);
    assert_eq!(field(&a, "pool", POOL_A, "name_label"), json!(""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_and_pool_properties_are_saved() {
    let h = harness();
    let a = MockServer::start_lab(false, "a", 1).await;
    a.set_task_timing(2, Duration::from_millis(20));
    let ca = connect(&h, &a).await;
    let state = h.backend.state();
    let m = &view(&state, ca).model;
    let host_target = PropertiesTarget::Host(r(HOST_A));
    let pool_target = PropertiesTarget::Pool(r(POOL_A));
    let rbac = view(&state, ca).rbac.clone();
    // A standalone server shows the pool-wide pages too.
    assert_eq!(
        properties::pages(m, &host_target, rbac.as_deref()),
        vec![
            Page::General,
            Page::CustomFields,
            Page::Alerts,
            Page::EmailOptions,
            Page::Multipath,
            Page::LogDestination,
            Page::PowerOn,
            Page::Autostart,
            Page::NetworkOptions,
            Page::Clustering,
            Page::Nrpe,
        ]
    );
    assert_eq!(
        properties::pages(m, &pool_target, rbac.as_deref()),
        vec![
            Page::General,
            Page::CustomFields,
            Page::EmailOptions,
            Page::PowerOn,
            Page::NetworkOptions,
            Page::Clustering,
            Page::Advanced,
            Page::Nrpe,
        ]
    );

    // The NRPE page reads the plugin when it opens.
    let id = 77;
    h.backend.send(Command::Precheck {
        connection: ca,
        id,
        check: xen_core::Precheck::Nrpe { host: r(HOST_A) },
    });
    let mut h = h;
    let nrpe = loop {
        if let xen_core::Notification::Precheck {
            id: 77,
            result: xen_core::PrecheckResult::Nrpe(r),
            ..
        } = h.next_notification().await
        {
            break r.expect("nrpe read");
        }
    };
    assert!(!nrpe.enabled);
    assert_eq!(nrpe.thresholds.len(), 11);
    assert_eq!(
        nrpe.thresholds.get("check_load"),
        Some(&("2.7,2.6,2.5".to_owned(), "3.2,3.1,3".to_owned()))
    );

    // Every server page.
    let mut new_nrpe = NrpeConfig {
        enabled: true,
        allow_hosts: "192.0.2.50,monitor.example.com".into(),
        debug: true,
        ..nrpe.clone()
    };
    new_nrpe
        .thresholds
        .insert("check_cpu".into(), ("70".into(), "95".into()));
    assert_eq!(properties::nrpe_problem(&new_nrpe), None);
    let changes = PropertyChanges {
        name: Some("renamed-a".into()),
        description: Some("described".into()),
        tags: Some(vec!["lab".into(), "f10a".into()]),
        iqn: Some("iqn.2026-11.org.example:renamed".into()),
        email: Some(EmailSettings {
            destination: Some("ops@example.com".into()),
            mailhub: Some("smtp.example.com:2525".into()),
            language: Some("en-US".into()),
        }),
        multipath: Some(true),
        syslog: Some(Some("syslog.example.com".into())),
        power_on: vec![(
            r(HOST_A),
            PowerOnMode::Drac {
                ip: "192.0.2.200".into(),
                user: "admin".into(),
                password: "drac-secret".into(),
            },
        )],
        autostart: Some(true),
        igmp_snooping: Some(true),
        nrpe: Some((new_nrpe.clone(), nrpe.clone())),
        ..PropertyChanges::default()
    };
    let op = run(
        &h,
        ca,
        ActionRequest::SetProperties {
            target: host_target.clone(),
            changes: Box::new(changes),
        },
    )
    .await;
    ok(&op);
    assert_eq!(field(&a, "host", HOST_A, "name_label"), json!("renamed-a"));
    assert_eq!(
        field(&a, "host", HOST_A, "name_description"),
        json!("described")
    );
    assert_eq!(field(&a, "host", HOST_A, "tags"), json!(["lab", "f10a"]));
    assert_eq!(
        field(&a, "host", HOST_A, "iscsi_iqn"),
        json!("iqn.2026-11.org.example:renamed")
    );
    assert_eq!(field(&a, "host", HOST_A, "multipathing"), json!(true));
    assert_eq!(
        field(&a, "host", HOST_A, "other_config")["multipathhandle"],
        json!("dmp")
    );
    assert_eq!(
        field(&a, "host", HOST_A, "logging"),
        json!({"syslog_destination": "syslog.example.com"})
    );
    assert!(a.calls().iter().any(|c| c == "host.syslog_reconfigure"));
    // PBDs were unplugged and plugged again around the multipath change.
    let calls = a.calls();
    let unplug = calls
        .iter()
        .position(|c| c == "PBD.unplug")
        .expect("unplug");
    let set = calls
        .iter()
        .position(|c| c == "host.set_multipathing")
        .unwrap();
    let plug = calls.iter().rposition(|c| c == "PBD.plug").expect("plug");
    assert!(unplug < set && set < plug);
    assert_eq!(field(&a, "host", HOST_A, "power_on_mode"), json!("DRAC"));
    let config = field(&a, "host", HOST_A, "power_on_config");
    assert_eq!(config["power_on_ip"], json!("192.0.2.200"));
    let secret_uuid = config["power_on_password_secret"]
        .as_str()
        .unwrap()
        .to_owned();
    let secret = a
        .refs("secret")
        .into_iter()
        .find(|s| field(&a, "secret", s, "uuid") == json!(secret_uuid))
        .expect("the password went to a secret");
    assert_eq!(field(&a, "secret", &secret, "value"), json!("drac-secret"));
    let pool_oc = field(&a, "pool", POOL_A, "other_config");
    assert_eq!(pool_oc["auto_poweron"], json!("true"));
    assert_eq!(pool_oc["mail-destination"], json!("ops@example.com"));
    assert_eq!(pool_oc["ssmtp-mailhub"], json!("smtp.example.com:2525"));
    assert_eq!(
        field(&a, "pool", POOL_A, "igmp_snooping_enabled"),
        json!(true)
    );
    assert!(a.nrpe_enabled(HOST_A));

    // The power-on password is read back for the page.
    h.wait_for("power-on mode seen", |s| {
        s.connection(ca).is_some_and(|c| {
            c.model
                .hosts
                .get(&r(HOST_A))
                .is_some_and(|h| h.power_on_mode == "DRAC")
        })
    })
    .await;
    h.backend.send(Command::Precheck {
        connection: ca,
        id: 78,
        check: xen_core::Precheck::PowerOnSecrets {
            hosts: vec![r(HOST_A)],
        },
    });
    let secrets = loop {
        if let xen_core::Notification::Precheck {
            id: 78,
            result: xen_core::PrecheckResult::PowerOnSecrets(s),
            ..
        } = h.next_notification().await
        {
            break s;
        }
    };
    assert_eq!(secrets, vec![(r(HOST_A), "drac-secret".to_owned())]);
    // NRPE as saved.
    h.backend.send(Command::Precheck {
        connection: ca,
        id: 79,
        check: xen_core::Precheck::Nrpe { host: r(HOST_A) },
    });
    let saved = loop {
        if let xen_core::Notification::Precheck {
            id: 79,
            result: xen_core::PrecheckResult::Nrpe(r),
            ..
        } = h.next_notification().await
        {
            break r.unwrap();
        }
    };
    assert_eq!(saved, new_nrpe);

    // The pool's own pages: name, clustering, migration compression; the
    // email settings removed; the log destination back to local.
    let changes = PropertyChanges {
        name: Some("Lab".into()),
        email: Some(EmailSettings::default()),
        clustering: Some(ClusteringChange::Enable(r(NET_ETH1_A))),
        migration_compression: Some(true),
        power_on: vec![(r(HOST_A), PowerOnMode::WakeOnLan)],
        ..PropertyChanges::default()
    };
    let op = run(
        &h,
        ca,
        ActionRequest::SetProperties {
            target: pool_target.clone(),
            changes: Box::new(changes),
        },
    )
    .await;
    ok(&op);
    assert_eq!(field(&a, "pool", POOL_A, "name_label"), json!("Lab"));
    assert_eq!(
        field(&a, "pool", POOL_A, "migration_compression"),
        json!(true)
    );
    assert!(
        field(&a, "pool", POOL_A, "other_config")
            .get("mail-destination")
            .is_none()
    );
    assert_eq!(
        field(&a, "host", HOST_A, "power_on_mode"),
        json!("wake-on-lan")
    );
    assert_eq!(a.refs("cluster").len(), 1);
    let state = h
        .wait_for("cluster seen", |s| {
            s.connection(ca)
                .is_some_and(|c| !c.model.clusters.is_empty())
        })
        .await;
    let m = &view(&state, ca).model;
    let pif = m
        .pifs
        .values()
        .find(|p| p.network == r(NET_ETH1_A))
        .unwrap();
    assert!(pif.disallow_unplug);
    let changes = PropertyChanges {
        clustering: Some(ClusteringChange::Disable),
        syslog: Some(None),
        ..PropertyChanges::default()
    };
    let op = run(
        &h,
        ca,
        ActionRequest::SetProperties {
            target: host_target.clone(),
            changes: Box::new(changes),
        },
    )
    .await;
    ok(&op);
    assert!(a.refs("cluster").is_empty());
    assert_eq!(field(&a, "host", HOST_A, "logging"), json!({}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn property_failures_are_reported() {
    let h = harness();
    let a = MockServer::start_lab(false, "a", 1).await;
    a.set_task_timing(2, Duration::from_millis(20));
    let ca = connect(&h, &a).await;
    // set_power_on_mode refused: the secret made for it is destroyed.
    a.fail_method(
        "host.set_power_on_mode",
        Some(("INTERNAL_ERROR", &["power"])),
    );
    let changes = PropertyChanges {
        power_on: vec![(
            r(HOST_A),
            PowerOnMode::Custom {
                mode: "my-script".into(),
                config: BTreeMap::from([
                    ("power_on_password_secret".into(), "pw".into()),
                    ("target".into(), "bmc-1".into()),
                ]),
            },
        )],
        ..PropertyChanges::default()
    };
    let op = run(
        &h,
        ca,
        ActionRequest::SetProperties {
            target: PropertiesTarget::Host(r(HOST_A)),
            changes: Box::new(changes),
        },
    )
    .await;
    failed_with(&op, "INTERNAL_ERROR");
    assert!(a.refs("secret").is_empty());
    assert_eq!(field(&a, "host", HOST_A, "power_on_mode"), json!(""));
    // A multipath change whose PBD replug fails still plugs the others and
    // reports the failure.
    a.fail_method("host.set_power_on_mode", None);
    a.fail_method("host.set_multipathing", Some(("HOST_NOT_DISABLED", &[])));
    let op = run(
        &h,
        ca,
        ActionRequest::SetProperties {
            target: PropertiesTarget::Host(r(HOST_A)),
            changes: Box::new(PropertyChanges {
                multipath: Some(true),
                ..PropertyChanges::default()
            }),
        },
    )
    .await;
    failed_with(&op, "HOST_NOT_DISABLED");
    assert_eq!(
        field(&a, "pbd", "OpaqueRef:pbd-local-01-a", "currently_attached"),
        json!(true),
        "the PBD was plugged again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_settings_operations() {
    let h = harness();
    let a = MockServer::start_lab(false, "a", 1).await;
    a.set_task_timing(2, Duration::from_millis(20));
    let ca = connect(&h, &a).await;

    // Change Server Password: a wrong current password is refused, the
    // right one changes it and later logins use the new one.
    let op = run(
        &h,
        ca,
        ActionRequest::ChangePassword {
            old: Password::new("wrong"),
            new: Password::new("new-secret"),
        },
    )
    .await;
    failed_with(&op, errors::WRONG_PASSWORD);
    assert_eq!(a.root_password(), xapi_mock::PASSWORD);
    let op = run(
        &h,
        ca,
        ActionRequest::ChangePassword {
            old: Password::new(xapi_mock::PASSWORD),
            new: Password::new("new-secret"),
        },
    )
    .await;
    ok(&op);
    assert_eq!(a.root_password(), "new-secret");
    let before = last_id(&h);
    a.invalidate_sessions();
    h.wait_for("logged in again with the new password", |s| {
        s.operations.iter().any(|o| {
            o.id > before
                && matches!(o.kind, xen_core::OperationKind::Reconnect)
                && matches!(o.status, OperationStatus::Succeeded)
        }) && s.connection(ca).is_some_and(|c| c.status.is_connected())
    })
    .await;
    let op = run(&h, ca, ActionRequest::RotatePoolSecret).await;
    ok(&op);
    assert_eq!(a.secret_rotations(), 1);

    // Certificate verification.
    a.set_field("pool", POOL_A, "tls_verification_enabled", json!(false));
    let op = run(&h, ca, ActionRequest::EnableTlsVerification).await;
    ok(&op);
    assert_eq!(
        field(&a, "pool", POOL_A, "tls_verification_enabled"),
        json!(true)
    );

    // Install Certificate: a missing file, a bad certificate, a good one.
    let key = temp_path("key.pem");
    let cert = temp_path("cert.pem");
    std::fs::write(
        &key,
        "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
    )
    .unwrap();
    std::fs::write(&cert, "not a certificate").unwrap();
    let install = |cert: &PathBuf, key: &PathBuf| ActionRequest::InstallCertificate {
        host: r(HOST_A),
        key: key.display().to_string(),
        certificate: cert.display().to_string(),
        chain: vec![],
    };
    let op = run(&h, ca, install(&cert, &temp_path("missing.pem"))).await;
    failed_with(&op, errors::FILE_MISSING);
    let op = run(&h, ca, install(&cert, &key)).await;
    failed_with(&op, "SERVER_CERTIFICATE_INVALID");
    std::fs::write(
        &cert,
        "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    let old = field(&a, "host", HOST_A, "certificates");
    let op = run(&h, ca, install(&cert, &key)).await;
    ok(&op);
    assert_ne!(field(&a, "host", HOST_A, "certificates"), old);
    let op = run(&h, ca, ActionRequest::ResetCertificate { host: r(HOST_A) }).await;
    ok(&op);
    let _ = std::fs::remove_file(&key);
    let _ = std::fs::remove_file(&cert);

    // Back up, restore; a backup of another server is refused.
    let backup = temp_path("host.xbk");
    let op = run(
        &h,
        ca,
        ActionRequest::BackupHost {
            host: r(HOST_A),
            path: backup.display().to_string(),
        },
    )
    .await;
    ok(&op);
    let body = std::fs::read_to_string(&backup).unwrap();
    assert!(body.starts_with("XCP-NG-MOCK-HOST-BACKUP "));
    let op = run(
        &h,
        ca,
        ActionRequest::RestoreHost {
            host: r(HOST_A),
            path: backup.display().to_string(),
        },
    )
    .await;
    ok(&op);
    assert_eq!(a.restored(), vec![HOST_A.to_owned()]);
    std::fs::write(&backup, "XCP-NG-MOCK-HOST-BACKUP other-uuid x").unwrap();
    let op = run(
        &h,
        ca,
        ActionRequest::RestoreHost {
            host: r(HOST_A),
            path: backup.display().to_string(),
        },
    )
    .await;
    failed_with(&op, "RESTORE_TARGET_MISMATCH");
    let op = run(
        &h,
        ca,
        ActionRequest::RestoreHost {
            host: r(HOST_A),
            path: temp_path("none.xbk").display().to_string(),
        },
    )
    .await;
    failed_with(&op, errors::FILE_MISSING);
    let _ = std::fs::remove_file(&backup);

    // Control domain memory, then the server reboots.
    let op = run(
        &h,
        ca,
        ActionRequest::SetDom0Memory {
            host: r(HOST_A),
            memory: 4 * 1024 * 1024 * 1024,
        },
    )
    .await;
    ok(&op);
    assert_eq!(
        field(&a, "vm", DOM0_A, "memory_dynamic_min"),
        json!(4_i64 * 1024 * 1024 * 1024)
    );
    assert!(a.calls().iter().any(|c| c == "Async.host.reboot"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pool_operations_need_the_right_role() {
    let h = harness();
    let a = MockServer::start_lab(false, "a", 1).await;
    let b = MockServer::start_lab(false, "b", 2).await;
    let ca = connect(&h, &a).await;
    // b as a pool operator: not allowed to join a pool.
    h.backend.send(Command::Connect {
        server: b.address(),
        username: "pool-operator-user".into(),
        password: Some("pool-operator-password".into()),
        remember_password: false,
    });
    let state = h
        .wait_for("b connected as pool operator", |s| {
            s.connections
                .iter()
                .any(|c| c.server == b.address() && c.status.is_connected() && c.rbac.is_some())
        })
        .await;
    let vb = state
        .connections
        .iter()
        .find(|c| c.server == b.address())
        .unwrap();
    assert_eq!(
        pool::can_join_pool(Some(vb), Some(view(&state, ca)), true, true, 1),
        JoinReason::WrongRoleOnSupporter
    );
    // The client refuses Remove Server and Rotate Pool Secret for this role.
    let model = &vb.model;
    for request in [
        ActionRequest::EjectHost { host: r(HOST_B) },
        ActionRequest::RotatePoolSecret,
        ActionRequest::InstallCertificate {
            host: r(HOST_B),
            key: String::new(),
            certificate: String::new(),
            chain: vec![],
        },
    ] {
        assert!(
            matches!(
                xen_core::rbac::check(model, vb.rbac.as_deref(), &request.rbac_methods()),
                xen_core::RbacCheck::Denied { .. }
            ),
            "{request:?}"
        );
    }
    // A pool operator may save properties.
    let changes = PropertyChanges {
        name: Some("renamed".into()),
        ..PropertyChanges::default()
    };
    let request = ActionRequest::SetProperties {
        target: PropertiesTarget::Host(r(HOST_B)),
        changes: Box::new(changes),
    };
    assert_eq!(
        xen_core::rbac::check(model, vb.rbac.as_deref(), &request.rbac_methods()),
        xen_core::RbacCheck::Allowed
    );
}
