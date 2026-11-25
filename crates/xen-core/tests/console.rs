//! F5: consoles through the backend against the mock: the HTTPS tunnel with
//! the session cookie, the RFB session, input, Ctrl+Alt+Del, clipboard,
//! reconnection after a reboot, a stopped VM, the host (dom0) console, and
//! closing.

mod common;

use std::time::Duration;

use common::{first, harness, wait_until};
use xapi::{Host, Ref, Vm, VmPowerState};
use xapi_mock::MockServer;
use xen_core::console::{self, ConsoleEvent, ConsoleHandle, ConsoleStatus, ConsoleTarget};
use xen_core::{Command, ConnectionId, Notification, VmAction};
use xen_vnc::{Input, KeyCode};

/// A running guest of the capture.
const GUEST: &str = "OpaqueRef:vm-0001";
/// The capture's halted guest (no console record until it starts).
const HALTED: &str = "OpaqueRef:vm-0010";
const HOST: &str = "OpaqueRef:host-0001";
const DOM0: &str = "OpaqueRef:vm-0011";

async fn connect(h: &mut common::Harness, server: &MockServer) -> ConnectionId {
    h.connect(server, xapi_mock::PASSWORD);
    match h.next_notification().await {
        Notification::CertificatePrompt(p) => p.accept(),
        other => panic!("unexpected {other:?}"),
    }
    let state = h.wait_conn("connected", |c| c.status.is_connected()).await;
    first(&state).id
}

fn open(h: &common::Harness, conn: ConnectionId, target: ConsoleTarget) -> ConsoleHandle {
    let (handle, link) = console::pair();
    h.backend.send(Command::OpenConsole {
        connection: conn,
        target,
        link,
    });
    handle
}

fn rgb(handle: &ConsoleHandle, x: u16, y: u16) -> [u8; 3] {
    let p = handle
        .screen()
        .lock()
        .framebuffer
        .pixel(x, y)
        .unwrap_or_default();
    [p[0], p[1], p[2]]
}

async fn vm_op(h: &common::Harness, conn: ConnectionId, vm: &str, action: VmAction) {
    h.backend.send(Command::Vm {
        connection: conn,
        vm: Ref::new(vm),
        action,
    });
}

#[tokio::test]
async fn a_vm_console_shows_takes_input_and_reconnects_after_a_reboot() {
    let server = MockServer::start_with_records(true, xapi_mock::capture_records()).await;
    let mut h = harness();
    let conn = connect(&mut h, &server).await;
    let guest = server.console(GUEST);
    guest.fill(xen_vnc::Rect::new(100, 100, 10, 10), [10, 200, 30]);

    let console = open(&h, conn, ConsoleTarget::Vm(Ref::new(GUEST)));
    wait_until("the guest screen", || {
        rgb(&console, 105, 105) == [10, 200, 30]
    })
    .await;
    wait_until("scancode mode", || {
        console.status() == ConsoleStatus::Connected { scancodes: true }
    })
    .await;
    assert_eq!(server.call_count("http/connect_console"), 1);
    assert_eq!(console.screen().lock().framebuffer.width(), 640);

    // Keys as scancodes, Ctrl+Alt+Del, pointer and clipboard.
    for down in [true, false] {
        console.send(Input::Key {
            down,
            scancode: KeyCode::KeyX.scancode(),
            keysym: KeyCode::KeyX.keysym(false),
        });
    }
    console.send_ctrl_alt_del();
    console.send(Input::Pointer {
        buttons: 4,
        x: 320,
        y: 200,
    });
    console.send(Input::CutText("texto copiado".into()));
    wait_until("the input", || {
        guest.scancodes().len() == 8 && guest.cut_texts().len() == 1
    })
    .await;
    assert_eq!(
        guest.scancodes(),
        [
            (true, 0x2d),
            (false, 0x2d),
            (true, 0x1d),
            (true, 0x38),
            (true, 0xd3),
            (false, 0x1d),
            (false, 0x38),
            (false, 0xd3)
        ]
    );
    assert_eq!(guest.pointer_events(), [(4, 320, 200)]);
    assert_eq!(guest.cut_texts(), ["texto copiado"]);
    guest.cut_text("da VM");
    guest.bell();
    let events = std::sync::Mutex::new(Vec::new());
    wait_until("guest clipboard and bell", || {
        let mut all = events.lock().unwrap();
        all.extend(console.take_events());
        all.contains(&ConsoleEvent::CutText("da VM".into())) && all.contains(&ConsoleEvent::Bell)
    })
    .await;

    // A reboot drops the session; the console comes back by itself.
    assert_eq!(console.sessions(), 1);
    vm_op(&h, conn, GUEST, VmAction::HardReboot).await;
    wait_until("the second session", || console.sessions() == 2).await;
    wait_until("connected again", || {
        matches!(console.status(), ConsoleStatus::Connected { .. })
    })
    .await;
    assert_eq!(guest.sessions(), 2);

    // Closing the handle closes the session.
    drop(console);
    wait_until("no session left", || guest.active_sessions() == 0).await;
}

#[tokio::test]
async fn a_stopped_vm_waits_and_connects_when_it_starts() {
    let server = MockServer::start_with_records(true, xapi_mock::capture_records()).await;
    let mut h = harness();
    let conn = connect(&mut h, &server).await;
    let console = open(&h, conn, ConsoleTarget::Vm(Ref::new(HALTED)));
    wait_until("not running", || {
        console.status() == ConsoleStatus::NotRunning(VmPowerState::Halted)
    })
    .await;
    assert_eq!(server.call_count("http/connect_console"), 0);

    vm_op(&h, conn, HALTED, VmAction::Start).await;
    wait_until("connected after start", || {
        matches!(console.status(), ConsoleStatus::Connected { .. })
    })
    .await;
    let guest = server.console(HALTED);
    wait_until("the boot screen", || rgb(&console, 2, 2) == [255, 255, 255]).await;
    assert_eq!(guest.sessions(), 1);

    // Shut down: the session ends and the console says so.
    vm_op(&h, conn, HALTED, VmAction::HardShutdown).await;
    wait_until("halted again", || {
        console.status() == ConsoleStatus::NotRunning(VmPowerState::Halted)
    })
    .await;
    wait_until("no session", || guest.active_sessions() == 0).await;
}

#[tokio::test]
async fn the_host_console_is_dom0_and_takes_keysyms() {
    let server = MockServer::start_with_records(true, xapi_mock::capture_records()).await;
    let mut h = harness();
    let conn = connect(&mut h, &server).await;
    let console = open(&h, conn, ConsoleTarget::Host(Ref::<Host>::new(HOST)));
    wait_until("connected", || {
        console.status() == ConsoleStatus::Connected { scancodes: false }
    })
    .await;
    let dom0 = server.console(DOM0);
    assert_eq!(dom0.sessions(), 1);
    assert_eq!(console.screen().lock().framebuffer.height(), 384);
    // Without the QEMU Extended Key Event, keys go as keysyms.
    console.send_ctrl_alt_del();
    wait_until("keysyms", || dom0.keysyms().len() == 6).await;
    assert!(dom0.scancodes().is_empty());
}

#[tokio::test]
async fn a_lost_session_and_a_server_restart_are_survived() {
    let server = MockServer::start_with_records(true, xapi_mock::capture_records()).await;
    let mut h = harness();
    let conn = connect(&mut h, &server).await;
    let console = open(&h, conn, ConsoleTarget::Vm(Ref::<Vm>::new(GUEST)));
    wait_until("connected", || console.sessions() == 1).await;

    // The toolstack restarts: every session (and tunnel) is gone; the
    // connection logs in again and the console follows with the new session.
    server.restart().await;
    wait_until("reconnected console", || console.sessions() == 2).await;
    assert!(matches!(console.status(), ConsoleStatus::Connected { .. }));
    assert_eq!(server.console(GUEST).active_sessions(), 1);

    // A connection that is removed ends its consoles.
    h.backend.send(Command::Disconnect { connection: conn });
    tokio::time::sleep(Duration::from_millis(300)).await;
    wait_until("closed", || server.console(GUEST).active_sessions() == 0).await;
}

#[tokio::test]
async fn an_invalid_session_is_refused_by_the_tunnel() {
    let server = MockServer::start_with_records(false, xapi_mock::capture_records()).await;
    let model = xen_core::ConnectionModel::from_database(&xapi_mock::capture_records());
    // The fixture model's locations still name the capture's host; take the
    // mock's own record instead.
    let location = server
        .record("console", "OpaqueRef:console-0003")
        .and_then(|r| r["location"].as_str().map(str::to_owned))
        .unwrap();
    assert!(location.starts_with(&server.address()), "{location}");
    let parsed = xen_vnc::ConsoleLocation::parse(&location).unwrap();
    let err = match xen_vnc::tunnel::open(
        &parsed,
        "OpaqueRef:not-a-session",
        None,
        Duration::from_secs(5),
    )
    .await
    {
        Ok(_) => panic!("the tunnel opened with a bad session"),
        Err(e) => e,
    };
    assert!(
        matches!(err, xen_vnc::VncError::Refused { status: 401, .. }),
        "{err:?}"
    );
    assert!(!console::rfb_consoles(&model, &Ref::new(GUEST)).is_empty());
}
