//! End-to-end: the full object cache against the mock server.
//!
//! Covers the F2a cache rules: an `event.inject` token taken before the
//! initial load, every class loaded and kept live, full reload on
//! `EVENTS_LOST`, re-login and reload on `SESSION_INVALID`, and classes an
//! older server does not know.

mod common;

use common::{Harness, first, harness, wait_until};
use serde_json::json;
use xapi::{Ref, VmPowerState};
use xapi_mock::MockServer;
use xen_core::{Command, ConnectionStatus};

/// Connects and waits until the event loop is following changes.
async fn connected(server: &MockServer) -> Harness {
    let h = harness();
    h.connect(server, xapi_mock::PASSWORD);
    h.wait_conn("connected", |c| c.status.is_connected()).await;
    wait_until("event loop", || server.call_count("event.from") >= 1).await;
    h
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loads_every_class_after_taking_an_event_token() {
    let server = MockServer::start_with_records(false, xapi_mock::capture_records()).await;
    // An XCP-ng 8.2 host does not know these classes.
    server.set_unknown_classes(&["VM_group", "Host_driver", "Driver_variant"]);
    let h = connected(&server).await;
    let state = h.backend.state();
    let model = &first(&state).model;

    assert_eq!(model.pools.len(), 1);
    assert_eq!(model.hosts.len(), 1);
    assert_eq!(model.vms.len(), 13);
    assert_eq!(model.srs.len(), 6);
    assert_eq!(model.vdis.len(), 16);
    assert_eq!(model.vbds.len(), 19);
    assert_eq!(model.pifs.len(), 6);
    assert_eq!(model.networks.len(), 7);
    assert_eq!(model.consoles.len(), 11);
    assert_eq!(model.roles.len(), 7);
    assert_eq!(model.messages.len(), 8);
    assert!(model.host_drivers.is_empty());
    assert!(model.driver_variants.is_empty());

    // The token comes first, then the records.
    let calls = server.calls();
    let inject = calls.iter().position(|c| c == "event.inject").unwrap();
    let first_load = calls
        .iter()
        .position(|c| c.ends_with(".get_all_records"))
        .unwrap();
    assert!(inject < first_load, "{calls:?}");
    assert!(calls.iter().any(|c| c == "SR.get_all_records"));
    assert!(
        calls
            .iter()
            .any(|c| c == "VM_guest_metrics.get_all_records")
    );
    // Sessions and secrets are never listed.
    assert!(!calls.iter().any(|c| c == "secret.get_all_records"));
    assert!(!calls.iter().any(|c| c == "session.get_all_records"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn objects_of_any_class_follow_events() {
    let server = MockServer::start(false).await;
    let h = connected(&server).await;

    server.add_object(
        "SR",
        "OpaqueRef:sr-new-nfs",
        json!({"uuid": "sr-nfs-uuid", "name_label": "NFS store", "type": "nfs", "shared": true}),
    );
    let state = h
        .wait_conn("SR added", |c| {
            c.model.srs.contains_key(&Ref::new("OpaqueRef:sr-new-nfs"))
        })
        .await;
    let sr = &first(&state).model.srs[&Ref::new("OpaqueRef:sr-new-nfs")];
    assert_eq!(sr.name_label, "NFS store");
    assert!(sr.shared);

    server.set_name_label("SR", "OpaqueRef:sr-new-nfs", "NFS renamed");
    h.wait_conn("SR renamed", |s| {
        s.model
            .srs
            .get(&Ref::new("OpaqueRef:sr-new-nfs"))
            .is_some_and(|r| r.name_label == "NFS renamed")
    })
    .await;

    server.remove_object("SR", "OpaqueRef:sr-new-nfs");
    h.wait_conn("SR removed", |s| {
        !s.model.srs.contains_key(&Ref::new("OpaqueRef:sr-new-nfs"))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_lost_reloads_the_whole_cache() {
    let server = MockServer::start(false).await;
    let h = connected(&server).await;
    assert_eq!(server.call_count("event.inject"), 1);

    server.lose_events();
    wait_until("reload", || server.call_count("event.inject") == 2).await;
    wait_until("second load", || {
        server.call_count("VM.get_all_records") == 2
    })
    .await;
    assert!(first(&h.backend.state()).status.is_connected());
    // Same session: no second login.
    assert_eq!(server.call_count("session.login_with_password"), 1);

    // Live updates still flow after the reload.
    server.set_vm_power_state("OpaqueRef:vm-db", "Running");
    h.wait_conn("db-01 running", |s| {
        s.model
            .vms
            .get(&Ref::new("OpaqueRef:vm-db"))
            .is_some_and(|v| v.power_state == VmPowerState::Running)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_session_logs_in_again_and_reloads() {
    let server = MockServer::start(false).await;
    let h = connected(&server).await;

    server.invalidate_sessions();
    wait_until("re-login", || {
        server.call_count("session.login_with_password") == 2
    })
    .await;
    wait_until("reload", || server.call_count("event.inject") == 2).await;
    let state = h
        .wait_conn("connected again", |c| {
            matches!(c.status, ConnectionStatus::Connected { .. })
        })
        .await;
    assert_eq!(server.active_sessions(), 1);

    // VM operations use the new session.
    h.backend.send(Command::Vm {
        connection: first(&state).id,
        vm: Ref::new("OpaqueRef:vm-db"),
        action: xen_core::VmAction::Start,
    });
    h.wait_conn("db-01 started", |s| {
        s.model
            .vms
            .get(&Ref::new("OpaqueRef:vm-db"))
            .is_some_and(|v| v.power_state == VmPowerState::Running)
    })
    .await;
}
