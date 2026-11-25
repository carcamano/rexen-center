//! RFB sessions against the fake guest console: handshake, every encoding
//! it sends, key events (scancodes and keysyms), pointer, clipboard, bell,
//! desktop resize, the cursor, and the end of a session.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use xen_vnc::keymap::{CTRL_ALT_DEL, KeyCode};
use xen_vnc::protocol::{ClientMessage, PixelFormat, encoding};
use xen_vnc::testing::MockConsole;
use xen_vnc::{Input, Rect, Screen, ServerEvent, client};

struct Session {
    screen: Arc<Screen>,
    inputs: mpsc::UnboundedSender<Input>,
    events: Arc<Mutex<Vec<ServerEvent>>>,
    task: JoinHandle<xen_vnc::Result<()>>,
}

async fn start(console: &MockConsole) -> Session {
    let addr = console.listen().await;
    let tcp = TcpStream::connect(addr).await.unwrap();
    let screen = Screen::new();
    let (inputs, mut rx) = mpsc::unbounded_channel();
    let events: Arc<Mutex<Vec<ServerEvent>>> = Arc::default();
    let task = {
        let screen = screen.clone();
        let events = events.clone();
        tokio::spawn(async move {
            let on_event = move |e: ServerEvent| events.lock().unwrap().push(e);
            client::run(tcp, &screen, &mut rx, &on_event).await
        })
    };
    Session {
        screen,
        inputs,
        events,
        task,
    }
}

async fn wait_until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

fn rgb(screen: &Screen, x: u16, y: u16) -> [u8; 3] {
    let p = screen.lock().framebuffer.pixel(x, y).unwrap_or_default();
    [p[0], p[1], p[2]]
}

#[tokio::test]
async fn the_screen_follows_the_guest_with_every_encoding() {
    for (forced, rfb_3_8) in [
        (None, false),
        (Some(encoding::HEXTILE), false),
        (Some(encoding::RAW), true),
    ] {
        let console = MockConsole::new(100, 70);
        console.force_encoding(forced);
        console.set_rfb_3_8(rfb_3_8);
        let s = start(&console).await;
        wait_until("the first update", || {
            rgb(&s.screen, 3, 3) == [255, 255, 255]
        })
        .await;
        assert_eq!(rgb(&s.screen, 50, 50), console.pixel(50, 50), "{forced:?}");
        assert_eq!(s.screen.lock().desktop_name, "mock guest");
        // The client asked for its pixel format and encodings first.
        let received = console.received();
        assert_eq!(
            received[0],
            ClientMessage::SetPixelFormat(PixelFormat::BGRX)
        );
        assert!(matches!(&received[1], ClientMessage::SetEncodings(l) if l[0] == encoding::ZRLE));
        assert!(matches!(
            received[2],
            ClientMessage::UpdateRequest {
                incremental: false,
                rect: Rect {
                    width: 100,
                    height: 70,
                    ..
                }
            }
        ));
        // Incremental updates follow the guest (twice: the ZRLE stream
        // carries on between payloads).
        console.fill(Rect::new(70, 60, 30, 10), [200, 10, 20]);
        wait_until("the red area", || rgb(&s.screen, 99, 69) == [200, 10, 20]).await;
        console.fill(Rect::new(0, 0, 100, 2), [0, 128, 0]);
        wait_until("the green strip", || rgb(&s.screen, 50, 1) == [0, 128, 0]).await;
        assert_eq!(rgb(&s.screen, 3, 3), [255, 255, 255]);
        assert!(console.received().iter().any(|m| matches!(
            m,
            ClientMessage::UpdateRequest {
                incremental: true,
                ..
            }
        )));
        assert!(
            s.events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, ServerEvent::Connected(init) if init.version == (3, if rfb_3_8 { 8 } else { 3 })))
        );
        s.task.abort();
    }
}

#[tokio::test]
async fn keys_are_scancodes_once_acknowledged_and_keysyms_otherwise() {
    let console = MockConsole::new(32, 32);
    let s = start(&console).await;
    wait_until("the QEMU Extended Key acknowledgement", || {
        s.screen.lock().scancodes
    })
    .await;
    for (key, down) in [(KeyCode::KeyA, true), (KeyCode::KeyA, false)] {
        s.inputs
            .send(Input::Key {
                down,
                scancode: key.scancode(),
                keysym: key.keysym(false),
            })
            .unwrap();
    }
    for (key, down) in CTRL_ALT_DEL {
        s.inputs
            .send(Input::Key {
                down,
                scancode: key.scancode(),
                keysym: key.keysym(false),
            })
            .unwrap();
    }
    wait_until("8 key events", || console.scancodes().len() == 8).await;
    assert_eq!(
        console.scancodes(),
        [
            (true, 0x1e),
            (false, 0x1e),
            (true, 0x1d),
            (true, 0x38),
            (true, 0xd3),
            (false, 0x1d),
            (false, 0x38),
            (false, 0xd3)
        ]
    );
    assert!(console.keysyms().is_empty());
    s.task.abort();

    // A text console (no QEMU Extended Key Event): keysyms.
    let console = MockConsole::new(32, 32);
    console.set_ext_key(false);
    let s = start(&console).await;
    wait_until("the first update", || {
        rgb(&s.screen, 1, 1) == [255, 255, 255]
    })
    .await;
    assert!(!s.screen.lock().scancodes);
    for (key, down) in CTRL_ALT_DEL {
        s.inputs
            .send(Input::Key {
                down,
                scancode: key.scancode(),
                keysym: key.keysym(false),
            })
            .unwrap();
    }
    // A key without a keysym is dropped.
    s.inputs
        .send(Input::Key {
            down: true,
            scancode: 0x70,
            keysym: 0,
        })
        .unwrap();
    wait_until("6 keysyms", || console.keysyms().len() == 6).await;
    assert_eq!(
        console.keysyms(),
        [
            (true, 0xffe3),
            (true, 0xffe9),
            (true, 0xffff),
            (false, 0xffe3),
            (false, 0xffe9),
            (false, 0xffff)
        ]
    );
    assert!(console.scancodes().is_empty());
    s.task.abort();
}

#[tokio::test]
async fn pointer_clipboard_bell_and_cursor() {
    let console = MockConsole::new(64, 48);
    let s = start(&console).await;
    wait_until("the first update", || {
        rgb(&s.screen, 1, 1) == [255, 255, 255]
    })
    .await;
    s.inputs
        .send(Input::Pointer {
            buttons: 1,
            x: 10,
            y: 20,
        })
        .unwrap();
    // Outside the screen: clamped, as the original's PointerEvent.
    s.inputs
        .send(Input::Pointer {
            buttons: 0,
            x: 500,
            y: 900,
        })
        .unwrap();
    s.inputs
        .send(Input::CutText("olá\r\nmundo €".into()))
        .unwrap();
    wait_until("pointer and clipboard", || {
        console.pointer_events().len() == 2 && console.cut_texts().len() == 1
    })
    .await;
    assert_eq!(console.pointer_events(), [(1, 10, 20), (0, 63, 47)]);
    assert_eq!(console.cut_texts(), ["olá\nmundo ?"]);

    console.cut_text("from the guest ç");
    console.bell();
    wait_until("clipboard and bell from the guest", || {
        let events = s.events.lock().unwrap();
        events.contains(&ServerEvent::CutText("from the guest ç".into()))
            && events.contains(&ServerEvent::Bell)
    })
    .await;

    let cursor = s.screen.lock().cursor.clone().expect("a cursor shape");
    assert_eq!((cursor.width, cursor.height), (4, 4));
    assert_eq!((cursor.hotspot_x, cursor.hotspot_y), (1, 1));
    assert_eq!(&cursor.rgba[..4], &[255, 0, 0, 0]);
    assert_eq!(&cursor.rgba[4..8], &[255, 0, 0, 255]);
    s.task.abort();
}

#[tokio::test]
async fn desktop_resize_and_refresh() {
    let console = MockConsole::new(64, 48);
    let s = start(&console).await;
    wait_until("the first update", || {
        rgb(&s.screen, 1, 1) == [255, 255, 255]
    })
    .await;
    console.resize(320, 200);
    wait_until("the new size", || {
        s.screen.lock().framebuffer.width() == 320
    })
    .await;
    wait_until("the new screen", || rgb(&s.screen, 300, 150) == [0, 64, 0]).await;
    assert!(s.events.lock().unwrap().contains(&ServerEvent::Resized {
        width: 320,
        height: 200
    }));
    // After the resize the client asks for the whole new screen.
    wait_until("a full request at the new size", || {
        console.received().iter().any(|m| {
            matches!(
                m,
                ClientMessage::UpdateRequest {
                    incremental: false,
                    rect: Rect {
                        width: 320,
                        height: 200,
                        ..
                    }
                }
            )
        })
    })
    .await;
    console.clear_received();
    s.inputs.send(Input::Refresh).unwrap();
    wait_until("a full request", || {
        console.received().iter().any(|m| {
            matches!(
                m,
                ClientMessage::UpdateRequest {
                    incremental: false,
                    ..
                }
            )
        })
    })
    .await;
    s.task.abort();
}

#[tokio::test]
async fn a_dropped_session_is_an_error_and_closing_the_inputs_ends_cleanly() {
    let console = MockConsole::new(16, 16);
    let s = start(&console).await;
    wait_until("the first update", || {
        rgb(&s.screen, 1, 1) == [255, 255, 255]
    })
    .await;
    console.disconnect_all();
    let result = tokio::time::timeout(Duration::from_secs(5), s.task)
        .await
        .expect("the session ends")
        .unwrap();
    assert!(result.is_err(), "{result:?}");
    wait_until("no session left", || console.active_sessions() == 0).await;

    let s = start(&console).await;
    wait_until("the first update", || {
        rgb(&s.screen, 1, 1) == [255, 255, 255]
    })
    .await;
    assert_eq!(console.sessions(), 2);
    drop(s.inputs);
    let result = tokio::time::timeout(Duration::from_secs(5), s.task)
        .await
        .expect("the session ends")
        .unwrap();
    assert!(result.is_ok(), "{result:?}");
}
