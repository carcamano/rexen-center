//! macOS in the window, against the mock, on every platform: the menus in
//! the system's menu bar (`crate::menu_bar`, here through a fake menu bar)
//! and the toolbar in the title bar (`crate::unified_title_bar`). The
//! window's own menu row goes; each item of the menu bar does what the
//! window's menus did, and the Pool, VM, Server and Storage menus follow
//! the selection.

use std::sync::{Arc, Mutex};

use egui::{Key, KeyboardShortcut, Modifiers, ViewportCommand};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use xapi_mock::MockServer;
use xen_core::{Store, VmAction};

use super::*;
use crate::menu_bar::{Item, Menu, NativeMenuBar, Role};
use crate::unified_title_bar::{Geometry, UnifiedTitleBar};
use crate::window_frame::TitleAction;

/// The menu bar the window last described, and the items chosen in it.
#[derive(Default)]
struct Bar {
    menus: Vec<Menu<MenuCommand>>,
    chosen: Vec<MenuCommand>,
}

#[derive(Clone, Default)]
struct FakeMenuBar(Arc<Mutex<Bar>>);

impl NativeMenuBar<MenuCommand> for FakeMenuBar {
    fn update(&mut self, menus: Vec<Menu<MenuCommand>>) {
        self.0.lock().unwrap().menus = menus;
    }

    fn take_chosen(&mut self) -> Vec<MenuCommand> {
        std::mem::take(&mut self.0.lock().unwrap().chosen)
    }
}

fn label(item: &Item<MenuCommand>) -> Option<&str> {
    match item {
        Item::Command { label, .. }
        | Item::Submenu { label, .. }
        | Item::Note(label)
        | Item::Standard { label, .. } => Some(label),
        Item::Separator => None,
    }
}

impl FakeMenuBar {
    fn titles(&self) -> Vec<String> {
        let bar = self.0.lock().unwrap();
        bar.menus.iter().map(|m| m.title.clone()).collect()
    }

    fn menu(&self, title: &str) -> Menu<MenuCommand> {
        let bar = self.0.lock().unwrap();
        bar.menus
            .iter()
            .find(|m| m.title == title)
            .unwrap_or_else(|| panic!("no menu {title:?}"))
            .clone()
    }

    /// The labels of menu `title`'s items (`-` for separators).
    fn labels(&self, title: &str) -> Vec<String> {
        self.menu(title)
            .items
            .iter()
            .map(|i| label(i).unwrap_or("-").to_owned())
            .collect()
    }

    /// The item at `path`: a menu's title, then labels down submenus.
    fn find(&self, path: &[&str]) -> Option<Item<MenuCommand>> {
        let (title, labels) = path.split_first().expect("a menu");
        let mut items = self.menu(title).items;
        for (i, wanted) in labels.iter().enumerate() {
            let item = items.into_iter().find(|it| label(it) == Some(wanted))?;
            if i + 1 == labels.len() {
                return Some(item);
            }
            let Item::Submenu { items: inner, .. } = item else {
                return None;
            };
            items = inner;
        }
        None
    }

    fn item(&self, path: &[&str]) -> Item<MenuCommand> {
        self.find(path)
            .unwrap_or_else(|| panic!("no {path:?} in {:?}", self.labels(path[0])))
    }

    fn enabled(&self, path: &[&str]) -> bool {
        matches!(
            self.find(path),
            Some(Item::Command { enabled: true, .. } | Item::Submenu { enabled: true, .. })
        )
    }

    /// Chooses the item at `path`, as a click in the menu bar does.
    fn choose(&self, path: &[&str]) {
        match self.item(path) {
            Item::Command {
                enabled: true,
                command,
                ..
            } => self.0.lock().unwrap().chosen.push(command),
            other => panic!("{path:?} cannot be chosen: {other:?}"),
        }
    }
}

const TITLE_BAR: Geometry = Geometry {
    height: 30.0,
    buttons_right: 70.0,
    buttons_middle: 15.0,
};

/// The app with `bar` as its menu bar and the toolbar in the title bar.
fn mac_harness(
    runtime: &tokio::runtime::Runtime,
    bar: &FakeMenuBar,
) -> Harness<'static, XenAdminApp> {
    let bar = bar.clone();
    app_harness_wrapped(runtime, Arc::new(Store::default()), move |app| {
        app.with_native_menu(Box::new(bar))
            .with_unified_title_bar(UnifiedTitleBar::new(
                || Some(TITLE_BAR),
                || TitleAction::ToggleMaximize,
            ))
    })
}

fn commands(h: &Harness<'_, XenAdminApp>) -> Vec<ViewportCommand> {
    h.output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

#[test]
fn the_menus_move_to_the_menu_bar() {
    let runtime = runtime();
    let bar = FakeMenuBar::default();
    let mut h = mac_harness(&runtime, &bar);
    let s = strings();
    h.step();
    h.get_by_label(s.cancel).click();
    h.step();

    // No menu row in the window; the toolbar is in the title bar, right of
    // the window buttons.
    for menu in [s.menu_file, s.menu_tools, s.menu_help] {
        assert!(h.query_by_label(menu).is_none(), "{menu}");
    }
    let connect = h.get_by_label(s.menu_connect).rect();
    assert!(connect.min.x > TITLE_BAR.buttons_right, "{connect:?}");
    assert!(
        (connect.center().y - TITLE_BAR.buttons_middle).abs() < 0.5,
        "{connect:?}"
    );

    // Apple's order: the application menu, File, Edit, View, the window's
    // menus, Window, Help. No Tools.
    let product = branding::PRODUCT_NAME;
    assert_eq!(
        bar.titles(),
        [
            product,
            s.menu_file,
            s.menu_edit,
            s.menu_view,
            pool_strings().menu_pool,
            s.menu_vm,
            ops_strings().menu_server,
            sr_strings().menu_storage,
            s.menu_window,
            s.menu_help,
        ]
    );
    let roles: Vec<Role> = {
        let b = bar.0.lock().unwrap();
        b.menus.iter().map(|m| m.role).collect()
    };
    assert_eq!(roles[0], Role::App);
    assert_eq!(roles[8], Role::Window);
    assert_eq!(roles[9], Role::Help);
    let about = format!("About {product}");
    let quit = format!("Quit {product}");
    assert_eq!(
        bar.labels(product),
        [
            about.as_str(),
            crate::update_strings::update_strings().menu_check,
            "-",
            s.mac_settings,
            "-",
            s.mac_services,
            "-",
            &format!("Hide {product}"),
            s.mac_hide_others,
            s.mac_show_all,
            "-",
            &quit,
        ]
    );
    let shortcut = |path: &[&str]| match bar.item(path) {
        Item::Command { shortcut, .. } => shortcut,
        other => panic!("{other:?}"),
    };
    let cmd = |key| Some(KeyboardShortcut::new(Modifiers::COMMAND, key));
    assert_eq!(shortcut(&[product, s.mac_settings]), cmd(Key::Comma));
    assert_eq!(shortcut(&[product, &quit]), cmd(Key::Q));
    assert_eq!(shortcut(&[s.menu_edit, s.edit_copy]), cmd(Key::C));
    assert_eq!(
        bar.labels(s.menu_window),
        [
            s.window_minimize,
            s.window_zoom,
            "-",
            s.window_bring_all_to_front
        ]
    );
    assert!(bar.labels(s.menu_help).is_empty());
    // Not connected: nothing to act on.
    assert!(!bar.enabled(&[s.menu_file, s.menu_disconnect]));
    assert_eq!(bar.labels(s.menu_vm), [ops_strings().host_menu_empty]);

    // Settings… is Tools > Options, About is Help > About.
    bar.choose(&[product, s.mac_settings]);
    step_until(&mut h, "options", |h| {
        h.query_by_label(s.options_theme_dark).is_some()
    });
    bar.choose(&[product, &about]);
    step_until(&mut h, "about", |h| {
        h.query_by_label(s.about_credits_heading).is_some()
    });

    // View > Events hides the Events panel, and the item shows it.
    let checked = |bar: &FakeMenuBar| match bar.item(&[s.menu_view, s.menu_events]) {
        Item::Command { checked, .. } => checked,
        other => panic!("{other:?}"),
    };
    assert_eq!(checked(&bar), Some(true));
    bar.choose(&[s.menu_view, s.menu_events]);
    step_until(&mut h, "events hidden", |h| !h.state().events_open);
    h.step();
    assert_eq!(checked(&bar), Some(false));

    // A text field has the keyboard: Edit's items are usable.
    assert!(!bar.enabled(&[s.menu_edit, s.edit_paste]));
    bar.choose(&[s.menu_file, s.menu_connect]);
    step_until(&mut h, "connect dialog", |h| {
        h.query_by_label(s.connect_title).is_some()
    });
    h.step();
    assert!(bar.enabled(&[s.menu_edit, s.edit_paste]));
    // Select All selects the focused field's text: typing replaces it.
    h.get_by_label(s.connect_server).type_text("old-name");
    h.step();
    bar.choose(&[s.menu_edit, s.edit_select_all]);
    h.step();
    h.get_by_label(s.connect_server).type_text("xcp");
    h.step();
    assert_eq!(h.state().connect.server, "xcp");
    bar.choose(&[s.menu_edit, s.edit_paste]);
    h.step();
    assert!(
        commands(&h).contains(&ViewportCommand::RequestPaste),
        "{:?}",
        commands(&h)
    );

    // Quit closes the window, which logs out.
    bar.choose(&[product, &quit]);
    h.step();
    assert!(commands(&h).contains(&ViewportCommand::Close));
}

#[test]
fn the_menu_bar_follows_the_selection() {
    let runtime = runtime();
    let server = runtime.block_on(MockServer::start(false));
    let bar = FakeMenuBar::default();
    let mut h = mac_harness(&runtime, &bar);
    let s = strings();
    let o = ops_strings();
    connect_through_dialog(&mut h, &server.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "tree", |h| h.query_by_label("Lab Pool").is_some());
    h.step();
    // The pool's menu needs no selection; the VM menu does.
    assert!(!bar.labels(pool_strings().menu_pool).is_empty());
    assert_eq!(bar.labels(s.menu_vm), [o.host_menu_empty]);

    // A halted VM: Start, not Shut Down.
    let start = i18n::action_label(&VmAction::Start);
    let shut_down = i18n::action_label(&VmAction::CleanShutdown);
    click_first(&mut h, "db-01");
    h.step();
    assert!(bar.enabled(&[s.menu_vm, start]));
    assert!(!bar.enabled(&[s.menu_vm, shut_down]));
    assert!(bar.enabled(&[s.menu_file, s.menu_disconnect]));

    // Start on Server lists the servers once the server agrees.
    step_until(&mut h, "start on xcp-host-01 offered", |_| {
        bar.enabled(&[s.menu_vm, o.start_on_server, "xcp-host-01"])
    });
    assert!(
        server
            .calls()
            .iter()
            .any(|c| c == "VM.assert_can_boot_here")
    );
    bar.choose(&[s.menu_vm, o.start_on_server, "xcp-host-01"]);
    step_until(&mut h, "db-01 running on xcp-host-01", |h| {
        let vm = vm_rec(h, "OpaqueRef:vm-db");
        vm.power_state == VmPowerState::Running && vm.resident_on.as_str() == "OpaqueRef:host-0001"
    });

    // Running: Shut Down, which asks first, as the window's menu does.
    step_until(&mut h, "shut down offered", |_| {
        bar.enabled(&[s.menu_vm, shut_down]) && !bar.enabled(&[s.menu_vm, start])
    });
    bar.choose(&[s.menu_vm, shut_down]);
    step_until(&mut h, "confirmation", |h| {
        h.query_by_label(o.yes).is_some()
    });
    click_last(&mut h, o.yes);
    step_until(&mut h, "db-01 halted", |h| {
        vm_rec(h, "OpaqueRef:vm-db").power_state == VmPowerState::Halted
    });

    // Storage > New SR… opens the wizard.
    let sr = sr_strings();
    let new_sr = bar.labels(sr.menu_storage)[0].clone();
    bar.choose(&[sr.menu_storage, &new_sr]);
    step_until(&mut h, "New SR wizard", |h| {
        h.query_by_label(sr.type_title).is_some()
    });

    // File > Disconnect.
    bar.choose(&[s.menu_file, s.menu_disconnect]);
    step_until(&mut h, "disconnected", |h| first_conn(h).is_none());
}
