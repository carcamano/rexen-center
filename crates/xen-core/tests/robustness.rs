//! End-to-end F2b behaviour against the mock server: reconnection with
//! backoff after a server restart, the heartbeat, the `HOST_IS_SLAVE`
//! redirect, duplicate detection, changed certificates, persisted settings
//! and passwords, and operations that run as tasks.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{WAIT, fast_options, first, harness, memory_store, spawn, wait_until};
use xapi::{Fingerprint, Ref, TaskStatusType, VmPowerState};
use xapi_mock::{MockCertificate, MockServer};
use xen_core::{
    Command, ConfigPaths, ConnectionError, ConnectionStatus, MemorySecretStore, Notification,
    OperationFailure, OperationKind, OperationStatus, SavedServer, SecretStore, Settings, Store,
    VmAction,
};

fn is_running(state: &xen_core::AppState, vm: &str) -> bool {
    first(state)
        .model
        .vms
        .get(&Ref::new(vm))
        .is_some_and(|v| v.power_state == VmPowerState::Running)
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("xen-core-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnects_after_the_server_restarts() {
    let server = MockServer::start(false).await;
    let h = harness();
    h.connect(&server, xapi_mock::PASSWORD);
    let state = h.wait_conn("connected", |c| c.status.is_connected()).await;
    let conn = first(&state).id;
    let objects = first(&state).model.object_count();

    // The server goes away: the connection notices and keeps retrying, with
    // the last cache still shown.
    server.stop();
    let state = h
        .wait_conn(
            "reconnecting",
            |c| matches!(c.status, ConnectionStatus::Reconnecting { attempt, .. } if attempt >= 2),
        )
        .await;
    assert_eq!(first(&state).model.object_count(), objects);
    assert!(
        state
            .operations
            .iter()
            .any(|o| o.kind == OperationKind::Reconnect && o.is_running())
    );
    // Operations are refused while reconnecting.
    h.backend.send(Command::Vm {
        connection: conn,
        vm: Ref::new("OpaqueRef:vm-db"),
        action: VmAction::Start,
    });
    h.wait_for("refused operation", |s| {
        s.operations.iter().any(|o| {
            matches!(o.kind, OperationKind::VmAction { .. })
                && matches!(
                    o.status,
                    OperationStatus::Failed(OperationFailure::NotConnected)
                )
        })
    })
    .await;

    // It comes back (every session forgotten, like a toolstack restart).
    server.restart().await;
    let state = h
        .wait_conn("reconnected", |c| c.status.is_connected() && c.id == conn)
        .await;
    assert_eq!(server.call_count("session.login_with_password"), 2);
    assert_eq!(server.active_sessions(), 1);
    assert!(
        state
            .operations
            .iter()
            .any(|o| o.kind == OperationKind::Reconnect
                && matches!(o.status, OperationStatus::Succeeded))
    );

    // Live updates and operations work on the new session.
    server.set_vm_power_state("OpaqueRef:vm-build", "Running");
    h.wait_for("live update", |s| is_running(s, "OpaqueRef:vm-build"))
        .await;
    h.backend.send(Command::Vm {
        connection: conn,
        vm: Ref::new("OpaqueRef:vm-db"),
        action: VmAction::Start,
    });
    h.wait_for("db-01 started", |s| is_running(s, "OpaqueRef:vm-db"))
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_now_skips_the_backoff() {
    let server = MockServer::start(false).await;
    let (store, _) = memory_store();
    let h = spawn(xen_core::BackendOptions {
        reconnect_delay: Duration::from_secs(600),
        max_reconnect_delay: Duration::from_secs(600),
        ..fast_options(store)
    });
    h.connect(&server, xapi_mock::PASSWORD);
    let state = h.wait_conn("connected", |c| c.status.is_connected()).await;
    let conn = first(&state).id;
    server.stop();
    h.wait_conn("waiting to retry", |c| {
        matches!(
            c.status,
            ConnectionStatus::Reconnecting {
                retry_at: Some(_),
                ..
            }
        )
    })
    .await;
    server.restart().await;
    h.backend.send(Command::RetryNow { connection: conn });
    h.wait_conn("reconnected", |c| c.status.is_connected())
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeat_detects_a_hung_server() {
    let server = MockServer::start(false).await;
    let h = harness();
    h.connect(&server, xapi_mock::PASSWORD);
    h.wait_conn("connected", |c| c.status.is_connected()).await;
    wait_until("heartbeat", || {
        server.call_count("host.get_servertime") >= 1
    })
    .await;

    // Requests hang: the event long-poll would wait for its long HTTP
    // timeout, but two missed heartbeats declare the connection lost.
    server.set_unresponsive(true);
    let state = h
        .wait_conn("lost", |c| {
            matches!(c.status, ConnectionStatus::Reconnecting { .. })
        })
        .await;
    let ConnectionStatus::Reconnecting { error, .. } = &first(&state).status else {
        unreachable!()
    };
    assert!(matches!(**error, ConnectionError::Lost(_)), "{error:?}");

    server.set_unresponsive(false);
    h.wait_conn("reconnected", |c| c.status.is_connected())
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pool_member_redirects_to_the_coordinator() {
    let coordinator = MockServer::start(false).await;
    let member = MockServer::start(false).await;
    member.redirect_logins_to(Some(&coordinator.authority()));
    let h = harness();
    h.connect(&member, xapi_mock::PASSWORD);
    let state = h.wait_conn("connected", |c| c.status.is_connected()).await;

    let coordinator_label = coordinator.address();
    assert_eq!(first(&state).server, coordinator_label);
    assert_eq!(member.call_count("session.login_with_password"), 1);
    assert_eq!(member.active_sessions(), 0);
    assert_eq!(coordinator.active_sessions(), 1);
    assert_eq!(first(&state).model.tree()[0].name, "Lab Pool");
    // The coordinator is what gets saved.
    let saved: Vec<&str> = state
        .settings
        .servers
        .iter()
        .map(|s| s.address.as_str())
        .collect();
    assert_eq!(saved, [coordinator_label.as_str()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redirect_loops_give_up() {
    let a = MockServer::start(false).await;
    let b = MockServer::start(false).await;
    a.redirect_logins_to(Some(&b.authority()));
    b.redirect_logins_to(Some(&a.authority()));
    let h = harness();
    h.connect(&a, xapi_mock::PASSWORD);
    let state = h.wait_conn("failed", |c| c.status.is_failed()).await;
    let ConnectionStatus::Failed { error } = &first(&state).status else {
        unreachable!()
    };
    assert!(matches!(**error, ConnectionError::TooManyRedirects));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_connections_are_refused() {
    let coordinator = MockServer::start(false).await;
    let mut h = harness();
    h.connect(&coordinator, xapi_mock::PASSWORD);
    h.wait_conn("connected", |c| c.status.is_connected()).await;

    // The same address again: refused before logging in.
    h.connect(&coordinator, xapi_mock::PASSWORD);
    match h.next_notification().await {
        Notification::ConnectionRefused { error, .. } => {
            assert!(matches!(*error, ConnectionError::AlreadyConnected { .. }));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(coordinator.call_count("session.login_with_password"), 1);

    // The same pool under another name: refused once the pool is known, and
    // its session is logged out.
    let alias = coordinator.address().replace("127.0.0.1", "localhost");
    h.connect_to(&alias, xapi_mock::PASSWORD);
    match h.next_notification().await {
        Notification::ConnectionRefused { error, .. } => match &*error {
            ConnectionError::AlreadyConnected { existing } => {
                assert_eq!(existing, &coordinator.address());
            }
            other => panic!("unexpected {other:?}"),
        },
        other => panic!("unexpected {other:?}"),
    }
    wait_until("duplicate logged out", || {
        coordinator.active_sessions() == 1
    })
    .await;

    // A member of an already connected pool: refused at the redirect.
    let member = MockServer::start(false).await;
    member.redirect_logins_to(Some(&coordinator.authority()));
    h.connect(&member, xapi_mock::PASSWORD);
    match h.next_notification().await {
        Notification::ConnectionRefused { error, .. } => {
            assert!(matches!(*error, ConnectionError::AlreadyConnected { .. }));
        }
        other => panic!("unexpected {other:?}"),
    }
    let state = h.backend.state();
    assert_eq!(state.connections.len(), 1);
    assert_eq!(
        state
            .operations
            .iter()
            .filter(|o| matches!(o.status, OperationStatus::Failed(_)))
            .count(),
        3
    );

    // Two different pools at once are fine.
    let other_pool = MockServer::start_with_records(false, xapi_mock::capture_records()).await;
    h.connect(&other_pool, xapi_mock::PASSWORD);
    h.wait_for("two connections", |s| {
        s.connections.len() == 2 && s.connections.iter().all(|c| c.status.is_connected())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_certificate_is_asked_about_again() {
    let server = MockServer::start(true).await;
    let (store, _) = memory_store();
    let mut h = spawn(fast_options(store.clone()));
    h.connect(&server, xapi_mock::PASSWORD);
    let original = Fingerprint::of_der(&MockCertificate::Original.der());
    match h.next_notification().await {
        Notification::CertificatePrompt(p) => {
            assert_eq!((p.fingerprint, p.previous), (original, None));
            p.accept();
        }
        other => panic!("unexpected {other:?}"),
    }
    h.wait_conn("connected", |c| c.status.is_connected()).await;
    assert_eq!(store.trust().get(&server.authority()), Some(original));

    // The host comes back with a new certificate: the reconnection stops and
    // asks, showing both fingerprints; no credentials are sent meanwhile.
    server.set_certificate(MockCertificate::Renewed);
    server.restart().await;
    let renewed = Fingerprint::of_der(&MockCertificate::Renewed.der());
    let prompt = match h.next_notification().await {
        Notification::CertificatePrompt(p) => p,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(
        (prompt.fingerprint, prompt.previous),
        (renewed, Some(original))
    );
    assert_eq!(server.call_count("session.login_with_password"), 1);
    prompt.accept();
    h.wait_conn("reconnected", |c| c.status.is_connected())
        .await;
    assert_eq!(store.trust().get(&server.authority()), Some(renewed));

    // Refusing a changed certificate ends the connection for good.
    server.set_certificate(MockCertificate::Original);
    server.restart().await;
    match h.next_notification().await {
        Notification::CertificatePrompt(p) => p.reject(),
        other => panic!("unexpected {other:?}"),
    }
    let state = h.wait_conn("failed", |c| c.status.is_failed()).await;
    let ConnectionStatus::Failed { error } = &first(&state).status else {
        unreachable!()
    };
    assert!(matches!(**error, ConnectionError::CertificateRejected));
    assert_eq!(store.trust().get(&server.authority()), Some(renewed));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn servers_certificates_and_passwords_survive_a_restart() {
    let server = MockServer::start(true).await;
    let dir = temp_dir("persist");
    let secrets = Arc::new(MemorySecretStore::new());

    // First run: connect, trust the certificate, save the password.
    {
        let store = Arc::new(Store::open(ConfigPaths::at(&dir), secrets.clone()));
        let mut h = spawn(fast_options(store));
        h.connect(&server, xapi_mock::PASSWORD);
        match h.next_notification().await {
            Notification::CertificatePrompt(p) => p.accept(),
            other => panic!("unexpected {other:?}"),
        }
        h.wait_conn("connected", |c| c.status.is_connected()).await;
        let backend = h.backend;
        tokio::task::spawn_blocking(move || backend.shutdown_blocking(WAIT))
            .await
            .unwrap();
    }
    assert_eq!(server.active_sessions(), 0);
    let account = format!("root@https://{}", server.authority());
    assert_eq!(secrets.accounts(), std::slice::from_ref(&account));
    for file in ["settings.toml", "known_servers.toml"] {
        let text = std::fs::read_to_string(dir.join(file)).unwrap();
        assert!(
            !text.contains(xapi_mock::PASSWORD),
            "{file} has the password"
        );
        assert!(!text.contains("mock-session"), "{file} has a session ref");
    }
    let settings = std::fs::read_to_string(dir.join("settings.toml")).unwrap();
    assert!(
        settings.contains(&server.address().replace("https://", "")),
        "{settings}"
    );
    let fingerprint = Fingerprint::of_der(&MockCertificate::Original.der()).to_string();
    let known = std::fs::read_to_string(dir.join("known_servers.toml")).unwrap();
    assert!(known.contains(&fingerprint), "{known}");

    // Second run: reconnects on its own, with no password and no prompt.
    let store = Arc::new(Store::open(ConfigPaths::at(&dir), secrets.clone()));
    let mut h = spawn(fast_options(store));
    let state = h.wait_conn("restored", |c| c.status.is_connected()).await;
    assert_eq!(first(&state).username, "root");
    assert!(h.backend.try_recv().is_none(), "no prompt expected");
    assert_eq!(server.call_count("session.login_with_password"), 2);

    // Forgetting the server deletes its password and its entry.
    h.backend.send(Command::Forget {
        address: first(&state).server.clone(),
    });
    let state = h
        .wait_for("forgotten", |s| {
            s.connections.is_empty() && s.settings.servers.is_empty()
        })
        .await;
    assert!(state.settings.servers.is_empty());
    assert!(secrets.accounts().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restoring_without_a_saved_password_needs_the_user() {
    let server = MockServer::start(false).await;
    let settings = Settings {
        servers: vec![SavedServer {
            address: server.address(),
            username: "root".into(),
            reconnect: true,
        }],
        ..Settings::default()
    };
    let store = Arc::new(Store::in_memory_with(
        settings,
        Arc::new(MemorySecretStore::new()),
    ));
    let h = spawn(fast_options(store));
    let state = h.wait_conn("failed", |c| c.status.is_failed()).await;
    let ConnectionStatus::Failed { error } = &first(&state).status else {
        unreachable!()
    };
    assert!(matches!(**error, ConnectionError::PasswordRequired));
    assert!(server.calls().is_empty());

    // Typing the password replaces the failed entry.
    h.connect(&server, xapi_mock::PASSWORD);
    let state = h
        .wait_for("connected", |s| {
            s.connections.len() == 1 && first(s).status.is_connected()
        })
        .await;
    assert_eq!(state.connections.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnection_reads_the_password_from_the_keyring() {
    let server = MockServer::start(false).await;
    let (store, secrets) = memory_store();
    let h = spawn(fast_options(store));
    h.connect(&server, xapi_mock::PASSWORD);
    h.wait_conn("connected", |c| c.status.is_connected()).await;
    let account = format!("root@{}", server.address());
    assert_eq!(
        secrets.get(&account).unwrap().as_deref(),
        Some(xapi_mock::PASSWORD)
    );

    // Change the saved password behind the backend's back: the next login
    // uses it (the backend no longer holds its own copy) and fails for good.
    secrets.set(&account, "changed-elsewhere").unwrap();
    server.invalidate_sessions();
    let state = h.wait_conn("failed", |c| c.status.is_failed()).await;
    let ConnectionStatus::Failed { error } = &first(&state).status else {
        unreachable!()
    };
    assert!(error.is_fatal());
    assert!(
        error
            .xapi_error()
            .is_some_and(|e| e.is_api("SESSION_AUTHENTICATION_FAILED")),
        "{error:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_password_not_remembered_stays_out_of_the_keyring() {
    let server = MockServer::start(false).await;
    let (store, secrets) = memory_store();
    secrets
        .set(&format!("root@{}", server.address()), "stale")
        .unwrap();
    let h = spawn(fast_options(store));
    h.backend.send(Command::Connect {
        server: server.address(),
        username: "root".into(),
        password: Some(xapi_mock::PASSWORD.into()),
        remember_password: false,
    });
    h.wait_conn("connected", |c| c.status.is_connected()).await;
    assert!(secrets.accounts().is_empty(), "the stale copy is deleted");

    // Reconnection still works from memory.
    server.invalidate_sessions();
    wait_until("re-login", || {
        server.call_count("session.login_with_password") == 2
    })
    .await;
    h.wait_conn("reconnected", |c| c.status.is_connected())
        .await;
    assert!(secrets.accounts().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_operations_run_as_tasks_with_progress_and_cancel() {
    let server = MockServer::start(false).await;
    server.set_task_timing(5, Duration::from_millis(100));
    let h = harness();
    h.connect(&server, xapi_mock::PASSWORD);
    let state = h.wait_conn("connected", |c| c.status.is_connected()).await;
    let conn = first(&state).id;

    // A normal start: progress, then success; the task is destroyed.
    h.backend.send(Command::Vm {
        connection: conn,
        vm: Ref::new("OpaqueRef:vm-db"),
        action: VmAction::Start,
    });
    let state = h
        .wait_for("progress", |s| {
            s.operations
                .iter()
                .any(|o| o.is_running() && o.progress.is_some_and(|p| p > 0.0 && p < 1.0))
        })
        .await;
    let op = state.operations.last().unwrap().clone();
    assert!(op.can_cancel());
    assert_eq!(
        op.kind,
        OperationKind::VmAction {
            vm_name: "db-01".into(),
            action: VmAction::Start
        }
    );
    assert!(server.calls().iter().any(|c| c == "Async.VM.start"));
    let state = h
        .wait_for("done", |s| {
            s.operation(op.id)
                .is_some_and(|o| matches!(o.status, OperationStatus::Succeeded))
        })
        .await;
    assert_eq!(state.operation(op.id).unwrap().progress, Some(1.0));
    wait_until("task destroyed", || server.tasks().is_empty()).await;
    h.wait_for("db-01 running", |s| is_running(s, "OpaqueRef:vm-db"))
        .await;

    // A slow shutdown, cancelled half-way: the VM keeps running.
    server.set_task_timing(50, Duration::from_millis(100));
    h.backend.send(Command::Vm {
        connection: conn,
        vm: Ref::new("OpaqueRef:vm-db"),
        action: VmAction::CleanShutdown,
    });
    let state = h
        .wait_for("cancellable", |s| {
            s.operations.last().is_some_and(|o| o.can_cancel())
        })
        .await;
    let op = state.operations.last().unwrap().id;
    h.backend.send(Command::CancelOperation { operation: op });
    h.wait_for("cancelled", |s| {
        s.operation(op)
            .is_some_and(|o| matches!(o.status, OperationStatus::Cancelled))
    })
    .await;
    assert!(server.calls().iter().any(|c| c == "task.cancel"));
    assert!(is_running(&h.backend.state(), "OpaqueRef:vm-db"));
    assert_eq!(
        server.record("vm", "OpaqueRef:vm-db").unwrap()["power_state"],
        "Running"
    );

    // Clearing the history keeps only running entries.
    h.backend.send(Command::ClearFinishedOperations);
    h.wait_for("cleared", |s| s.operations.is_empty()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tasks_of_other_clients_are_followed_and_can_be_cancelled() {
    let server = MockServer::start(false).await;
    server.set_task_timing(100, Duration::from_millis(100));
    let h = harness();
    h.connect(&server, xapi_mock::PASSWORD);
    let state = h.wait_conn("connected", |c| c.status.is_connected()).await;
    let conn = first(&state).id;

    let task = Ref::new(server.start_task("Async.VM.export"));
    let state = h
        .wait_conn("task progress", |c| {
            c.model.tasks.get(&task).is_some_and(|t| t.progress > 0.0)
        })
        .await;
    assert_eq!(
        first(&state).model.tasks[&task].name_label,
        "Async.VM.export"
    );

    h.backend.send(Command::CancelTask {
        connection: conn,
        task: task.clone(),
    });
    let state = h
        .wait_conn("task cancelled", |c| {
            c.model
                .tasks
                .get(&task)
                .is_some_and(|t| t.status == TaskStatusType::Cancelled)
        })
        .await;
    let op = state.operations.last().unwrap();
    assert_eq!(
        op.kind,
        OperationKind::CancelTask {
            task_name: "Async.VM.export".into()
        }
    );
    h.wait_for("cancel recorded", |s| {
        s.operations
            .last()
            .is_some_and(|o| matches!(o.status, OperationStatus::Succeeded))
    })
    .await;
}
