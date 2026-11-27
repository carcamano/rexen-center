//! The README screenshots, drawn by the real window code against the mock
//! XAPI server (`tests/fixtures/records.json`: the hand-written "Lab Pool"
//! with documentation addresses only). Built only with the
//! `readme-screenshots` feature, which renders through wgpu, and ignored
//! unless asked for:
//!
//! ```text
//! REXEN_SCREENSHOTS_DIR=/some/dir cargo test -p xenadmin \
//!   --features readme-screenshots readme_screenshots -- --ignored --nocapture
//! ```
//!
//! It writes one PNG per view into that folder; the README's copies live in
//! `docs/screenshots/` (optimised after rendering).

use std::path::{Path, PathBuf};

use egui_kittest::Harness;
use xapi::{Ref, Vm};
use xapi_mock::MockServer;
use xen_core::console::ConsoleTarget;
use xen_core::{Command, ThemeSetting};

use super::console::connected;
use super::provisioning::{open_combo, press, select, shows, tab};
use super::*;
use crate::console_view::ConsoleKey;
use crate::perf_strings::perf_strings;
use crate::prov_strings::prov_strings;
use crate::tab_strings::tab_strings;

/// The window size of every screenshot, in points.
const SIZE: egui::Vec2 = egui::vec2(1280.0, 800.0);

/// Pixels per point: a little above 1 so the text stays sharp when GitHub
/// scales the image down to its column.
const SCALE: f32 = 1.25;

/// The halted guest started for the console screenshot (the mock makes a
/// VM's console when it starts).
const DB: &str = "OpaqueRef:vm-db";

/// What the guest's text console shows (all of it mock data).
const CONSOLE_TEXT: &str = "\
Debian GNU/Linux 12 db-01 tty1

db-01 login: admin
Password:
Linux db-01 6.1.0-28-amd64 #1 SMP PREEMPT_DYNAMIC Debian 6.1.119-1 x86_64

The programs included with the Debian GNU/Linux system are free software.
Last login: Mon Nov 23 09:12:44 on tty1
admin@db-01:~$ systemctl is-active postgresql
active
admin@db-01:~$ uptime
 10:42:07 up 0 min,  1 user,  load average: 0.31, 0.08, 0.02
admin@db-01:~$ \u{2588}";

/// The app at [`SIZE`] and [`SCALE`], rendered with wgpu, over an
/// in-memory store (as [`app_harness_with`]).
fn screenshot_harness(runtime: &tokio::runtime::Runtime) -> Harness<'static, XenAdminApp> {
    let handle = runtime.handle().clone();
    Harness::builder()
        .with_size(SIZE)
        .with_pixels_per_point(SCALE)
        .with_render_cursor(false)
        .wgpu()
        .build_eframe(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let options = BackendOptions {
                originator: "rexen-readme-screenshots".into(),
                store: Arc::new(Store::default()),
                perf_interval: Duration::from_millis(200),
                ..Default::default()
            };
            XenAdminApp::new(BackendHandle::spawn(
                &handle,
                options,
                Arc::new(move || ctx.request_repaint()),
            ))
        })
}

fn set_theme(h: &mut Harness<'_, XenAdminApp>, theme: ThemeSetting) {
    let mut p = h.state().backend.state().settings.preferences.clone();
    p.theme = theme;
    let wanted = p.clone();
    h.state().backend.send(Command::SetPreferences(p));
    step_until(h, "theme applied", |h| {
        h.state().backend.state().settings.preferences == wanted
    });
    settle(h);
}

/// Lets the window settle for `frames` frames (graphs and layouts that
/// take a few passes), then writes it to `dir/name`.
fn shoot(h: &mut Harness<'_, XenAdminApp>, dir: &Path, name: &str) {
    for _ in 0..10 {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    let image = h.render().expect("render the window");
    let path = dir.join(name);
    image.save(&path).expect("write the PNG");
    println!("wrote {}", path.display());
}

/// [`CONSOLE_TEXT`] in white on black, drawn by egui in a monospace font
/// and returned as rows of RGB pixels `width` wide.
fn console_pixels(width: u16, height: u16) -> Vec<Vec<[u8; 3]>> {
    let mut h = Harness::builder()
        .with_size(egui::vec2(f32::from(width), f32::from(height)))
        .wgpu()
        .build_ui(|ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, egui::Color32::BLACK);
            ui.painter().text(
                egui::pos2(6.0, 6.0),
                egui::Align2::LEFT_TOP,
                CONSOLE_TEXT,
                egui::FontId::monospace(13.0),
                egui::Color32::from_gray(200),
            );
        });
    h.run();
    let image = h.render().expect("render the console text");
    (0..u32::from(height))
        .map(|y| {
            (0..u32::from(width))
                .map(|x| {
                    let p = image.get_pixel(x, y).0;
                    [p[0], p[1], p[2]]
                })
                .collect()
        })
        .collect()
}

/// Paints `rows` onto the guest's screen, one run of equal pixels at a time.
fn paint_console(console: &xen_vnc::testing::MockConsole, rows: &[Vec<[u8; 3]>]) {
    for (y, row) in rows.iter().enumerate() {
        let mut x = 0;
        while x < row.len() {
            let run = row[x..].iter().take_while(|p| **p == row[x]).count();
            console.fill(
                xen_vnc::Rect::new(
                    u16::try_from(x).unwrap(),
                    u16::try_from(y).unwrap(),
                    u16::try_from(run).unwrap(),
                    1,
                ),
                row[x],
            );
            x += run;
        }
    }
}

#[test]
#[ignore = "writes the README screenshots to REXEN_SCREENSHOTS_DIR (mock server only)"]
fn readme_screenshots() {
    let dir =
        PathBuf::from(std::env::var("REXEN_SCREENSHOTS_DIR").expect("set REXEN_SCREENSHOTS_DIR"));
    std::fs::create_dir_all(&dir).expect("the screenshot folder");
    let runtime = runtime();
    let server = runtime.block_on(MockServer::start(false));
    let mut h = screenshot_harness(&runtime);
    let s = strings();
    let t = tab_strings();
    let p = prov_strings();

    // The guest's screen, painted before the console connects.
    let (width, height) = (720, 400);
    let console = server.console(DB);
    console.resize(width, height);
    paint_console(&console, &console_pixels(width, height));

    set_theme(&mut h, ThemeSetting::Light);
    connect_through_dialog(&mut h, &server.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "tree", |h| {
        h.query_by_label("Lab Pool").is_some()
            && first_conn(h).is_some_and(|c| c.status.is_connected())
    });
    let conn = first_conn(&h).unwrap().id;

    // The main window: a running VM's General tab, light and dark.
    click_first(&mut h, "web-01");
    step_until(&mut h, "VM general", |h| {
        h.query_by_label("Debian GNU/Linux 12 (bookworm)").is_some()
    });
    tab(&mut h, t.tab_general);
    shoot(&mut h, &dir, "main-light.png");
    set_theme(&mut h, ThemeSetting::Dark);
    shoot(&mut h, &dir, "main-dark.png");

    // A host's Performance tab (dark).
    select(
        &mut h,
        Selection::Host(conn, Ref::new("OpaqueRef:host-0001")),
    );
    tab(&mut h, perf_strings().tab_performance);
    step_until(&mut h, "graphs", |h| {
        h.query_by_label(perf_strings().graph_cpu).is_some()
    });
    std::thread::sleep(Duration::from_secs(2));
    shoot(&mut h, &dir, "performance-dark.png");

    // The VM console (dark): db-01 started from the toolbar.
    let vm = Ref::<Vm>::new(DB);
    let key: ConsoleKey = (conn, ConsoleTarget::Vm(vm.clone()));
    select(&mut h, Selection::Vm(conn, vm));
    click_first(&mut h, s.action_start);
    step_until(&mut h, "db-01 running", |_| {
        server.vm_power_state(DB) == "Running"
    });
    settle(&mut h);
    if h.state().tab != Tab::Console {
        tab(&mut h, t.tab_console);
    }
    step_until(&mut h, "the guest's screen", |h| connected(h, &key));
    shoot(&mut h, &dir, "console-dark.png");

    // The New VM wizard's installation media page (light), over the
    // pool's General tab.
    set_theme(&mut h, ThemeSetting::Light);
    select(
        &mut h,
        Selection::Pool(conn, Ref::new("OpaqueRef:pool-0001")),
    );
    tab(&mut h, t.tab_general);
    click_first(&mut h, p.new_vm_toolbar);
    step_until(&mut h, "wizard", |h| {
        h.query_by_label(p.title_template).is_some()
    });
    click_last(&mut h, "Debian Bookworm 12");
    settle(&mut h);
    press(&mut h, p.next);
    press(&mut h, p.next);
    h.get_by_label(p.title_media);
    open_combo(&mut h);
    click_last(&mut h, "    debian-12.iso");
    settle(&mut h);
    shoot(&mut h, &dir, "new-vm-wizard-light.png");
    press(&mut h, p.cancel);
    assert!(!shows(&h, p.title_media), "the wizard closed");

    // The About window (light).
    let about_item = format!("{} {}", s.menu_about, branding::PRODUCT_NAME);
    h.get_by_label(s.menu_help).click();
    step_until(&mut h, "help menu", |h| {
        h.query_by_label(&about_item).is_some()
    });
    h.get_by_label(&about_item).click();
    step_until(&mut h, "about window", |h| {
        h.query_by_label(s.about_credits).is_some()
    });
    shoot(&mut h, &dir, "about-light.png");
}
