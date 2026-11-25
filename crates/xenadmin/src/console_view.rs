//! The Console tab (`VNCTabView` / `XSVNCScreen` in the original): the
//! remote screen drawn from the backend's framebuffer, keyboard and mouse
//! input, the cursor, Send Ctrl+Alt+Del, Scale, Undock (its own window) and
//! Fullscreen, clipboard text both ways, and the power-state messages when
//! the VM does not run.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use egui::{
    Button, Color32, ColorImage, Context, CursorIcon, Event, EventFilter, Id, Pos2, Rect, RichText,
    Sense, TextureHandle, TextureOptions, Ui, Vec2, ViewportBuilder, ViewportCommand, ViewportId,
};
use xapi::{VmOperations, VmPowerState};
use xen_core::console::{self, ConsoleEvent, ConsoleHandle, ConsoleStatus, ConsoleTarget};
use xen_core::{BackendHandle, Command, ConnectionId, ConnectionView, VmAction};
use xen_vnc::{Cursor, Input, KeyCode};

use crate::console_keys::{self, Keyboard, Shortcut};
use crate::i18n::fill;
use crate::tab_strings::tab_strings;

/// Which console: a connection's VM or host.
pub type ConsoleKey = (ConnectionId, ConsoleTarget);

/// Maps a point of the drawn console to a framebuffer pixel (the mouse
/// position the guest gets). `image` is where the framebuffer
/// (`fb_width` × `fb_height`) is drawn, at any scale.
pub fn framebuffer_point(pos: Pos2, image: Rect, fb_width: u16, fb_height: u16) -> (u16, u16) {
    let fx = ((pos.x - image.min.x) / image.width() * f32::from(fb_width)).floor();
    let fy = ((pos.y - image.min.y) / image.height() * f32::from(fb_height)).floor();
    let clamp = |v: f32, max: u16| v.clamp(0.0, f32::from(max.saturating_sub(1))) as u16;
    (clamp(fx, fb_width), clamp(fy, fb_height))
}

/// Where a `fb` framebuffer goes inside `available`: scaled to fit (keeping
/// the aspect ratio, centred) or at one framebuffer pixel per screen pixel.
pub fn image_rect(available: Rect, fb: Vec2, scale: bool, pixels_per_point: f32) -> Rect {
    let size = if scale {
        let factor = (available.width() / fb.x).min(available.height() / fb.y);
        fb * factor.max(0.01)
    } else {
        fb / pixels_per_point
    };
    let offset = ((available.size() - size) * 0.5).max(Vec2::ZERO);
    Rect::from_min_size(available.min + offset, size)
}

/// Mouse buttons as RFB numbers them.
fn button_mask(ui: &Ui) -> u8 {
    ui.input(|i| {
        u8::from(i.pointer.primary_down())
            | u8::from(i.pointer.middle_down()) << 1
            | u8::from(i.pointer.secondary_down()) << 2
    })
}

/// What the user asked of the console while drawing it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Requests {
    pub fullscreen: bool,
    pub dock: bool,
}

/// One open console and its drawing state.
pub struct ConsoleView {
    pub handle: ConsoleHandle,
    /// The console widget's id (stable while the console is open).
    pub id: Id,
    texture: Option<TextureHandle>,
    texture_smooth: bool,
    generation: u64,
    cursor: Option<(u64, TextureHandle, Arc<Cursor>)>,
    keyboard: Keyboard,
    buttons: u8,
    last_pointer: Option<(u16, u16)>,
    focused: bool,
    /// Where the framebuffer was drawn last (screen points).
    pub image: Option<Rect>,
    /// Wheel movement not yet sent (points).
    wheel: f32,
    /// The egui pass it was last drawn in (drawn once per pass, even when
    /// it moves between its window and the tab during one).
    drawn_pass: Option<u64>,
}

/// Points of wheel movement per wheel "click" sent to the guest.
const WHEEL_STEP: f32 = 40.0;

impl ConsoleView {
    pub fn new(handle: ConsoleHandle) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            handle,
            id: Id::unique(("vnc-console", n)),
            texture: None,
            texture_smooth: false,
            generation: u64::MAX,
            cursor: None,
            keyboard: Keyboard::default(),
            buttons: 0,
            last_pointer: None,
            focused: false,
            image: None,
            wheel: 0.0,
            drawn_pass: None,
        }
    }

    /// Uploads what changed in the framebuffer; returns the texture and the
    /// framebuffer size.
    fn upload(&mut self, ctx: &Context, smooth: bool) -> Option<(egui::TextureId, Vec2)> {
        let screen = self.handle.screen().clone();
        let mut state = screen.lock();
        let fb = &mut state.framebuffer;
        let (w, h) = (usize::from(fb.width()), usize::from(fb.height()));
        if w == 0 || h == 0 {
            return None;
        }
        let options = if smooth {
            TextureOptions::LINEAR
        } else {
            TextureOptions::NEAREST
        };
        let size_changed = self.texture.as_ref().is_none_or(|t| t.size() != [w, h]);
        if size_changed || smooth != self.texture_smooth {
            fb.take_dirty();
            let image = ColorImage::from_rgba_unmultiplied([w, h], fb.rgba());
            match &mut self.texture {
                Some(t) => t.set(image, options),
                None => self.texture = Some(ctx.load_texture("vnc-console", image, options)),
            }
            self.texture_smooth = smooth;
            self.generation = fb.generation();
        } else if fb.generation() != self.generation {
            if let Some(dirty) = fb.take_dirty() {
                let region = fb.region(dirty);
                let image = ColorImage::from_rgba_unmultiplied(
                    [usize::from(dirty.width), usize::from(dirty.height)],
                    &region,
                );
                if let Some(t) = &mut self.texture {
                    t.set_partial([usize::from(dirty.x), usize::from(dirty.y)], image, options);
                }
            }
            self.generation = fb.generation();
        }
        // The cursor shape.
        let generation = state.cursor_generation;
        match &state.cursor {
            Some(c) if self.cursor.as_ref().is_none_or(|(g, ..)| *g != generation) => {
                let image = ColorImage::from_rgba_unmultiplied(
                    [usize::from(c.width), usize::from(c.height)],
                    &c.rgba,
                );
                let texture = ctx.load_texture("vnc-cursor", image, TextureOptions::NEAREST);
                self.cursor = Some((generation, texture, c.clone()));
            }
            None => self.cursor = None,
            _ => {}
        }
        let size = Vec2::new(w as f32, h as f32);
        self.texture.as_ref().map(|t| (t.id(), size))
    }

    /// Draws the console in `ui`'s remaining space and handles its input.
    pub fn show(&mut self, ui: &mut Ui, scale: bool) -> Requests {
        let mut requests = Requests::default();
        let available = ui.available_rect_before_wrap();
        let pass = ui.ctx().cumulative_pass_nr();
        if self.drawn_pass == Some(pass) {
            ui.allocate_rect(available, Sense::hover());
            return requests;
        }
        self.drawn_pass = Some(pass);
        let ppp = ui.ctx().pixels_per_point();
        let Some((texture, fb_size)) = self.upload(ui.ctx(), scale) else {
            ui.allocate_rect(available, Sense::hover());
            self.image = None;
            return requests;
        };
        let rect = image_rect(available, fb_size, scale, ppp);
        let id = self.id;
        let response = ui.interact(rect, id, Sense::click_and_drag());
        ui.allocate_rect(available, Sense::hover());
        let painter = ui.painter_at(available);
        painter.rect_filled(available, 0.0, Color32::BLACK);
        painter.image(
            texture,
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        self.image = Some(rect);
        let pressed_on = response.is_pointer_button_down_on();
        if pressed_on || response.clicked() {
            response.request_focus();
        }
        let focused = response.has_focus();
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                );
            });
            painter.rect_stroke(
                rect.expand(1.0),
                0.0,
                egui::Stroke::new(1.0, ui.visuals().selection.stroke.color),
                egui::StrokeKind::Outside,
            );
        }
        if self.focused && !focused {
            // Focus lost: let go of every key and button in the guest.
            for input in self.keyboard.release_all() {
                self.handle.send(input);
            }
            if self.buttons != 0 {
                self.buttons = 0;
                if let Some((x, y)) = self.last_pointer {
                    self.handle.send(Input::Pointer { buttons: 0, x, y });
                }
            }
        }
        self.focused = focused;
        // Keyboard zoom (Ctrl +/-) would eat keys meant for the guest.
        ui.ctx().options_mut(|o| o.zoom_with_keyboard = !focused);

        let (fb_w, fb_h) = (fb_size.x as u16, fb_size.y as u16);
        if focused {
            requests = self.keyboard_input(ui);
        }

        // Mouse: while over the console (or dragging from it).
        let pointer = ui.input(|i| i.pointer.latest_pos());
        let over = pointer.is_some_and(|p| rect.contains(p));
        if let Some(pos) = pointer
            && (over || response.dragged())
            && (focused || pressed_on || response.clicked())
        {
            let (x, y) = framebuffer_point(pos, rect, fb_w, fb_h);
            let buttons = button_mask(ui);
            if self.last_pointer != Some((x, y)) || self.buttons != buttons {
                self.handle.send(Input::Pointer { buttons, x, y });
                self.last_pointer = Some((x, y));
                self.buttons = buttons;
            }
            // The wheel: one button 4/5 press and release per step.
            let delta = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        Event::MouseWheel { unit, delta, .. } => Some(match unit {
                            egui::MouseWheelUnit::Point => delta.y,
                            egui::MouseWheelUnit::Line => delta.y * WHEEL_STEP,
                            egui::MouseWheelUnit::Page => delta.y * WHEEL_STEP * 10.0,
                        }),
                        _ => None,
                    })
                    .sum::<f32>()
            });
            self.wheel += delta;
            while self.wheel.abs() >= WHEEL_STEP {
                let up = self.wheel > 0.0;
                let bit = if up { 1 << 3 } else { 1 << 4 };
                self.handle.send(Input::Pointer {
                    buttons: buttons | bit,
                    x,
                    y,
                });
                self.handle.send(Input::Pointer { buttons, x, y });
                self.wheel -= if up { WHEEL_STEP } else { -WHEEL_STEP };
            }
        }

        // The guest's cursor, drawn at the mouse (the server does not draw
        // it into the framebuffer once it sends a shape).
        if let Some(pos) = pointer
            && over
        {
            if let Some((_, tex, shape)) = &self.cursor {
                ui.ctx().set_cursor_icon(CursorIcon::None);
                let k = rect.width() / fb_size.x;
                let size = Vec2::new(f32::from(shape.width), f32::from(shape.height)) * k;
                let hot = Vec2::new(f32::from(shape.hotspot_x), f32::from(shape.hotspot_y)) * k;
                painter.image(
                    tex.id(),
                    Rect::from_min_size(pos - hot, size),
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
            }
        }

        // Clipboard text from the guest goes to the local clipboard.
        for event in self.handle.take_events() {
            if let ConsoleEvent::CutText(text) = event {
                ui.ctx().copy_text(text);
            }
        }
        requests
    }

    fn keyboard_input(&mut self, ui: &Ui) -> Requests {
        let mut requests = Requests::default();
        let scancodes = self.handle.screen().lock().scancodes;
        let events = ui.input(|i| i.events.clone());
        for event in events {
            match event {
                Event::Key {
                    key,
                    physical_key,
                    pressed,
                    ..
                } => {
                    let Some(code) = console_keys::key_code(key, physical_key) else {
                        continue;
                    };
                    if !scancodes && pressed && !self.keyboard.send_in_keysym_mode(code) {
                        // The character comes with the text event.
                        continue;
                    }
                    let (inputs, shortcut) = self.keyboard.key(code, pressed);
                    for input in inputs {
                        self.handle.send(input);
                    }
                    match shortcut {
                        Some(Shortcut::CtrlAltDel) => self.handle.send_ctrl_alt_del(),
                        Some(Shortcut::Fullscreen) => requests.fullscreen = true,
                        Some(Shortcut::Dock) => requests.dock = true,
                        Some(Shortcut::Release) => {
                            ui.ctx().memory_mut(|m| m.surrender_focus(self.id));
                        }
                        None => {}
                    }
                }
                Event::Text(text) if !scancodes => {
                    for c in text.chars() {
                        let keysym = console_keys::char_keysym(c);
                        for down in [true, false] {
                            self.handle.send(Input::Key {
                                down,
                                scancode: 0,
                                keysym,
                            });
                        }
                    }
                }
                // egui turns Ctrl+C / Ctrl+X / Ctrl+V into these instead of
                // key events; the guest gets the key (Ctrl is already down).
                Event::Copy => self.tap(KeyCode::KeyC),
                Event::Cut => self.tap(KeyCode::KeyX),
                Event::Paste(text) => {
                    // The local clipboard also goes to the guest's.
                    self.handle.send(Input::CutText(text));
                    self.tap(KeyCode::KeyV);
                }
                _ => {}
            }
        }
        requests
    }

    fn tap(&mut self, code: KeyCode) {
        for pressed in [true, false] {
            let (inputs, _) = self.keyboard.key(code, pressed);
            for input in inputs {
                self.handle.send(input);
            }
        }
    }
}

/// Every open console of the window, and the console options.
#[derive(Default)]
pub struct Consoles {
    views: HashMap<ConsoleKey, ConsoleView>,
    /// Scale to fit (the original's "Scale" box).
    pub scale: bool,
    /// Consoles in their own window.
    pub undocked: BTreeSet<ConsoleKeyOrd>,
    /// The console shown full screen.
    pub fullscreen: Option<ConsoleKey>,
}

/// A [`ConsoleKey`] that sorts (connection, then the target's reference).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleKeyOrd(pub ConsoleKey);

impl PartialOrd for ConsoleKeyOrd {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ConsoleKeyOrd {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let k = |c: &ConsoleKeyOrd| {
            let (id, t) = &c.0;
            let (kind, r) = match t {
                ConsoleTarget::Vm(r) => (0, r.as_str().to_owned()),
                ConsoleTarget::Host(r) => (1, r.as_str().to_owned()),
            };
            (*id, kind, r)
        };
        k(self).cmp(&k(other))
    }
}

/// What the Console tab asks the window to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabAction {
    /// Start or resume the VM ("Click here to start it").
    Vm(VmAction),
}

impl Consoles {
    pub fn new() -> Self {
        Self {
            scale: true,
            ..Self::default()
        }
    }

    /// The console of `key`, opened if needed.
    pub fn open(&mut self, backend: &BackendHandle, key: &ConsoleKey) -> &mut ConsoleView {
        self.views.entry(key.clone()).or_insert_with(|| {
            let (handle, link) = console::pair();
            backend.send(Command::OpenConsole {
                connection: key.0,
                target: key.1.clone(),
                link,
            });
            ConsoleView::new(handle)
        })
    }

    /// The open console of `key`.
    #[cfg(test)]
    pub fn get(&self, key: &ConsoleKey) -> Option<&ConsoleView> {
        self.views.get(key)
    }

    /// Closes the consoles not shown: only `selected` (the console of the
    /// selected object) and the undocked ones stay open.
    pub fn retain(&mut self, selected: Option<&ConsoleKey>, live: impl Fn(ConnectionId) -> bool) {
        self.undocked.retain(|k| live(k.0.0));
        if self.fullscreen.as_ref().is_some_and(|k| !live(k.0)) {
            self.fullscreen = None;
        }
        let undocked: Vec<ConsoleKey> = self.undocked.iter().map(|k| k.0.clone()).collect();
        let full = self.fullscreen.clone();
        self.views
            .retain(|k, _| Some(k) == selected || undocked.contains(k) || full.as_ref() == Some(k));
    }

    pub fn is_undocked(&self, key: &ConsoleKey) -> bool {
        self.undocked.contains(&ConsoleKeyOrd(key.clone()))
    }

    fn toggle_dock(&mut self, key: &ConsoleKey) {
        let k = ConsoleKeyOrd(key.clone());
        if !self.undocked.remove(&k) {
            self.undocked.insert(k);
        }
    }

    fn toggle_fullscreen(&mut self, ctx: &Context, key: &ConsoleKey) {
        let on = self.fullscreen.as_ref() != Some(key);
        self.fullscreen = on.then(|| key.clone());
        ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Fullscreen(on));
    }

    /// The Console tab of the selected VM or host.
    pub fn tab(
        &mut self,
        ui: &mut Ui,
        backend: &BackendHandle,
        conn: &ConnectionView,
        key: &ConsoleKey,
        title: &str,
    ) -> Option<TabAction> {
        let t = tab_strings();
        let mut action = None;
        let model = &conn.model;
        let undocked = self.is_undocked(key);
        let scale = self.scale;
        let view = self.open(backend, key);
        let status = view.handle.status();
        let connected = matches!(status, ConsoleStatus::Connected { .. });
        let mut requests = Requests::default();
        ui.horizontal(|ui| {
            if let ConsoleTarget::Host(_) = &key.1 {
                ui.label(RichText::new(fill(t.con_host, &[("name", title)])).strong());
            }
            // The power-state line, clickable when the VM can be started or
            // resumed, as the original.
            let vm = match &key.1 {
                ConsoleTarget::Vm(r) => model.get(r),
                ConsoleTarget::Host(_) => None,
            };
            match (&status, vm) {
                (ConsoleStatus::NotRunning(VmPowerState::Halted), Some(vm)) => {
                    if vm.allowed_operations.contains(&VmOperations::Start) {
                        if ui.link(t.con_halted_start).clicked() {
                            action = Some(TabAction::Vm(VmAction::Start));
                        }
                    } else {
                        ui.label(t.con_halted);
                    }
                }
                (ConsoleStatus::NotRunning(VmPowerState::Suspended), Some(vm)) => {
                    if vm.allowed_operations.contains(&VmOperations::Resume) {
                        if ui.link(t.con_suspended_resume).clicked() {
                            action = Some(TabAction::Vm(VmAction::Resume));
                        }
                    } else {
                        ui.label(t.con_suspended);
                    }
                }
                (ConsoleStatus::NotRunning(VmPowerState::Paused), _) => {
                    ui.label(t.con_paused);
                }
                (ConsoleStatus::NotRunning(_), _) => {
                    ui.label(t.con_halted);
                }
                (ConsoleStatus::HostDown, _) => {
                    ui.label(t.con_host_dead);
                }
                (ConsoleStatus::Connecting, _) => {
                    ui.spinner();
                    ui.label(t.con_looking);
                }
                (ConsoleStatus::Retrying { error }, _) => {
                    ui.spinner();
                    ui.label(fill(t.con_retrying, &[("error", error)]));
                }
                (ConsoleStatus::Disconnected, _) => {
                    ui.label(t.con_disconnected);
                }
                (ConsoleStatus::Gone, _) => {
                    ui.label(t.con_gone);
                }
                (ConsoleStatus::Connected { .. }, _) => {}
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t.con_fullscreen).clicked() {
                    requests.fullscreen = true;
                }
                let dock = if undocked { t.con_redock } else { t.con_undock };
                if ui.button(dock).clicked() {
                    requests.dock = true;
                }
                let mut scale_box = scale;
                if ui.checkbox(&mut scale_box, t.con_scale).changed() {
                    self.scale = scale_box;
                }
                if ui
                    .add_enabled(connected, Button::new(t.con_send_cad))
                    .clicked()
                {
                    self.views[key].handle.send_ctrl_alt_del();
                }
            });
        });
        ui.separator();
        let view = self.views.get_mut(key).expect("opened above");
        if undocked {
            ui.centered_and_justified(|ui| ui.label(t.con_undocked_here));
        } else if connected
            || matches!(
                status,
                ConsoleStatus::Retrying { .. } | ConsoleStatus::Connecting
            ) && view.handle.sessions() > 0
        {
            // While reconnecting (a reboot), the last screen stays, as in
            // the original; a stopped VM shows only its message.
            let r = view.show(ui, self.scale);
            requests.fullscreen |= r.fullscreen;
            requests.dock |= r.dock;
        } else {
            ui.allocate_rect(ui.available_rect_before_wrap(), Sense::hover());
        }
        if requests.dock {
            self.toggle_dock(key);
        }
        if requests.fullscreen {
            self.toggle_fullscreen(ui.ctx(), key);
        }
        // Keep drawing while the guest's screen changes.
        if connected {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
        action
    }

    /// The undocked consoles' windows.
    pub fn windows(&mut self, ctx: &Context, titles: impl Fn(&ConsoleKey) -> String) {
        let t = tab_strings();
        let keys: Vec<ConsoleKey> = self.undocked.iter().map(|k| k.0.clone()).collect();
        for key in keys {
            let Some(view) = self.views.get_mut(&key) else {
                continue;
            };
            let scale = self.scale;
            let title = fill(t.con_window_title, &[("name", &titles(&key))]);
            let (w, h) = {
                let state = view.handle.screen().lock();
                (
                    state.framebuffer.width().max(640),
                    state.framebuffer.height().max(400),
                )
            };
            let id = ViewportId::from_hash_of(("console", key.0, format!("{:?}", key.1)));
            let mut redock = false;
            let mut requests = Requests::default();
            ctx.show_viewport_immediate(
                id,
                ViewportBuilder::default()
                    .with_title(title)
                    .with_inner_size([f32::from(w), f32::from(h) + 36.0]),
                |ui, _class| {
                    if ui.input(|i| i.viewport().close_requested()) {
                        redock = true;
                    }
                    egui::CentralPanel::default().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button(t.con_redock).clicked() {
                                redock = true;
                            }
                            if ui.button(t.con_send_cad).clicked() {
                                view.handle.send_ctrl_alt_del();
                            }
                        });
                        let r = view.show(ui, scale);
                        requests.dock |= r.dock;
                        requests.fullscreen |= r.fullscreen;
                    });
                },
            );
            if redock || requests.dock {
                self.undocked.remove(&ConsoleKeyOrd(key.clone()));
            }
            if requests.fullscreen {
                self.toggle_fullscreen(ctx, &key);
            }
        }
    }

    /// Full screen: only the console, with a thin bar to leave. Returns
    /// whether a console is full screen (then nothing else is drawn).
    pub fn fullscreen_ui(&mut self, ui: &mut Ui) -> bool {
        let t = tab_strings();
        let Some(key) = self.fullscreen.clone() else {
            return false;
        };
        let Some(view) = self.views.get_mut(&key) else {
            self.fullscreen = None;
            ui.ctx()
                .send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Fullscreen(false));
            return false;
        };
        let mut leave = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button(t.con_exit_fullscreen).clicked() {
                        leave = true;
                    }
                    if ui.button(t.con_send_cad).clicked() {
                        view.handle.send_ctrl_alt_del();
                    }
                });
                // Full screen always fits the console to the screen.
                let r = view.show(ui, true);
                leave |= r.fullscreen;
            });
        if leave {
            self.fullscreen = None;
            ui.ctx()
                .send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Fullscreen(false));
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mouse_lands_on_the_pixel_under_it_at_any_scale() {
        // 800x600 drawn at half size at (100, 50).
        let image = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(400.0, 300.0));
        assert_eq!(
            framebuffer_point(Pos2::new(100.0, 50.0), image, 800, 600),
            (0, 0)
        );
        assert_eq!(
            framebuffer_point(Pos2::new(300.0, 200.0), image, 800, 600),
            (400, 300)
        );
        assert_eq!(
            framebuffer_point(Pos2::new(499.9, 349.9), image, 800, 600),
            (799, 599)
        );
        // Outside: clamped to the edge, as the original's PointerEvent.
        assert_eq!(
            framebuffer_point(Pos2::new(0.0, 900.0), image, 800, 600),
            (0, 599)
        );
        // Scaled up 2x: two points per pixel.
        let image = Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 768.0));
        assert_eq!(
            framebuffer_point(Pos2::new(3.0, 3.0), image, 640, 384),
            (1, 1)
        );
        assert_eq!(
            framebuffer_point(Pos2::new(1279.0, 767.0), image, 640, 384),
            (639, 383)
        );
    }

    #[test]
    fn scaling_fits_and_centres_or_keeps_pixels() {
        let available = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(1000.0, 500.0));
        let fb = Vec2::new(800.0, 600.0);
        let r = image_rect(available, fb, true, 1.0);
        assert!((r.width() - 500.0 * 800.0 / 600.0).abs() < 0.01, "{r:?}");
        assert!((r.height() - 500.0).abs() < 0.01, "{r:?}");
        assert!((r.center().x - available.center().x).abs() < 0.01);
        // 1:1 on a 2x display: 400x300 points (800x600 screen pixels).
        let r = image_rect(available, fb, false, 2.0);
        assert_eq!(r.size(), Vec2::new(400.0, 300.0));
    }
}
