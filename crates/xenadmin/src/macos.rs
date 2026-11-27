//! AppKit for macOS: the menu bar through `muda`, and the main window's
//! title bar and double-click setting through `objc2`. Safe Rust only (the
//! workspace forbids `unsafe`); compiled on macOS only.

use std::sync::mpsc;

use egui::{Key, KeyboardShortcut};
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{CheckMenuItem, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSWindow, NSWindowButton, NSWindowStyleMask};
use objc2_foundation::{NSString, NSUserDefaults};

use crate::menu_bar::{Item, ItemId, Menu, NativeMenuBar, Rebuild, Role, Shown, Standard};
use crate::unified_title_bar::{self, Geometry};
use crate::window_frame::TitleAction;

// ---- menu bar ---------------------------------------------------------------

/// The menu bar through `muda`: AppKit's menus, set as the application's
/// main menu (in place of winit's) on the first update.
pub struct MudaMenuBar<C> {
    bar: muda::Menu,
    /// The bar's menus as built, left to right.
    menus: Vec<Submenu>,
    shown: Shown<C>,
    chosen: mpsc::Receiver<MenuId>,
}

impl<C> MudaMenuBar<C> {
    /// A menu bar whose chosen items wake `ctx`. Created on the main thread
    /// once the application has started (from eframe's app creator).
    pub fn new(ctx: &egui::Context) -> Self {
        let (sender, chosen) = mpsc::channel();
        let ctx = ctx.clone();
        // AppKit calls this on the main thread, from the menu's action.
        muda::MenuEvent::set_event_handler(Some(move |event: muda::MenuEvent| {
            // The receiver only goes away with the window.
            let _ = sender.send(event.id);
            ctx.request_repaint();
        }));
        Self {
            bar: muda::Menu::new(),
            menus: Vec::new(),
            shown: Shown::default(),
            chosen,
        }
    }

    /// Builds every menu, puts them in the bar and the bar in the
    /// application.
    fn build_bar(&mut self) {
        for _ in 0..self.bar.items().len() {
            self.bar.remove_at(0);
        }
        self.menus.clear();
        for i in 0..self.shown.len() {
            let submenu = Submenu::new("", true);
            if let Err(e) = self.bar.append(&submenu) {
                tracing::warn!(error = %e, "menu bar");
            }
            self.menus.push(submenu);
            self.fill(i);
        }
        self.bar.init_for_nsapp();
        // Once the menus are in the application's main menu.
        for (i, submenu) in self.menus.iter().enumerate() {
            match self.shown.menu(i).map(|(menu, _)| menu.role) {
                Some(Role::Window) => submenu.set_as_windows_menu_for_nsapp(),
                Some(Role::Help) => submenu.set_as_help_menu_for_nsapp(),
                _ => {}
            }
        }
    }

    /// Gives menu `index` its title and builds its items again.
    fn fill(&self, index: usize) {
        let (Some((menu, build)), Some(submenu)) = (self.shown.menu(index), self.menus.get(index))
        else {
            return;
        };
        submenu.set_text(&menu.title);
        // Only the items built here: AppKit keeps its own (the Window
        // menu's list of windows).
        for _ in 0..submenu.items().len() {
            submenu.remove_at(0);
        }
        let mut next = 0;
        for item in &menu.items {
            append(submenu, item, index, build, &mut next);
        }
    }
}

impl<C: Clone> NativeMenuBar<C> for MudaMenuBar<C> {
    fn update(&mut self, menus: Vec<Menu<C>>) {
        match self.shown.update(menus) {
            Rebuild::Nothing => {}
            Rebuild::Menus(changed) => {
                for i in changed {
                    self.fill(i);
                }
            }
            Rebuild::Bar => self.build_bar(),
        }
    }

    fn take_chosen(&mut self) -> Vec<C> {
        self.chosen
            .try_iter()
            .filter_map(|id| ItemId::parse(&id.0))
            .filter_map(|id| self.shown.command(id).cloned())
            .collect()
    }
}

/// Appends `item` to `parent`, numbering the command items as
/// [`Menu::split`] does.
fn append(parent: &Submenu, item: &Item<()>, menu: usize, build: u64, next: &mut usize) {
    let result = match item {
        Item::Command {
            label,
            enabled,
            checked,
            shortcut,
            command: (),
        } => {
            let id = ItemId {
                menu,
                build,
                index: *next,
            }
            .to_string();
            *next += 1;
            let accelerator = shortcut.as_ref().and_then(accelerator);
            match checked {
                Some(checked) => parent.append(&CheckMenuItem::with_id(
                    id,
                    label,
                    *enabled,
                    *checked,
                    accelerator,
                )),
                None => parent.append(&MenuItem::with_id(id, label, *enabled, accelerator)),
            }
        }
        Item::Submenu {
            label,
            enabled,
            items,
        } => {
            let submenu = Submenu::new(label, *enabled);
            for child in items {
                append(&submenu, child, menu, build, next);
            }
            parent.append(&submenu)
        }
        Item::Note(label) => parent.append(&MenuItem::new(label, false, None)),
        Item::Separator => parent.append(&PredefinedMenuItem::separator()),
        Item::Standard { item, label } => parent.append(&standard(*item, label)),
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, "menu item");
    }
}

fn standard(item: Standard, label: &str) -> PredefinedMenuItem {
    let text = Some(label);
    match item {
        Standard::Services => PredefinedMenuItem::services(text),
        Standard::Hide => PredefinedMenuItem::hide(text),
        Standard::HideOthers => PredefinedMenuItem::hide_others(text),
        Standard::ShowAll => PredefinedMenuItem::show_all(text),
        Standard::FullScreen => PredefinedMenuItem::fullscreen(text),
        Standard::Minimize => PredefinedMenuItem::minimize(text),
        Standard::Zoom => PredefinedMenuItem::maximize(text),
        Standard::BringAllToFront => PredefinedMenuItem::bring_all_to_front(text),
    }
}

/// `shortcut` as AppKit's key equivalent (letters and the comma; ⌘ for
/// egui's Command).
fn accelerator(shortcut: &KeyboardShortcut) -> Option<Accelerator> {
    let m = shortcut.modifiers;
    let mut mods = Modifiers::empty();
    for (on, modifier) in [
        (m.command || m.mac_cmd, Modifiers::SUPER),
        (m.ctrl, Modifiers::CONTROL),
        (m.alt, Modifiers::ALT),
        (m.shift, Modifiers::SHIFT),
    ] {
        if on {
            mods |= modifier;
        }
    }
    let code = match shortcut.logical_key {
        Key::A => Code::KeyA,
        Key::B => Code::KeyB,
        Key::C => Code::KeyC,
        Key::D => Code::KeyD,
        Key::E => Code::KeyE,
        Key::F => Code::KeyF,
        Key::G => Code::KeyG,
        Key::H => Code::KeyH,
        Key::I => Code::KeyI,
        Key::J => Code::KeyJ,
        Key::K => Code::KeyK,
        Key::L => Code::KeyL,
        Key::M => Code::KeyM,
        Key::N => Code::KeyN,
        Key::O => Code::KeyO,
        Key::P => Code::KeyP,
        Key::Q => Code::KeyQ,
        Key::R => Code::KeyR,
        Key::S => Code::KeyS,
        Key::T => Code::KeyT,
        Key::U => Code::KeyU,
        Key::V => Code::KeyV,
        Key::W => Code::KeyW,
        Key::X => Code::KeyX,
        Key::Y => Code::KeyY,
        Key::Z => Code::KeyZ,
        Key::Comma => Code::Comma,
        other => {
            tracing::debug!(key = ?other, "no key equivalent for this key");
            return None;
        }
    };
    Some(Accelerator::new(Some(mods), code))
}

// ---- title bar ---------------------------------------------------------------

/// The main window: the one whose contents fill it under a transparent
/// title bar ([`unified_title_bar::viewport`]; the undocked consoles keep
/// the system's title bar).
fn main_window(mtm: MainThreadMarker) -> Option<Retained<NSWindow>> {
    NSApplication::sharedApplication(mtm)
        .windows()
        .to_vec()
        .into_iter()
        .find(|w| {
            w.titlebarAppearsTransparent()
                && w.styleMask()
                    .contains(NSWindowStyleMask::FullSizeContentView)
        })
}

/// The main window's title bar and window buttons as AppKit lays them out,
/// in points from the window's top left corner.
pub fn title_bar_geometry() -> Option<Geometry> {
    let mtm = MainThreadMarker::new()?;
    let window = main_window(mtm)?;
    let height = window.contentView()?.frame().size.height;
    let layout = window.contentLayoutRect();
    let zoom = window.standardWindowButton(NSWindowButton::ZoomButton)?;
    // In the window's coordinates, which go up from its bottom left corner.
    let button = zoom.convertRect_toView(zoom.bounds(), None);
    let geometry = Geometry {
        height: (height - (layout.origin.y + layout.size.height)) as f32,
        buttons_right: (button.origin.x + button.size.width) as f32,
        buttons_middle: (height - (button.origin.y + button.size.height / 2.0)) as f32,
    };
    let sane = |v: f32| v.is_finite() && (1.0..200.0).contains(&v);
    (sane(geometry.height) && sane(geometry.buttons_right) && sane(geometry.buttons_middle))
        .then_some(geometry)
}

/// What a double-click on a title bar does, from System Settings.
pub fn double_click_action() -> TitleAction {
    let key = NSString::from_str("AppleActionOnDoubleClick");
    let value = NSUserDefaults::standardUserDefaults()
        .stringForKey(&key)
        .map(|v| v.to_string());
    unified_title_bar::double_click_action(value.as_deref())
}
