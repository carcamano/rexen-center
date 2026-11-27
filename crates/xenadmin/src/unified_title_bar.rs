//! The toolbar in the title bar on macOS: the unified title bar and toolbar
//! of macOS 11 and later (this console's own; the original had its toolbar
//! in a row under the menus).
//!
//! The main window has a full-size content view and a transparent title bar
//! without its title. AppKit still draws the window buttons (the "traffic
//! lights") at the top left and resizes the window at its edges; the app
//! draws its toolbar across the title bar, right of the buttons. The
//! buttons stay where AppKit puts them, in the middle of the title bar: only
//! an `NSToolbar` moves them lower, and it would also take the clicks meant
//! for the app's own buttons under it. So the bar is as tall as AppKit's
//! title bar and its contents are centred on the window buttons, both
//! measured from the window (`crate::macos::title_bar_geometry`). Dragging
//! the bar's empty space moves the window; double-clicking it zooms or
//! minimizes the window as System Settings say (Desktop & Dock,
//! "Double-click a window's title bar to"). `REXENCENTER_TITLE_BAR=system`
//! keeps the system's title bar, with the toolbar in a row under it.

use egui::{
    Align, Frame, Layout, Margin, PointerButton, Rect, Sense, Ui, UiBuilder, ViewportBuilder,
    ViewportCommand,
};

use crate::window_frame::{TITLE_BAR_ENV, TitleAction};

/// Where AppKit puts the title bar and the window buttons, in points from
/// the window's top left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    /// The title bar's height.
    pub height: f32,
    /// The right edge of the rightmost window button (Zoom).
    pub buttons_right: f32,
    /// The window buttons' vertical centre.
    pub buttons_middle: f32,
}

impl Default for Geometry {
    /// About macOS 11's layout, until the window's own is known: a
    /// 28-point title bar, three 14-point buttons 20 points apart from 7
    /// points in.
    fn default() -> Self {
        Self {
            height: 28.0,
            buttons_right: 61.0,
            buttons_middle: 14.0,
        }
    }
}

impl Geometry {
    fn scaled(self, factor: f32) -> Self {
        Self {
            height: self.height * factor,
            buttons_right: self.buttons_right * factor,
            buttons_middle: self.buttons_middle * factor,
        }
    }
}

/// Between the window buttons and the toolbar, and after the toolbar.
const GAP: f32 = 12.0;

/// The toolbar's bar across the title bar.
pub struct UnifiedTitleBar {
    /// The window's title bar layout, in AppKit's points (`None` while it
    /// cannot be read).
    geometry: fn() -> Option<Geometry>,
    /// What a double-click on the bar does: read at each double-click, so
    /// that a change in System Settings applies at once.
    double_click: fn() -> TitleAction,
}

impl UnifiedTitleBar {
    // Called on macOS (and in the tests).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn new(geometry: fn() -> Option<Geometry>, double_click: fn() -> TitleAction) -> Self {
        Self {
            geometry,
            double_click,
        }
    }

    /// The frame of the panel at the window's top: its fill, and no margins
    /// (the bar places its contents).
    pub fn panel_frame(ui: &Ui) -> Frame {
        Frame::side_top_panel(ui.style()).inner_margin(Margin::ZERO)
    }

    /// The bar across `ui`'s width, as tall as the title bar, with
    /// `contents` (the toolbar) right of the window buttons and centred on
    /// them. In full screen the title bar only slides in over the window
    /// while the pointer is at the screen's top, so the contents start at
    /// the left edge.
    pub fn show(&self, ui: &mut Ui, contents: impl FnOnce(&mut Ui)) {
        let ctx = ui.ctx().clone();
        let (full_screen, maximized) = ctx.input(|i| {
            let v = i.viewport();
            (v.fullscreen.unwrap_or(false), v.maximized.unwrap_or(false))
        });
        // AppKit's points are egui's at zoom 1.
        let g = (self.geometry)()
            .unwrap_or_default()
            .scaled(1.0 / ctx.zoom_factor());
        let row = ui.spacing().interact_size.y;
        let area = ui.max_rect();
        let top = ui.cursor().min.y;
        let (left, middle) = if full_screen {
            (area.left() + GAP, top + g.height.max(row + 8.0) / 2.0)
        } else {
            (area.left() + g.buttons_right + GAP, g.buttons_middle)
        };
        let bottom = (top + g.height).max(middle + row / 2.0 + 4.0);
        let rect = Rect::from_x_y_ranges(area.x_range(), top..=bottom);
        // First, so that every widget on the bar is on top of it.
        let bar = ui.interact(
            rect,
            ui.make_persistent_id("unified-title-bar"),
            Sense::click_and_drag(),
        );

        let right = (rect.right() - GAP).max(left);
        let contents_rect =
            Rect::from_x_y_ranges(left..=right, (middle - row / 2.0)..=(middle + row / 2.0));
        let mut contents_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(contents_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        contents_ui.set_clip_rect(
            Rect::from_x_y_ranges(left..=right, rect.y_range()).intersect(ui.clip_rect()),
        );
        contents(&mut contents_ui);
        ui.advance_cursor_after_rect(rect);

        // egui gives a drag that starts on a toolbar button to the bar under
        // it; that drag is the button's (as for AppKit's toolbars), so where
        // the press landed is remembered.
        let on_widget = bar.id.with("pressed-on-widget");
        if ctx.input(|i| i.pointer.primary_pressed()) {
            let under: Vec<egui::Id> = ctx.interaction_snapshot(|s| {
                s.hovered
                    .iter()
                    .copied()
                    .filter(|id| *id != bar.id)
                    .collect()
            });
            let pressed_on_widget = under.into_iter().any(|id| {
                ctx.read_response(id)
                    .is_some_and(|r| r.sense.senses_click() || r.sense.senses_drag())
            });
            ctx.data_mut(|d| d.insert_temp(on_widget, pressed_on_widget));
        }
        let pressed_on_widget = ctx.data(|d| d.get_temp::<bool>(on_widget).unwrap_or(false));

        if bar.drag_started_by(PointerButton::Primary) && !pressed_on_widget {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            // AppKit moves the window until the button is released, and
            // egui never sees the release.
            ctx.stop_dragging();
        } else if bar.double_clicked() && !full_screen {
            match (self.double_click)() {
                TitleAction::ToggleMaximize => {
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                }
                TitleAction::Minimize => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
                TitleAction::Close | TitleAction::Menu | TitleAction::Nothing => {}
            }
        }
    }
}

/// The action of macOS's `AppleActionOnDoubleClick` setting: `Maximize`
/// (Zoom, the default), `Minimize`, `None` (Do Nothing) or `Fill` (macOS
/// 15: fill the screen, which AppKit offers on its own title bar only; Zoom
/// is the nearest).
// Called on macOS (and in the tests).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn double_click_action(setting: Option<&str>) -> TitleAction {
    match setting.map(str::trim) {
        Some("Minimize") => TitleAction::Minimize,
        Some("None") => TitleAction::Nothing,
        _ => TitleAction::ToggleMaximize,
    }
}

/// Whether the main window gets the unified title bar: on macOS, unless
/// `REXENCENTER_TITLE_BAR` asks for the system's title bar (`system`) or
/// for one drawn by [`crate::window_frame`] (`own_title_bar`).
pub fn wanted(own_title_bar: bool) -> bool {
    let system =
        std::env::var(TITLE_BAR_ENV).is_ok_and(|v| v.trim().eq_ignore_ascii_case("system"));
    cfg!(target_os = "macos") && !own_title_bar && !system
}

/// `viewport` for the unified title bar: the window's contents fill it,
/// under a transparent title bar without its title (the window buttons
/// stay).
pub fn viewport(viewport: ViewportBuilder) -> ViewportBuilder {
    viewport
        .with_fullsize_content_view(true)
        .with_titlebar_shown(false)
        .with_title_shown(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, Pos2, pos2, vec2};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    const MEASURED: Geometry = Geometry {
        height: 32.0,
        buttons_right: 76.0,
        buttons_middle: 16.0,
    };

    fn harness(
        geometry: fn() -> Option<Geometry>,
        double_click: fn() -> TitleAction,
    ) -> Harness<'static> {
        let bar = UnifiedTitleBar::new(geometry, double_click);
        let mut h = Harness::builder()
            .with_size(vec2(700.0, 120.0))
            // 60 frames a second, so that two clicks make a double-click.
            .with_step_dt(1.0 / 60.0)
            .build_ui(move |ui| {
                bar.show(ui, |ui| {
                    let _ = ui.button("Connect…");
                    let _ = ui.button("Start");
                });
                ui.label("Below");
            });
        h.run();
        h
    }

    /// The commands the last frame sent to the window.
    fn commands(h: &Harness<'_>) -> Vec<ViewportCommand> {
        h.output()
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.commands.clone())
            .unwrap_or_default()
    }

    fn press(h: &Harness<'_>, pos: Pos2, pressed: bool) {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }

    #[test]
    fn the_toolbar_sits_right_of_the_window_buttons_and_centred_on_them() {
        let h = harness(|| Some(MEASURED), || TitleAction::ToggleMaximize);
        let connect = h.get_by_label("Connect…").rect();
        let start = h.get_by_label("Start").rect();
        assert!(
            connect.min.x >= MEASURED.buttons_right + GAP - 0.5,
            "{connect:?}"
        );
        assert!(connect.max.x < start.min.x);
        assert!(
            (connect.center().y - MEASURED.buttons_middle).abs() < 0.5,
            "{connect:?}"
        );
        // The rest of the window starts under the title bar.
        assert!(h.get_by_label("Below").rect().min.y >= MEASURED.height);

        // Until AppKit's layout is known, macOS 11's.
        let h = harness(|| None, || TitleAction::ToggleMaximize);
        let connect = h.get_by_label("Connect…").rect();
        let default = Geometry::default();
        assert!(connect.min.x >= default.buttons_right + GAP - 0.5);
        assert!((connect.center().y - default.buttons_middle).abs() < 0.5);
    }

    #[test]
    fn dragging_the_bar_moves_the_window() {
        let mut h = harness(|| Some(MEASURED), || TitleAction::ToggleMaximize);
        let empty = pos2(500.0, MEASURED.buttons_middle);
        h.hover_at(empty);
        press(&h, empty, true);
        h.step();
        h.hover_at(empty + vec2(30.0, 5.0));
        h.step();
        assert!(
            commands(&h).contains(&ViewportCommand::StartDrag),
            "{:?}",
            commands(&h)
        );
        press(&h, empty + vec2(30.0, 5.0), false);
        h.run();

        // The toolbar's buttons are not part of the bar.
        let start = h.get_by_label("Start").rect().center();
        h.hover_at(start);
        press(&h, start, true);
        h.step();
        h.hover_at(start + vec2(30.0, 0.0));
        h.step();
        assert!(!commands(&h).contains(&ViewportCommand::StartDrag));
    }

    fn double_click(h: &mut Harness<'_>, pos: Pos2) -> Vec<ViewportCommand> {
        h.hover_at(pos);
        h.step();
        let mut seen = Vec::new();
        for pressed in [true, false, true, false] {
            press(h, pos, pressed);
            h.step();
            seen.extend(commands(h));
        }
        seen
    }

    #[test]
    fn double_clicks_do_what_system_settings_say() {
        let empty = pos2(500.0, MEASURED.buttons_middle);
        let mut h = harness(|| Some(MEASURED), || TitleAction::ToggleMaximize);
        assert_eq!(
            double_click(&mut h, empty),
            vec![ViewportCommand::Maximized(true)]
        );
        let mut h = harness(|| Some(MEASURED), || TitleAction::Minimize);
        assert_eq!(
            double_click(&mut h, empty),
            vec![ViewportCommand::Minimized(true)]
        );
        let mut h = harness(|| Some(MEASURED), || TitleAction::Nothing);
        assert_eq!(double_click(&mut h, empty), vec![]);

        for (setting, action) in [
            (None, TitleAction::ToggleMaximize),
            (Some("Maximize"), TitleAction::ToggleMaximize),
            (Some("Fill"), TitleAction::ToggleMaximize),
            (Some("Minimize"), TitleAction::Minimize),
            (Some("None"), TitleAction::Nothing),
        ] {
            assert_eq!(double_click_action(setting), action, "{setting:?}");
        }
    }

    #[test]
    fn the_window_contents_fill_the_title_bar() {
        let v = viewport(ViewportBuilder::default());
        assert_eq!(v.fullsize_content_view, Some(true));
        assert_eq!(v.titlebar_shown, Some(false));
        assert_eq!(v.title_shown, Some(false));
        // The window buttons stay.
        assert_eq!(v.titlebar_buttons_shown, None);
        // Only on macOS.
        if !cfg!(target_os = "macos") {
            assert!(!wanted(false));
        }
        assert!(!wanted(true));
    }
}
