//! F5: the Console tab through the real window code (egui_kittest): against
//! the mock always, and against a real host when asked (ignored tests,
//! `real_console_*`).

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use egui::{Event, Key, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use xapi::{Ref, Vm, VmPowerState};
use xapi_mock::MockServer;
use xen_core::console::{ConsoleStatus, ConsoleTarget};
use xen_vnc::KeyCode;

use super::*;
use crate::console_keys;
use crate::console_view::{ConsoleKey, framebuffer_point};

pub(super) fn view<'h>(
    h: &'h Harness<'_, XenAdminApp>,
    key: &ConsoleKey,
) -> Option<&'h crate::console_view::ConsoleView> {
    h.state().consoles.get(key)
}

fn status(h: &Harness<'_, XenAdminApp>, key: &ConsoleKey) -> Option<ConsoleStatus> {
    view(h, key).map(|v| v.handle.status())
}

pub(super) fn connected(h: &Harness<'_, XenAdminApp>, key: &ConsoleKey) -> bool {
    matches!(status(h, key), Some(ConsoleStatus::Connected { .. }))
        && view(h, key).is_some_and(|v| v.image.is_some())
}

/// The egui key whose position is `code`.
pub(super) fn egui_key(code: KeyCode) -> Key {
    *Key::ALL
        .iter()
        .find(|k| console_keys::key_code(**k, Some(**k)) == Some(code))
        .unwrap_or_else(|| panic!("no egui key for {code:?}"))
}

pub(super) fn key_event(
    h: &Harness<'_, XenAdminApp>,
    key: Key,
    physical: Option<Key>,
    pressed: bool,
) {
    h.event(Event::Key {
        key,
        physical_key: physical,
        pressed,
        repeat: false,
        modifiers: Modifiers::default(),
    });
}

/// Presses and releases the key at `code`'s position (with Shift if asked),
/// as a keyboard would.
pub(super) fn tap(h: &mut Harness<'_, XenAdminApp>, code: KeyCode, shift: bool) {
    let k = egui_key(code);
    if shift {
        key_event(h, Key::ShiftLeft, Some(Key::ShiftLeft), true);
    }
    key_event(h, k, Some(k), true);
    h.step();
    key_event(h, k, Some(k), false);
    if shift {
        key_event(h, Key::ShiftLeft, Some(Key::ShiftLeft), false);
    }
    h.step();
}

/// Types `text` (US layout in the guest).
pub(super) fn type_us(h: &mut Harness<'_, XenAdminApp>, text: &str) {
    for c in text.chars() {
        let (code, shift) = KeyCode::for_us_char(c).unwrap_or_else(|| panic!("{c:?}"));
        tap(h, code, shift);
    }
}

/// Moves the mouse to `pos` and clicks there (focuses the console).
pub(super) fn click_at(h: &mut Harness<'_, XenAdminApp>, pos: Pos2) {
    h.event(Event::PointerMoved(pos));
    h.step();
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::default(),
    });
    h.step();
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::default(),
    });
    h.step();
}

pub(super) fn image_rect(h: &Harness<'_, XenAdminApp>, key: &ConsoleKey) -> egui::Rect {
    view(h, key)
        .and_then(|v| v.image)
        .expect("the console is drawn")
}

/// Writes the console's framebuffer as a PPM image.
pub(super) fn snapshot(h: &Harness<'_, XenAdminApp>, key: &ConsoleKey, path: &str) -> (u16, u16) {
    let v = view(h, key).expect("console open");
    let state = v.handle.screen().lock();
    let fb = &state.framebuffer;
    let mut ppm = format!("P6\n{} {}\n255\n", fb.width(), fb.height()).into_bytes();
    for p in fb.rgba().as_chunks::<4>().0 {
        ppm.extend_from_slice(&p[..3]);
    }
    std::fs::write(path, ppm).expect("write snapshot");
    (fb.width(), fb.height())
}

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

/// The Console tab of a running VM against the mock: the guest's screen,
/// keys as scancodes (US and ABNT2 positions), Ctrl+Alt+Del (button and
/// shortcut), the mouse on the pixel under it while scaled, clipboard,
/// undock, full screen, and reconnection after a reboot.
#[test]
fn the_console_tab_shows_the_guest_and_sends_input() {
    let runtime = runtime();
    let (server, mut harness) = mock_harness(&runtime);
    let t = tab_strings();
    let conn = first_conn(&harness).unwrap().id;
    let vm = Ref::<Vm>::new("OpaqueRef:vm-0001");
    let key: ConsoleKey = (conn, ConsoleTarget::Vm(vm.clone()));
    let guest = server.console(vm.as_str());
    guest.fill(xen_vnc::Rect::new(600, 440, 40, 40), [0, 0, 255]);

    harness.state_mut().selection = Some(Selection::Vm(conn, vm.clone()));
    settle(&mut harness);
    click_first(&mut harness, t.tab_console);
    step_until(&mut harness, "the guest's screen", |h| {
        connected(h, &key)
            && view(h, &key).is_some_and(|v| {
                v.handle.screen().lock().framebuffer.pixel(620, 460) == Some([0, 0, 255, 255])
            })
    });
    assert_eq!(
        status(&harness, &key),
        Some(ConsoleStatus::Connected { scancodes: true })
    );
    // Scaled to fit by default: the 640x480 screen is drawn smaller here.
    assert!(harness.state().consoles.scale);
    let rect = image_rect(&harness, &key);
    assert!(rect.width() < 600.0, "{rect:?}");
    assert!((rect.width() / rect.height() - 640.0 / 480.0).abs() < 0.01);

    // Click the white square at the top-left (pixels 0..8): the click lands
    // on it, and focuses the console.
    let on_square = rect.min + (rect.size() / egui::vec2(640.0, 480.0)) * 4.0;
    click_at(&mut harness, on_square);
    step_until(&mut harness, "pointer events", |_| {
        guest.pointer_events().iter().any(|e| e.0 == 1)
    });
    for (buttons, x, y) in guest.pointer_events() {
        assert!(x < 8 && y < 8, "({x}, {y}) is not on the square");
        assert!(buttons <= 1);
    }
    // And a point near the bottom-right corner lands on the blue square.
    let on_blue = rect.min + rect.size() * egui::vec2(620.0 / 640.0, 460.0 / 480.0);
    harness.event(Event::PointerMoved(on_blue));
    harness.step();
    step_until(&mut harness, "the move", |_| {
        guest
            .pointer_events()
            .last()
            .is_some_and(|&(_, x, y)| (600..640).contains(&x) && (440..480).contains(&y))
    });
    assert_eq!(
        guest.pointer_events().last().map(|&(_, x, y)| (x, y)),
        Some(framebuffer_point(on_blue, rect, 640, 480))
    );

    // Keys go as scancodes of their position: US "a", "A" (Shift), and
    // the ABNT2 keys `ç` (US `;` position), `\|` (the ISO key) and `/?`
    // (the key egui-winit reports without a position).
    guest.clear_received();
    tap(&mut harness, KeyCode::KeyA, false);
    tap(&mut harness, KeyCode::KeyA, true);
    // ABNT2 `ç`: logical key unknown to egui, so the physical one comes.
    key_event(&harness, Key::Semicolon, Some(Key::Semicolon), true);
    key_event(&harness, Key::Semicolon, Some(Key::Semicolon), false);
    key_event(&harness, Key::Backslash, Some(Key::IntlBackslash), true);
    key_event(&harness, Key::Backslash, Some(Key::IntlBackslash), false);
    key_event(&harness, Key::Slash, None, true);
    key_event(&harness, Key::Slash, None, false);
    harness.step();
    step_until(&mut harness, "the keys", |_| guest.scancodes().len() == 12);
    assert_eq!(
        guest.scancodes(),
        [
            (true, 0x1e),
            (false, 0x1e),
            (true, 0x2a),
            (true, 0x1e),
            (false, 0x1e),
            (false, 0x2a),
            (true, 0x27),
            (false, 0x27),
            (true, 0x56),
            (false, 0x56),
            (true, 0x73),
            (false, 0x73),
        ]
    );
    // Tab and arrows go to the guest, not to egui's focus navigation.
    guest.clear_received();
    tap(&mut harness, KeyCode::Tab, false);
    tap(&mut harness, KeyCode::ArrowDown, false);
    step_until(&mut harness, "tab and arrow", |_| {
        guest.scancodes().len() == 4
    });
    assert_eq!(guest.scancodes()[0], (true, 0x0f));
    assert_eq!(guest.scancodes()[2], (true, 0xd0));

    // Ctrl+Alt+Insert sends Ctrl+Alt+Del; so does the button.
    guest.clear_received();
    key_event(&harness, Key::ControlLeft, Some(Key::ControlLeft), true);
    key_event(&harness, Key::AltLeft, Some(Key::AltLeft), true);
    harness.step();
    key_event(&harness, Key::Insert, Some(Key::Insert), true);
    harness.step();
    key_event(&harness, Key::Insert, Some(Key::Insert), false);
    key_event(&harness, Key::AltLeft, Some(Key::AltLeft), false);
    key_event(&harness, Key::ControlLeft, Some(Key::ControlLeft), false);
    harness.step();
    step_until(&mut harness, "Ctrl+Alt+Del", |_| {
        guest.scancodes().contains(&(true, 0xd3))
    });
    assert!(
        !guest.scancodes().contains(&(true, 0xd2)),
        "Insert was sent"
    );
    guest.clear_received();
    harness.get_by_label(t.con_send_cad).click();
    harness.step();
    step_until(&mut harness, "Ctrl+Alt+Del button", |_| {
        guest.scancodes()
            == [
                (true, 0x1d),
                (true, 0x38),
                (true, 0xd3),
                (false, 0x1d),
                (false, 0x38),
                (false, 0xd3),
            ]
    });

    // Paste: the local clipboard goes to the guest's, then Ctrl+V's V.
    guest.clear_received();
    click_at(&mut harness, rect.center());
    harness.event(Event::Paste("colado".into()));
    harness.step();
    step_until(&mut harness, "pasted", |_| guest.cut_texts() == ["colado"]);
    assert!(guest.scancodes().contains(&(true, 0x2f)));

    // Undock: the console moves to its own window (embedded here).
    harness.get_by_label(t.con_undock).click();
    settle(&mut harness);
    assert!(harness.state().consoles.is_undocked(&key));
    harness.get_by_label(t.con_undocked_here);
    assert_eq!(harness.query_all_by_label(t.con_redock).count(), 2);
    harness
        .get_all_by_label(t.con_redock)
        .last()
        .unwrap()
        .click();
    settle(&mut harness);
    assert!(!harness.state().consoles.is_undocked(&key));

    // Full screen: only the console; the tree is gone until it ends.
    harness.get_by_label(t.con_fullscreen).click();
    settle(&mut harness);
    assert_eq!(harness.state().consoles.fullscreen, Some(key.clone()));
    assert!(harness.query_by_label("guest-10").is_none());
    harness.get_by_label(t.con_exit_fullscreen).click();
    settle(&mut harness);
    assert!(harness.state().consoles.fullscreen.is_none());
    assert!(harness.query_all_by_label("guest-10").next().is_some());

    // A reboot: the session drops and the console reconnects by itself.
    let sessions = view(&harness, &key).unwrap().handle.sessions();
    harness.state().backend.send(Command::Vm {
        connection: conn,
        vm: vm.clone(),
        action: xen_core::VmAction::HardReboot,
    });
    step_until(&mut harness, "reconnected after the reboot", |h| {
        view(h, &key).is_some_and(|v| v.handle.sessions() > sessions) && connected(h, &key)
    });
    assert_eq!(guest.sessions(), 2);

    // Selecting another object closes the console.
    harness.state_mut().selection = None;
    settle(&mut harness);
    step_until(&mut harness, "console closed", |_| {
        guest.active_sessions() == 0
    });
}

/// Starting a VM from the window switches to its Console tab once it runs;
/// a shut-down VM's tab offers to start it; the host's console is dom0's.
#[test]
fn start_switches_to_the_console_and_the_host_console_is_dom0() {
    let runtime = runtime();
    let (server, mut harness) = mock_harness(&runtime);
    let s = strings();
    let t = tab_strings();
    let conn = first_conn(&harness).unwrap().id;
    let vm = Ref::<Vm>::new("OpaqueRef:vm-0010");
    let key: ConsoleKey = (conn, ConsoleTarget::Vm(vm.clone()));

    harness.state_mut().selection = Some(Selection::Vm(conn, vm.clone()));
    settle(&mut harness);
    // The halted VM's console says so and offers to start it.
    click_first(&mut harness, t.tab_console);
    step_until(&mut harness, "halted message", |h| {
        h.query_by_label(t.con_halted_start).is_some()
    });
    click_first(&mut harness, t.tab_general);
    assert_eq!(harness.state().tab, Tab::General);

    // Start from the toolbar: once running, the Console tab shows.
    harness
        .get_all_by_label(s.action_start)
        .next()
        .unwrap()
        .click();
    step_until(&mut harness, "the Console tab", |h| {
        h.state().tab == Tab::Console && connected(h, &key)
    });
    assert_eq!(server.console(vm.as_str()).sessions(), 1);

    // The "Click here to start it" link starts it too.
    harness.state().backend.send(Command::Vm {
        connection: conn,
        vm: vm.clone(),
        action: xen_core::VmAction::HardShutdown,
    });
    step_until(&mut harness, "halted", |h| {
        h.query_by_label(t.con_halted_start).is_some()
    });
    harness.get_by_label(t.con_halted_start).click();
    step_until(&mut harness, "started from the console tab", |h| {
        connected(h, &key)
    });

    // The host's console: dom0's text console, keys as keysyms, nothing
    // sent while only looked at.
    let host = Ref::new("OpaqueRef:host-0001");
    let host_key: ConsoleKey = (conn, ConsoleTarget::Host(host.clone()));
    harness.state_mut().selection = Some(Selection::Host(conn, host));
    settle(&mut harness);
    assert_eq!(harness.state().tab, Tab::Console, "the Console tab is kept");
    step_until(&mut harness, "dom0 console", |h| connected(h, &host_key));
    harness.get_by_label(&fill(t.con_host, &[("name", "xcp-host-01")]));
    assert_eq!(
        status(&harness, &host_key),
        Some(ConsoleStatus::Connected { scancodes: false })
    );
    assert_eq!(view(&harness, &host_key).unwrap().handle.inputs_sent(), 0);
    let dom0 = server.console("OpaqueRef:vm-0011");
    assert!(dom0.scancodes().is_empty() && dom0.keysyms().is_empty());
    assert!(dom0.pointer_events().is_empty());
}

// ---- real host ------------------------------------------------------------

/// Appends a line to the log (and prints it).
pub(super) fn note(log: &mut String, line: String) {
    println!("{line}");
    let _ = writeln!(log, "{line}");
}

/// Snapshots the framebuffer into `dir` until it has not changed for
/// `quiet` (the guest settled at a prompt) or `limit` passed; returns
/// how many shots were taken.
fn watch_boot(
    h: &mut Harness<'_, XenAdminApp>,
    key: &ConsoleKey,
    dir: &str,
    prefix: &str,
    quiet: Duration,
    limit: Duration,
    log: &mut String,
) -> usize {
    let started = Instant::now();
    let mut last_change = Instant::now();
    let mut generation = 0;
    let mut shots = 0;
    let mut next_shot = Instant::now();
    while started.elapsed() < limit {
        h.step();
        std::thread::sleep(Duration::from_millis(100));
        let Some(v) = view(h, key) else { break };
        let g = v.handle.screen().lock().framebuffer.generation();
        if g != generation {
            generation = g;
            last_change = Instant::now();
        }
        if Instant::now() >= next_shot && connected(h, key) {
            shots += 1;
            let (w, hgt) = snapshot(h, key, &format!("{dir}/{prefix}-{shots:02}.ppm"));
            note(
                log,
                format!(
                    "  {prefix}-{shots:02}: {w}x{hgt} at {:.0} s",
                    started.elapsed().as_secs_f64()
                ),
            );
            next_shot = Instant::now() + Duration::from_secs(8);
        }
        if connected(h, key) && last_change.elapsed() > quiet && started.elapsed() > quiet {
            break;
        }
    }
    shots
}

/// F5 acceptance on the real host, through the real window, on the VM
/// named exactly [`FLUXER`] only (refused unless exactly one VM has that
/// name and it is halted). Ignored unless asked for:
///
/// ```text
/// XENADMIN_REAL_HOST=… XENADMIN_REAL_USER=root XENADMIN_REAL_PASSWORD=… \
/// XENADMIN_REAL_FINGERPRINT=AA:BB:… XENADMIN_REAL_LOG=/outside/log.txt \
/// XENADMIN_REAL_SHOTS=/outside/dir cargo test -p xenadmin real_console_fluxer -- --ignored --nocapture
/// ```
///
/// It starts the VM from the toolbar (the Console tab must come up by
/// itself), watches the boot, types a test text at the login prompt
/// (without Enter, so it never logs in) and erases it, moves the mouse,
/// reboots the VM (the console must reconnect), and shuts it down, leaving
/// it halted. Framebuffer snapshots go to `XENADMIN_REAL_SHOTS`.
#[test]
#[ignore = "operates the real VM named \"09 - Ubuntu Fluxer\" (XENADMIN_REAL_* variables)"]
fn real_console_fluxer() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let shots = std::env::var("XENADMIN_REAL_SHOTS").expect("set XENADMIN_REAL_SHOTS");
    let typed = std::env::var("XENADMIN_REAL_TEXT").unwrap_or_else(|_| {
        "xenadmin-rs F5 test: Hello World 0123456789 -=[];',./ !$%^&*()_+{}:\"<>?|~".into()
    });
    let runtime = runtime();
    let mut harness = real_host_harness(&runtime);
    let s = strings();
    let o = ops_strings();
    let t = tab_strings();
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
    assert_eq!(
        conn.model.vms[&vm].power_state,
        VmPowerState::Halted,
        "refusing: {FLUXER:?} is not halted"
    );
    let key: ConsoleKey = (conn.id, ConsoleTarget::Vm(vm.clone()));
    let mut log = String::new();
    note(&mut log, format!("VM {FLUXER:?} found once, halted"));
    let power = |h: &Harness<'_, XenAdminApp>| vm_rec(h, vm.as_str()).power_state.clone();
    let offered = |h: &mut Harness<'_, XenAdminApp>, label: &str| {
        step_for(h, Duration::from_secs(90), |h| {
            h.query_all_by_label(label)
                .next()
                .is_some_and(|n| !n.accesskit_node().is_disabled())
        })
    };

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        harness.state_mut().selection = Some(Selection::Vm(conn.id, vm.clone()));
        settle(&mut harness);
        click_first(&mut harness, t.tab_general);

        // 1. Start from the toolbar; the Console tab comes up by itself.
        let started = Instant::now();
        assert!(offered(&mut harness, s.action_start), "Start not offered");
        settle(&mut harness);
        harness
            .get_all_by_label(s.action_start)
            .next()
            .unwrap()
            .click();
        let switched = step_for(&mut harness, Duration::from_secs(120), |h| {
            h.state().tab == Tab::Console
        });
        note(
            &mut log,
            format!(
                "start: power {:?}; switched to the Console tab by itself: {switched} after {:.1} s",
                power(&harness),
                started.elapsed().as_secs_f64()
            ),
        );
        assert!(switched, "the Console tab did not come up");
        let up = step_for(&mut harness, Duration::from_secs(60), |h| {
            connected(h, &key)
        });
        let v = view(&harness, &key).unwrap();
        let (w, hgt) = {
            let st = v.handle.screen().lock();
            (st.framebuffer.width(), st.framebuffer.height())
        };
        note(
            &mut log,
            format!(
                "console connected={up} {:.1} s after Start: status {:?}, {w}x{hgt}, sessions {}",
                started.elapsed().as_secs_f64(),
                v.handle.status(),
                v.handle.sessions()
            ),
        );
        assert!(up, "the console did not connect");

        // 2. The boot, until the screen settles at the login prompt.
        let n = watch_boot(
            &mut harness,
            &key,
            &shots,
            "boot",
            Duration::from_secs(20),
            // A blinking text cursor never lets the screen settle.
            Duration::from_secs(150),
            &mut log,
        );
        note(
            &mut log,
            format!(
                "boot watched: {n} snapshots, {:.0} s",
                started.elapsed().as_secs_f64()
            ),
        );
        snapshot(&harness, &key, &format!("{shots}/prompt.ppm"));

        // 3. Type at the prompt (no Enter), snapshot, erase.
        let rect = image_rect(&harness, &key);
        click_at(&mut harness, rect.center());
        let focused_inputs = view(&harness, &key).unwrap().handle.inputs_sent();
        type_us(&mut harness, &typed);
        // Two keys by ABNT2 position on this US guest: the `ç` key gives `;`
        // and the `~^` key gives `'`, proving the position is what is sent.
        tap(&mut harness, KeyCode::Semicolon, false);
        tap(&mut harness, KeyCode::Quote, false);
        step_for(&mut harness, Duration::from_secs(3), |_| false);
        snapshot(&harness, &key, &format!("{shots}/typed.ppm"));
        note(
            &mut log,
            format!(
                "typed {} characters (+2 by ABNT2 position) at the prompt, no Enter; status {:?}",
                typed.chars().count(),
                view(&harness, &key).unwrap().handle.status()
            ),
        );
        // 4. The mouse: a few moves and no click (a text console ignores it).
        for (fx, fy) in [(0.25, 0.25), (0.75, 0.5), (0.5, 0.9)] {
            let pos = rect.min + rect.size() * egui::vec2(fx, fy);
            harness.event(Event::PointerMoved(pos));
            harness.step();
            let (x, y) = framebuffer_point(pos, rect, w, hgt);
            note(
                &mut log,
                format!("mouse at screen {pos:?} -> guest pixel ({x}, {y})"),
            );
        }
        for _ in 0..typed.chars().count() + 2 {
            tap(&mut harness, KeyCode::Backspace, false);
        }
        step_for(&mut harness, Duration::from_secs(3), |_| false);
        snapshot(&harness, &key, &format!("{shots}/erased.ppm"));
        note(
            &mut log,
            format!(
                "erased with Backspace; inputs sent while focused: {}",
                view(&harness, &key).unwrap().handle.inputs_sent() - focused_inputs
            ),
        );

        // 5. Reboot from the toolbar: the console reconnects by itself.
        let sessions = view(&harness, &key).unwrap().handle.sessions();
        let rebooted = Instant::now();
        assert!(offered(&mut harness, o.reboot), "Reboot not offered");
        settle(&mut harness);
        harness.get_all_by_label(o.reboot).next().unwrap().click();
        assert!(
            step_for(&mut harness, Duration::from_secs(30), |h| h
                .query_all_by_label(o.yes)
                .next()
                .is_some()),
            "no reboot confirmation"
        );
        click_last(&mut harness, o.yes);
        let back = step_for(&mut harness, Duration::from_secs(240), |h| {
            view(h, &key).is_some_and(|v| v.handle.sessions() > sessions) && connected(h, &key)
        });
        note(
            &mut log,
            format!(
                "reboot: console reconnected={back} after {:.1} s, sessions {} -> {}, status {:?}",
                rebooted.elapsed().as_secs_f64(),
                sessions,
                view(&harness, &key).unwrap().handle.sessions(),
                view(&harness, &key).unwrap().handle.status()
            ),
        );
        assert!(back, "the console did not reconnect after the reboot");
        let n = watch_boot(
            &mut harness,
            &key,
            &shots,
            "reboot",
            Duration::from_secs(20),
            // A blinking text cursor never lets the screen settle.
            Duration::from_secs(150),
            &mut log,
        );
        note(&mut log, format!("reboot watched: {n} snapshots"));

        // 6. Shut down from the toolbar; the console says so.
        assert!(
            offered(&mut harness, s.action_clean_shutdown),
            "Shut Down not offered"
        );
        settle(&mut harness);
        harness
            .get_all_by_label(s.action_clean_shutdown)
            .next()
            .unwrap()
            .click();
        assert!(
            step_for(&mut harness, Duration::from_secs(30), |h| h
                .query_all_by_label(o.yes)
                .next()
                .is_some()),
            "no shut down confirmation"
        );
        click_last(&mut harness, o.yes);
        let halted = step_for(&mut harness, Duration::from_secs(240), |h| {
            power(h) == VmPowerState::Halted
                && status(h, &key) == Some(ConsoleStatus::NotRunning(VmPowerState::Halted))
        });
        settle(&mut harness);
        let message = harness.query_by_label(t.con_halted_start).is_some()
            || harness.query_by_label(t.con_halted).is_some();
        note(
            &mut log,
            format!("shut down: halted={halted}, console message shown={message}"),
        );
        assert!(halted && message);
        only_on(&harness, &vm);
    }));

    // Leave the VM halted, whatever happened.
    if power(&harness) != VmPowerState::Halted {
        note(
            &mut log,
            format!("cleanup: forcing a shut down from {:?}", power(&harness)),
        );
        harness.state().backend.send(Command::Vm {
            connection: conn.id,
            vm: vm.clone(),
            action: xen_core::VmAction::HardShutdown,
        });
        step_for(&mut harness, Duration::from_secs(120), |h| {
            power(h) == VmPowerState::Halted
        });
    }
    note(&mut log, format!("end state: {:?}", power(&harness)));
    harness
        .state()
        .backend
        .shutdown_blocking(Duration::from_secs(5));
    note(&mut log, "logged out".into());
    let host = std::env::var("XENADMIN_REAL_HOST").unwrap_or_default();
    let log = if host.is_empty() {
        log
    } else {
        log.replace(&host, "<host>")
    };
    std::fs::write(&log_path, &log).unwrap();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// The host's (dom0) console on the real host, view-only: shown in the
/// Console tab, with nothing sent to it (no key, mouse, Ctrl+Alt+Del or
/// clipboard). Ignored unless asked for (same variables as
/// [`real_console_fluxer`]; `cargo test -p xenadmin real_console_dom0`).
#[test]
#[ignore = "opens the real host's console view-only (XENADMIN_REAL_* variables)"]
fn real_console_dom0() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let shots = std::env::var("XENADMIN_REAL_SHOTS").expect("set XENADMIN_REAL_SHOTS");
    let runtime = runtime();
    let mut harness = real_host_harness(&runtime);
    let t = tab_strings();
    let state = harness.state().backend.state();
    let conn = state.connections[0].clone();
    assert_eq!(
        conn.model.hosts.len(),
        1,
        "expected the single-host lab pool"
    );
    let host = conn.model.hosts.keys().next().unwrap().clone();
    let key: ConsoleKey = (conn.id, ConsoleTarget::Host(host.clone()));
    let mut log = String::new();
    harness.state_mut().selection = Some(Selection::Host(conn.id, host));
    settle(&mut harness);
    // Only the tab button is clicked; the console itself never is.
    click_first(&mut harness, t.tab_console);
    let started = Instant::now();
    let up = step_for(&mut harness, Duration::from_secs(60), |h| {
        connected(h, &key)
    });
    step_for(&mut harness, Duration::from_secs(3), |_| false);
    let v = view(&harness, &key).unwrap();
    let (w, hgt) = snapshot(&harness, &key, &format!("{shots}/dom0.ppm"));
    note(
        &mut log,
        format!(
            "dom0 console connected={up} after {:.1} s: status {:?}, {w}x{hgt}, desktop {:?}",
            started.elapsed().as_secs_f64(),
            v.handle.status(),
            v.handle.screen().lock().desktop_name
        ),
    );
    let sent = v.handle.inputs_sent();
    note(&mut log, format!("inputs sent to dom0: {sent}"));
    assert!(up && sent == 0);
    harness.state_mut().selection = None;
    settle(&mut harness);
    harness
        .state()
        .backend
        .shutdown_blocking(Duration::from_secs(5));
    note(&mut log, "closed and logged out".into());
    std::fs::write(&log_path, &log).unwrap();
}
