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
use crate::window_frame::WindowFrame;

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
    framed_screenshot_harness(runtime, None)
}

/// [`screenshot_harness`] with the app's own title bar.
fn framed_screenshot_harness(
    runtime: &tokio::runtime::Runtime,
    frame: Option<WindowFrame>,
) -> Harness<'static, XenAdminApp> {
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
            .with_window_frame(frame)
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

/// The title bar drawn on GNOME, KDE Plasma and Windows, light and dark,
/// with a button under the pointer (and Windows' while the window is
/// inactive): for checking its look by eye.
///
/// ```text
/// REXEN_SCREENSHOTS_DIR=/some/dir cargo test -p xenadmin \
///   --features readme-screenshots title_bar_screenshots -- --ignored --nocapture
/// ```
#[test]
#[ignore = "writes title bar screenshots to REXEN_SCREENSHOTS_DIR"]
fn title_bar_screenshots() {
    use crate::window_frame::{Control, Flavor};
    let dir =
        PathBuf::from(std::env::var("REXEN_SCREENSHOTS_DIR").expect("set REXEN_SCREENSHOTS_DIR"));
    std::fs::create_dir_all(&dir).expect("the screenshot folder");
    let runtime = runtime();
    let s = strings();
    let mut gnome = WindowFrame::new(Flavor::Gnome);
    gnome.layout.right = vec![Control::Minimize, Control::Maximize, Control::Close];
    let mut gnome_left = WindowFrame::new(Flavor::Gnome);
    gnome_left.layout.left = vec![Control::Close, Control::Minimize, Control::Maximize];
    gnome_left.layout.right = vec![Control::Icon];
    let kde = WindowFrame::new(Flavor::Kde);
    let windows = WindowFrame::new(Flavor::Windows);
    let shots = [
        ("gnome", gnome.clone(), ThemeSetting::Light, None),
        ("gnome", gnome, ThemeSetting::Dark, Some(s.window_close)),
        (
            "gnome-left",
            gnome_left,
            ThemeSetting::Light,
            Some(s.window_minimize),
        ),
        (
            "kde",
            kde.clone(),
            ThemeSetting::Light,
            Some(s.window_minimize),
        ),
        ("kde", kde, ThemeSetting::Dark, Some(s.window_close)),
        (
            "windows",
            windows.clone(),
            ThemeSetting::Light,
            Some(s.window_maximize),
        ),
        (
            "windows",
            windows.clone(),
            ThemeSetting::Dark,
            Some(s.window_close),
        ),
        (
            "windows-close",
            windows.clone(),
            ThemeSetting::Light,
            Some(s.window_close),
        ),
        (
            "windows-inactive",
            windows.clone(),
            ThemeSetting::Light,
            None,
        ),
        (
            "windows-inactive",
            windows.clone(),
            ThemeSetting::Dark,
            None,
        ),
        (
            "windows-maximized",
            windows,
            ThemeSetting::Dark,
            Some(s.window_restore),
        ),
    ];
    for (name, frame, theme, hovered) in shots {
        let mut h = framed_screenshot_harness(&runtime, Some(frame));
        set_theme(&mut h, theme);
        let viewport = h
            .input_mut()
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default();
        if name.ends_with("-inactive") {
            viewport.focused = Some(false);
        }
        if name.ends_with("-maximized") {
            viewport.maximized = Some(true);
        }
        h.step();
        if let Some(label) = hovered {
            h.get_by_label(label).hover();
        }
        let theme = if theme == ThemeSetting::Dark {
            "dark"
        } else {
            "light"
        };
        shoot(&mut h, &dir, &format!("title-bar-{name}-{theme}.png"));
    }
}

/// The dialogs in each look (egui's, GNOME's, KDE Plasma's), light and
/// dark, over the window against the mock server: the connect dialog, a
/// confirmation and the New VM wizard. Besides the PNGs it writes
/// `dialogs.tsv`: each file and its dialog's rectangle in pixels, with a
/// little of the window around it, for cropping.
///
/// ```text
/// REXEN_SCREENSHOTS_DIR=/some/dir cargo test -p xenadmin \
///   --features readme-screenshots dialog_screenshots -- --ignored --nocapture
/// ```
#[test]
#[ignore = "writes dialog screenshots to REXEN_SCREENSHOTS_DIR (mock server only)"]
fn dialog_screenshots() {
    use std::fmt::Write as _;

    use crate::ops_strings::ops_strings;
    use crate::window_frame::{Control, Flavor};
    let dir =
        PathBuf::from(std::env::var("REXEN_SCREENSHOTS_DIR").expect("set REXEN_SCREENSHOTS_DIR"));
    std::fs::create_dir_all(&dir).expect("the screenshot folder");
    let runtime = runtime();
    let server = runtime.block_on(MockServer::start(false));
    let s = strings();
    let o = ops_strings();
    let p = prov_strings();
    let t = tab_strings();
    let wizard = fill(p.wizard_title, &[("pool", "Lab Pool")]);

    let mut gnome_left = WindowFrame::new(Flavor::Gnome);
    gnome_left.layout.left = vec![Control::Close, Control::Minimize, Control::Maximize];
    gnome_left.layout.right = vec![];
    let looks = [
        ("egui", None),
        ("gnome", Some(WindowFrame::new(Flavor::Gnome))),
        ("kde", Some(WindowFrame::new(Flavor::Kde))),
    ];
    let mut rects = String::new();
    let mut shoot_dialog = |h: &mut Harness<'_, XenAdminApp>, title: &str, name: String| {
        shoot(h, &dir, &name);
        // The dialog, and the close button's tooltip if it shows.
        let r = h
            .query_all_by_label(strings().dialog_close)
            .fold(h.get_by_label(title).rect(), |r, n| r.union(n.rect()))
            .expand(24.0);
        let px = |v: f32| (v * SCALE).round().max(0.0);
        writeln!(
            rects,
            "{name}\t{}\t{}\t{}\t{}",
            px(r.left()),
            px(r.top()),
            px(r.width()),
            px(r.height())
        )
        .unwrap();
    };

    for (look, frame) in looks {
        for theme in [ThemeSetting::Light, ThemeSetting::Dark] {
            let shade = if theme == ThemeSetting::Dark {
                "dark"
            } else {
                "light"
            };
            let mut h = framed_screenshot_harness(&runtime, frame.clone());
            set_theme(&mut h, theme);

            // The connect dialog, open on start; KDE's close button under
            // the pointer in the dark theme.
            h.get_by_label(s.connect_title);
            if look == "kde" && theme == ThemeSetting::Dark {
                h.get_by_label(s.dialog_close).hover();
            }
            shoot_dialog(
                &mut h,
                s.connect_title,
                format!("connect-{look}-{shade}.png"),
            );
            connect_through_dialog(&mut h, &server.address(), xapi_mock::PASSWORD);
            step_until(&mut h, "tree", |h| {
                h.query_by_label("Lab Pool").is_some()
                    && first_conn(h).is_some_and(|c| c.status.is_connected())
            });
            let conn = first_conn(&h).unwrap().id;

            // A confirmation: shutting down a running VM.
            click_first(&mut h, "web-01");
            step_until(&mut h, "VM general", |h| {
                h.query_by_label("Debian GNU/Linux 12 (bookworm)").is_some()
            });
            click_first(&mut h, s.action_clean_shutdown);
            step_until(&mut h, "confirmation", |h| {
                h.query_by_label(o.confirm_shutdown_vm_title).is_some()
            });
            shoot_dialog(
                &mut h,
                o.confirm_shutdown_vm_title,
                format!("confirm-{look}-{shade}.png"),
            );
            press(&mut h, o.no);
            assert!(!shows(&h, o.confirm_shutdown_vm_title), "answered");

            // The New VM wizard's first page, over the pool's General tab.
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
            shoot_dialog(&mut h, &wizard, format!("new-vm-{look}-{shade}.png"));
            press(&mut h, p.cancel);
            assert!(!shows(&h, p.title_template), "the wizard closed");
        }
    }

    // GNOME with the window's buttons on the left: so is the dialog's.
    let mut h = framed_screenshot_harness(&runtime, Some(gnome_left));
    set_theme(&mut h, ThemeSetting::Light);
    h.get_by_label(s.connect_title);
    shoot_dialog(
        &mut h,
        s.connect_title,
        "connect-gnome-left-light.png".into(),
    );

    std::fs::write(dir.join("dialogs.tsv"), rects).expect("write dialogs.tsv");
}

/// A live host's tabs, Tools > Options, the host's Search tab and a VM's
/// and an SR's Properties, in egui's look and GNOME's and KDE Plasma's,
/// light: for checking the tabs, the navigation pane, the column headings,
/// the page lists and the dialogs' rows and buttons by eye. Besides the
/// PNGs it writes `alignment.tsv`: each dialog's file and rectangle in
/// pixels.
///
/// ```text
/// REXEN_SCREENSHOTS_DIR=/some/dir cargo test -p xenadmin \
///   --features readme-screenshots alignment_screenshots -- --ignored --nocapture
/// ```
#[test]
#[ignore = "writes alignment screenshots to REXEN_SCREENSHOTS_DIR (mock server only)"]
fn alignment_screenshots() {
    use std::fmt::Write as _;

    use crate::window_frame::Flavor;
    let dir =
        PathBuf::from(std::env::var("REXEN_SCREENSHOTS_DIR").expect("set REXEN_SCREENSHOTS_DIR"));
    std::fs::create_dir_all(&dir).expect("the screenshot folder");
    let runtime = runtime();
    let server = runtime.block_on(MockServer::start(false));
    let s = strings();
    let t = tab_strings();
    let looks = [
        ("egui", None),
        ("gnome", Some(WindowFrame::new(Flavor::Gnome))),
        ("kde", Some(WindowFrame::new(Flavor::Kde))),
    ];
    let mut rects = String::new();
    for (look, frame) in looks {
        let mut h = framed_screenshot_harness(&runtime, frame);
        set_theme(&mut h, ThemeSetting::Light);
        connect_through_dialog(&mut h, &server.address(), xapi_mock::PASSWORD);
        step_until(&mut h, "tree", |h| {
            h.query_by_label("Lab Pool").is_some()
                && first_conn(h).is_some_and(|c| c.status.is_connected())
        });
        let conn = first_conn(&h).unwrap().id;

        // A live host: its many tabs.
        select(
            &mut h,
            Selection::Host(conn, Ref::new("OpaqueRef:host-0001")),
        );
        tab(&mut h, t.tab_general);
        shoot(&mut h, &dir, &format!("host-{look}-light.png"));

        // The navigation tabs' More menu, open.
        let more = crate::org_strings::org_strings().nav_more;
        click_first(&mut h, more);
        settle(&mut h);
        shoot(&mut h, &dir, &format!("more-{look}-light.png"));
        click_first(&mut h, more);
        settle(&mut h);

        // Tools > Options.
        h.get_by_label(s.menu_tools).click();
        step_until(&mut h, "tools menu", |h| {
            h.query_by_label(s.menu_options).is_some()
        });
        h.get_by_label(s.menu_options).click();
        step_until(&mut h, "options", |h| {
            h.query_by_label(s.options_restore).is_some()
        });
        let mut record = |h: &mut Harness<'_, XenAdminApp>, title: &str, name: String| {
            shoot(h, &dir, &name);
            let r = h.get_by_label(title).rect().expand(24.0);
            let px = |v: f32| (v * SCALE).round().max(0.0);
            writeln!(
                rects,
                "{name}\t{}\t{}\t{}\t{}",
                px(r.left()),
                px(r.top()),
                px(r.width()),
                px(r.height())
            )
            .unwrap();
        };
        record(&mut h, s.options_title, format!("options-{look}-light.png"));
        press(&mut h, s.close);

        // The host's Search tab: its column headings.
        tab(&mut h, crate::org_strings::org_strings().tab_search);
        step_until(&mut h, "search results", |h| {
            h.query_by_label("xcp-host-02").is_some()
        });
        shoot(&mut h, &dir, &format!("search-{look}-light.png"));

        // A VM's Properties: the page list with summaries.
        click_first(&mut h, "web-01");
        settle(&mut h);
        click_first(&mut h, crate::org_strings::org_strings().properties);
        let vm_title = fill(
            crate::org_strings::org_strings().vm_properties_title,
            &[("name", "web-01")],
        );
        step_until(&mut h, "VM properties", |h| {
            h.query_by_label(&vm_title).is_some()
        });
        record(&mut h, &vm_title, format!("vm-props-{look}-light.png"));
        press(&mut h, s.cancel);

        // An SR's Properties, sized to its contents.
        select(&mut h, Selection::Sr(conn, Ref::new("OpaqueRef:sr-nfs")));
        click_first(&mut h, crate::sr_strings::sr_strings().properties);
        let sr_title = fill(
            crate::sr_strings::sr_strings().properties_title,
            &[("name", "NFS VM storage")],
        );
        step_until(&mut h, "SR properties", |h| {
            h.query_by_label(&sr_title).is_some()
        });
        record(&mut h, &sr_title, format!("sr-props-{look}-light.png"));
        press(&mut h, s.cancel);
    }
    std::fs::write(dir.join("alignment.tsv"), rects).expect("write alignment.tsv");
}
