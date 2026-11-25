//! End-to-end: backend + mock XAPI server over HTTPS, no UI.
//!
//! Proves the F1 flow: connect with trust-on-first-use, initial tree, start
//! and clean shutdown reflected through `event.from`, changes made by others
//! reflected without any refresh, repaint requests, and logout.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{WAIT, first, harness};
use xapi::{Fingerprint, Ref, VmPowerState};
use xapi_mock::MockServer;
use xen_core::{AppState, Command, ConnectionError, ConnectionStatus, Notification, VmAction};

fn power_state(state: &AppState, vm: &str) -> Option<VmPowerState> {
    first(state)
        .model
        .vms
        .get(&Ref::new(vm))
        .map(|r| r.power_state.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_operate_and_follow_live_updates() {
    let server = MockServer::start(true).await;
    let mut h = harness();
    h.connect(&server, xapi_mock::PASSWORD);

    // Trust on first use: the backend asks before sending credentials.
    let prompt = match h.next_notification().await {
        Notification::CertificatePrompt(p) => p,
        other => panic!("expected certificate prompt, got {other:?}"),
    };
    assert_eq!(
        prompt.fingerprint,
        Fingerprint::of_der(&xapi_mock::certificate_der())
    );
    assert!(prompt.previous.is_none());
    assert!(server.calls().is_empty());
    prompt.accept();

    let state = h.wait_conn("connected", |c| c.status.is_connected()).await;
    let conn = first(&state).id;
    let ConnectionStatus::Connected { api_version, .. } = &first(&state).status else {
        unreachable!()
    };
    assert_eq!(*api_version, (2, 21));
    let tree = first(&state).model.tree();
    assert_eq!(tree[0].name, "Lab Pool");
    assert_eq!(tree[0].hosts.len(), 2);
    for method in [
        "session.login_with_password",
        "pool.get_all_records",
        "host.get_all_records",
        "VM.get_all_records",
    ] {
        assert!(
            server.calls().iter().any(|c| c == method),
            "{method} not called"
        );
    }
    // Event loop is running.
    h.wait_for("first event.from", |_| {
        server.calls().iter().filter(|c| *c == "event.from").count() >= 2
    })
    .await;

    // Start: the new power state arrives through event.from.
    let repaints_before = h.repaints.load(Ordering::SeqCst);
    h.backend.send(Command::Vm {
        connection: conn,
        vm: Ref::new("OpaqueRef:vm-db"),
        action: VmAction::Start,
    });
    let state = h
        .wait_for("db-01 running", |s| {
            power_state(s, "OpaqueRef:vm-db") == Some(VmPowerState::Running)
        })
        .await;
    let host2 = &first(&state).model.tree()[0].hosts[1];
    assert!(
        host2
            .vms
            .iter()
            .any(|v| v.name == "db-01" && v.power_state == VmPowerState::Running)
    );
    assert!(
        h.repaints.load(Ordering::SeqCst) > repaints_before,
        "backend must request repaints"
    );
    h.wait_conn("busy cleared", |c| c.busy_vms.is_empty()).await;

    // Clean shutdown.
    h.backend.send(Command::Vm {
        connection: conn,
        vm: Ref::new("OpaqueRef:vm-web"),
        action: VmAction::CleanShutdown,
    });
    h.wait_for("web-01 halted", |s| {
        power_state(s, "OpaqueRef:vm-web") == Some(VmPowerState::Halted)
    })
    .await;

    // Someone else changes things: the tree follows with no refresh.
    server.set_vm_power_state("OpaqueRef:vm-build", "Running");
    server.set_name_label("pool", "OpaqueRef:pool-0001", "Renamed Pool");
    let state = h
        .wait_for("external changes", |s| {
            power_state(s, "OpaqueRef:vm-build") == Some(VmPowerState::Running)
                && first(s)
                    .model
                    .tree()
                    .first()
                    .is_some_and(|p| p.name == "Renamed Pool")
        })
        .await;
    assert!(
        first(&state).model.tree()[0].vms.is_empty(),
        "build-runner moved under its host"
    );

    // A refused operation surfaces as a notification.
    h.backend.send(Command::Vm {
        connection: conn,
        vm: Ref::new("OpaqueRef:vm-db"),
        action: VmAction::Start,
    });
    match h.next_notification().await {
        Notification::VmActionFailed {
            vm_name,
            action,
            error,
            ..
        } => {
            assert_eq!(vm_name, "db-01");
            assert_eq!(action, VmAction::Start);
            assert!(error.is_api("VM_BAD_POWER_STATE"), "{error:?}");
        }
        other => panic!("expected failure notification, got {other:?}"),
    }

    // Disconnect logs out, and the server is no longer restored on start.
    assert!(h.backend.state().settings.servers[0].reconnect);
    h.backend.send(Command::Disconnect { connection: conn });
    let state = h
        .wait_for("disconnected", |s| s.connections.is_empty())
        .await;
    assert!(!state.settings.servers[0].reconnect);
    let deadline = tokio::time::Instant::now() + WAIT;
    while server.active_sessions() > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "session not logged out"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejecting_the_certificate_aborts_without_sending_credentials() {
    let server = MockServer::start(true).await;
    let mut h = harness();
    h.connect(&server, xapi_mock::PASSWORD);
    match h.next_notification().await {
        Notification::CertificatePrompt(p) => p.reject(),
        other => panic!("expected certificate prompt, got {other:?}"),
    }
    let state = h.wait_conn("failed", |c| c.status.is_failed()).await;
    let ConnectionStatus::Failed { error, .. } = &first(&state).status else {
        unreachable!()
    };
    assert!(matches!(**error, ConnectionError::CertificateRejected));
    assert!(server.calls().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_password_fails_the_connection() {
    let server = MockServer::start(false).await;
    let h = harness();
    h.connect(&server, "wrong");
    let state = h.wait_conn("failed", |c| c.status.is_failed()).await;
    let ConnectionStatus::Failed { error, .. } = &first(&state).status else {
        unreachable!()
    };
    match &**error {
        ConnectionError::Connect(e) => assert!(e.is_api("SESSION_AUTHENTICATION_FAILED"), "{e:?}"),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_api_versions_are_refused() {
    let mut db = xapi_mock::fixture_records();
    for host in db["host"].as_object_mut().unwrap().values_mut() {
        host["API_version_minor"] = 15.into();
    }
    let server = MockServer::start_with_records(false, db).await;
    let h = harness();
    h.connect(&server, xapi_mock::PASSWORD);
    let state = h.wait_conn("failed", |c| c.status.is_failed()).await;
    let ConnectionStatus::Failed { error, .. } = &first(&state).status else {
        unreachable!()
    };
    assert!(
        matches!(**error, ConnectionError::Unsupported(_)),
        "{error:?}"
    );
    // The session opened for the version check is logged out.
    let deadline = tokio::time::Instant::now() + WAIT;
    while server.active_sessions() > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "session not logged out"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_logs_out() {
    let server = MockServer::start(false).await;
    let h = harness();
    h.connect(&server, xapi_mock::PASSWORD);
    h.wait_conn("connected", |c| c.status.is_connected()).await;
    assert_eq!(server.active_sessions(), 1);
    let backend = h.backend;
    tokio::task::spawn_blocking(move || backend.shutdown_blocking(Duration::from_secs(5)))
        .await
        .unwrap();
    assert_eq!(server.active_sessions(), 0);
}
