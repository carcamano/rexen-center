//! Logs never contain passwords or session references, even at TRACE level
//! with every dependency's logging on (reqwest, hyper, rustls…).

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::{first, harness, wait_until};
use xapi::Ref;
use xapi_mock::MockServer;
use xen_core::{Command, Notification, VmAction};

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_never_contain_passwords_or_session_refs() {
    let captured = Captured::default();
    let writer = captured.clone();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("trace"))
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .init();

    let server = MockServer::start(true).await;
    let mut h = harness();
    h.connect(&server, xapi_mock::PASSWORD);
    match h.next_notification().await {
        Notification::CertificatePrompt(p) => p.accept(),
        other => panic!("unexpected {other:?}"),
    }
    let state = h.wait_conn("connected", |c| c.status.is_connected()).await;
    let conn = first(&state).id;
    h.backend.send(Command::Vm {
        connection: conn,
        vm: Ref::new("OpaqueRef:vm-db"),
        action: VmAction::Start,
    });
    wait_until("task done", || server.call_count("task.destroy") == 1).await;
    // An operation run as another user (role elevation).
    h.backend.send(Command::RunElevated {
        connection: conn,
        request: xen_core::ActionRequest::Vm {
            vm: Ref::new("OpaqueRef:vm-web"),
            action: VmAction::Pause,
        },
        username: "vm-power-admin-user".into(),
        password: "vm-power-admin-password".into(),
        request_id: 1,
    });
    match h.next_notification().await {
        Notification::Elevation {
            outcome: xen_core::ElevationOutcome::Authorized { .. },
            ..
        } => {}
        other => panic!("unexpected {other:?}"),
    }
    wait_until("elevated task done", || {
        server.call_count("task.destroy") == 2
    })
    .await;
    // Re-login after a forgotten session and after a restart.
    server.invalidate_sessions();
    wait_until("re-login", || {
        server.call_count("session.login_with_password") == 2
    })
    .await;
    h.wait_conn("connected", |c| c.status.is_connected()).await;
    server.restart().await;
    wait_until("re-login", || {
        server.call_count("session.login_with_password") == 3
    })
    .await;
    h.wait_conn("connected", |c| c.status.is_connected()).await;
    let backend = h.backend;
    tokio::task::spawn_blocking(move || {
        backend.shutdown_blocking(std::time::Duration::from_secs(5));
    })
    .await
    .unwrap();

    let log = String::from_utf8_lossy(&captured.0.lock().unwrap()).into_owned();
    assert!(log.contains("connected"), "nothing captured");
    assert!(log.contains("xapi call"), "call tracing not captured");
    assert!(log.len() > 10_000, "too little captured: {}", log.len());
    assert!(!log.contains(xapi_mock::PASSWORD), "password in the log");
    assert!(
        !log.contains("vm-power-admin-password"),
        "elevated password in the log"
    );
    let leaks: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("mock-session-"))
        .take(5)
        .collect();
    assert!(leaks.is_empty(), "session ref in the log: {leaks:#?}");
}
