//! The window's own title bar on Windows, GNOME and KDE Plasma (this
//! console's own; the original was a Windows Forms window with the system's
//! frame).
//!
//! Under GNOME the compositor draws no title bars (applications draw their
//! own), so winit falls back to a plain frame of its own that matches no
//! desktop. On GNOME and on KDE Plasma the windows are therefore opened
//! without decorations and draw a title bar here instead: the menus on the
//! left, the title in the middle and the window buttons in the desktop's
//! look (Adwaita's round buttons, Breeze's glyphs), in the order the
//! desktop's settings put them (`button-layout` in GNOME,
//! `ButtonsOnLeft`/`ButtonsOnRight` in `kwinrc`). Dragging the bar moves the
//! window; double-, middle- and right-clicks do what the desktop's settings
//! say; the window's edges resize it.
//!
//! On Windows the menus move into the title bar too, as in Office or Visual
//! Studio Code, following Microsoft's guidance for custom title bars: a
//! 32-pixel bar with the icon on the left, the title, and Windows 11's
//! 46×32 Minimize, Maximize (Restore) and Close buttons on the right.
//! Dragging the bar moves the window with the system's move loop, so Snap
//! still works; double-clicking it maximizes or restores; right-clicking it
//! or clicking the icon opens the window menu; double-clicking the icon
//! closes. The system keeps drawing the window's shadow, border and rounded
//! corners (winit's undecorated shadow, which eframe asks for on borderless
//! windows). Snap Layouts, the flyout over the Maximize button, needs the
//! window to answer `WM_NCHITTEST` with `HTMAXBUTTON`, which winit has no
//! way to do without unsafe code: it is left out (Win+Z still opens it).
//!
//! macOS and other desktops keep the system's frame.
//! `REXENCENTER_TITLE_BAR` overrides the choice: `system` keeps the system's
//! frame, `gnome`, `kde` or `windows` force that look on any platform.

use std::path::{Path, PathBuf};

use egui::{
    Align, Area, Color32, Context, CornerRadius, CursorIcon, Frame, Id, Image, Layout, Margin,
    Order, PointerButton, Popup, Pos2, Rect, Response, Role, Sense, SetOpenCommand, Stroke,
    StrokeKind, TextStyle, TextWrapMode, TextureHandle, TextureOptions, Ui, UiBuilder, Vec2,
    ViewportCommand, WidgetInfo, WidgetText, pos2, vec2, viewport::ResizeDirection,
};

use crate::i18n::strings;

/// Overrides the title bar: `system`, `gnome`, `kde` or `windows`.
pub const TITLE_BAR_ENV: &str = "REXENCENTER_TITLE_BAR";

/// Whose look the title bar takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// GNOME's (libadwaita's header bar).
    Gnome,
    /// KDE Plasma's (the Breeze window decoration).
    Kde,
    /// Windows 11's (a custom title bar as Microsoft's guidance draws it).
    Windows,
}

/// A button of the title bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// The application's icon (GNOME's `icon`; KDE's `M` and Windows' icon,
    /// which open the window menu).
    Icon,
    Minimize,
    Maximize,
    Close,
}

/// The buttons at each end of the bar, each list read left to right as the
/// desktops write them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonLayout {
    pub left: Vec<Control>,
    pub right: Vec<Control>,
}

/// What a click on the bar's empty space does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleAction {
    ToggleMaximize,
    Minimize,
    Close,
    /// The window menu (Minimize, Maximize or Restore, Close).
    Menu,
    Nothing,
}

/// The title bar drawn by the app, and the desktop settings it follows.
#[derive(Clone)]
pub struct WindowFrame {
    pub flavor: Flavor,
    pub layout: ButtonLayout,
    pub double_click: TitleAction,
    pub middle_click: TitleAction,
    pub right_click: TitleAction,
    icon: Option<TextureHandle>,
}

impl WindowFrame {
    /// The desktop's defaults for `flavor`.
    #[cfg(test)]
    pub fn new(flavor: Flavor) -> Self {
        match flavor {
            Flavor::Gnome => gnome_frame(""),
            Flavor::Kde => kde_frame(""),
            Flavor::Windows => windows_frame(),
        }
    }
}

/// The title bar for this session, or `None` to keep the system's frame.
pub fn detect() -> Option<WindowFrame> {
    let forced = std::env::var(TITLE_BAR_ENV).unwrap_or_default();
    Some(match chosen_flavor(&forced, platform_flavor)? {
        Flavor::Gnome => gnome_frame(&gnome_settings()),
        Flavor::Kde => kde_frame(&kwinrc_text()),
        Flavor::Windows => windows_frame(),
    })
}

/// The flavor `REXENCENTER_TITLE_BAR` (`forced`) asks for, or the
/// platform's own when it is unset; `None` keeps the system's frame.
fn chosen_flavor(forced: &str, platform: impl FnOnce() -> Option<Flavor>) -> Option<Flavor> {
    match forced.trim().to_ascii_lowercase().as_str() {
        "" => platform(),
        "system" => None,
        "gnome" => Some(Flavor::Gnome),
        "kde" => Some(Flavor::Kde),
        "windows" => Some(Flavor::Windows),
        other => {
            tracing::warn!(
                value = other,
                "{TITLE_BAR_ENV}: expected system, gnome, kde or windows"
            );
            None
        }
    }
}

/// The platform's own title bar: Windows' on Windows, GNOME's or KDE's on
/// those desktops; none (the system's frame) on macOS and other desktops.
fn platform_flavor() -> Option<Flavor> {
    if cfg!(windows) {
        Some(Flavor::Windows)
    } else if cfg!(all(unix, not(target_os = "macos"))) {
        flavor_of(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
    } else {
        None
    }
}

/// GNOME or KDE from `XDG_CURRENT_DESKTOP` (a colon-separated list, such as
/// `ubuntu:GNOME`).
fn flavor_of(desktops: &str) -> Option<Flavor> {
    desktops
        .split(':')
        .find_map(|d| match d.trim().to_ascii_uppercase().as_str() {
            "GNOME" => Some(Flavor::Gnome),
            "KDE" => Some(Flavor::Kde),
            _ => None,
        })
}

// ---- GNOME ------------------------------------------------------------------

const GNOME_WM_SCHEMA: &str = "org.gnome.desktop.wm.preferences";

/// `gsettings list-recursively org.gnome.desktop.wm.preferences`, or empty.
fn gnome_settings() -> String {
    let output = std::process::Command::new("gsettings")
        .args(["list-recursively", GNOME_WM_SCHEMA])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        Ok(_) | Err(_) => {
            tracing::info!("gsettings unavailable; GNOME's default title bar buttons");
            String::new()
        }
    }
}

/// The value of `key` in `gsettings list-recursively` output, unquoted.
fn gsettings_value<'a>(listing: &'a str, key: &str) -> Option<&'a str> {
    listing.lines().find_map(|line| {
        let mut parts = line.splitn(3, ' ');
        let (schema, k, value) = (parts.next()?, parts.next()?, parts.next()?);
        (schema == GNOME_WM_SCHEMA && k == key).then(|| value.trim().trim_matches('\''))
    })
}

/// The frame from GNOME's window manager preferences (the output of
/// [`gnome_settings`]; GNOME's defaults for keys it lacks).
fn gnome_frame(listing: &str) -> WindowFrame {
    let action = |key, default| gnome_action(gsettings_value(listing, key).unwrap_or(default));
    WindowFrame {
        flavor: Flavor::Gnome,
        layout: gnome_layout(gsettings_value(listing, "button-layout").unwrap_or("appmenu:close")),
        double_click: action("action-double-click-titlebar", "toggle-maximize"),
        middle_click: action("action-middle-click-titlebar", "none"),
        right_click: action("action-right-click-titlebar", "menu"),
        icon: None,
    }
}

/// GNOME's `button-layout`: the left side's buttons, a colon, the right
/// side's (`appmenu:minimize,maximize,close`). Without a colon all the
/// buttons are on the left, as GTK reads it. Unknown names are skipped.
fn gnome_layout(value: &str) -> ButtonLayout {
    let side = |names: &str| {
        let mut controls = Vec::new();
        for name in names.split(',') {
            let control = match name.trim() {
                "icon" => Control::Icon,
                "minimize" => Control::Minimize,
                "maximize" => Control::Maximize,
                "close" => Control::Close,
                _ => continue,
            };
            if !controls.contains(&control) {
                controls.push(control);
            }
        }
        controls
    };
    let (left, right) = value.split_once(':').unwrap_or((value, ""));
    let left = side(left);
    let right = side(right)
        .into_iter()
        .filter(|c| !left.contains(c))
        .collect();
    ButtonLayout { left, right }
}

fn gnome_action(value: &str) -> TitleAction {
    match value {
        "toggle-maximize" | "toggle-maximize-horizontally" | "toggle-maximize-vertically" => {
            TitleAction::ToggleMaximize
        }
        "minimize" => TitleAction::Minimize,
        "menu" => TitleAction::Menu,
        _ => TitleAction::Nothing,
    }
}

// ---- KDE --------------------------------------------------------------------

/// The `kwinrc` files, the system's first so that the user's override them.
fn kwinrc_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::env::var("XDG_CONFIG_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/etc/xdg".to_owned())
        .split(':')
        .rev()
        .map(|dir| Path::new(dir).join("kwinrc"))
        .collect();
    let user = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")));
    if let Some(dir) = user {
        paths.push(dir.join("kwinrc"));
    }
    paths
}

/// The `kwinrc` files read one after the other (later keys win).
fn kwinrc_text() -> String {
    kwinrc_paths()
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The last value of `key` in `[group]` of a KDE configuration file. Keys
/// may carry flags (`ButtonsOnLeft[$i]`).
fn kconfig_value<'a>(text: &'a str, group: &str, key: &str) -> Option<&'a str> {
    let mut in_group = false;
    let mut found = None;
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_group = name == group;
        } else if in_group
            && let Some((k, v)) = line.split_once('=')
            && k.split('[').next().map(str::trim) == Some(key)
        {
            found = Some(v.trim());
        }
    }
    found
}

/// The frame from KWin's settings (`kwinrc`; Plasma's defaults for keys it
/// lacks).
fn kde_frame(kwinrc: &str) -> WindowFrame {
    const DECORATION: &str = "org.kde.kdecoration2";
    let buttons =
        |key, default| kde_buttons(kconfig_value(kwinrc, DECORATION, key).unwrap_or(default));
    let left = buttons("ButtonsOnLeft", "M");
    let right = buttons("ButtonsOnRight", "IAX")
        .into_iter()
        .filter(|c| !left.contains(c))
        .collect();
    let mouse =
        |key, default| kde_action(kconfig_value(kwinrc, "MouseBindings", key).unwrap_or(default));
    WindowFrame {
        flavor: Flavor::Kde,
        layout: ButtonLayout { left, right },
        double_click: kde_action(
            kconfig_value(kwinrc, "Windows", "TitlebarDoubleClickCommand").unwrap_or("Maximize"),
        ),
        middle_click: mouse("CommandActiveTitlebar2", "Nothing"),
        right_click: mouse("CommandActiveTitlebar3", "Operations menu"),
        icon: None,
    }
}

/// KWin's button letters: `M` menu (the icon), `I` minimize, `A` maximize,
/// `X` close; the others (help, on all desktops, keep above…) are skipped.
fn kde_buttons(letters: &str) -> Vec<Control> {
    let mut controls = Vec::new();
    for letter in letters.chars() {
        let control = match letter {
            'M' => Control::Icon,
            'I' => Control::Minimize,
            'A' => Control::Maximize,
            'X' => Control::Close,
            _ => continue,
        };
        if !controls.contains(&control) {
            controls.push(control);
        }
    }
    controls
}

fn kde_action(value: &str) -> TitleAction {
    match value {
        v if v.starts_with("Maximize") => TitleAction::ToggleMaximize,
        "Minimize" => TitleAction::Minimize,
        "Close" => TitleAction::Close,
        "Operations menu" => TitleAction::Menu,
        _ => TitleAction::Nothing,
    }
}

// ---- Windows ----------------------------------------------------------------

/// Windows' title bar, which has no settings: the icon on the left,
/// Minimize, Maximize and Close on the right; double-click maximizes or
/// restores, right-click opens the window menu (as the system's title bar).
fn windows_frame() -> WindowFrame {
    WindowFrame {
        flavor: Flavor::Windows,
        layout: ButtonLayout {
            left: vec![Control::Icon],
            right: vec![Control::Minimize, Control::Maximize, Control::Close],
        },
        double_click: TitleAction::ToggleMaximize,
        middle_click: TitleAction::Nothing,
        right_click: TitleAction::Menu,
        icon: None,
    }
}

// ---- drawing ----------------------------------------------------------------

/// Sizes of the bar and its buttons, in points.
struct Look {
    height: f32,
    /// The window's corner radius (when not maximized).
    radius: u8,
    /// From the window's left and right edges to the outermost button.
    left_edge: f32,
    right_edge: f32,
    /// A window button's clickable size (the icon's is square, as high as
    /// this), and the gap between two.
    slot: Vec2,
    gap: f32,
}

impl Flavor {
    fn look(self) -> Look {
        match self {
            // libadwaita: a 46-point header bar, 24-point round buttons
            // with 5 points around them, 12-point window corners.
            Flavor::Gnome => Look {
                height: 46.0,
                radius: 12,
                left_edge: 6.0,
                right_edge: 6.0,
                slot: Vec2::splat(34.0),
                gap: 3.0,
            },
            // Breeze: an 18-point button grid, small corners.
            Flavor::Kde => Look {
                height: 32.0,
                radius: 6,
                left_edge: 4.0,
                right_edge: 4.0,
                slot: Vec2::splat(24.0),
                gap: 2.0,
            },
            // Windows 11: a 32-pixel bar; the 16-pixel icon 16 pixels from
            // the left edge (centred in a 32-pixel square 8 from it); 46×32
            // caption buttons side by side against the right edge; 8-pixel
            // window corners.
            Flavor::Windows => Look {
                height: 32.0,
                radius: 8,
                left_edge: 8.0,
                right_edge: 0.0,
                slot: vec2(46.0, 32.0),
                gap: 0.0,
            },
        }
    }
}

/// The resize border inside the window's edges, and how far a corner's
/// diagonal resize reaches along them (points).
const RESIZE_MARGIN: f32 = 5.0;
const RESIZE_CORNER: f32 = 16.0;

/// The window's state as the title bar shows it.
#[derive(Debug, Clone, Copy)]
struct WindowState {
    maximized: bool,
    fullscreen: bool,
    focused: bool,
}

fn window_state(ctx: &Context) -> WindowState {
    ctx.input(|i| {
        let v = i.viewport();
        WindowState {
            maximized: v.maximized.unwrap_or(false),
            fullscreen: v.fullscreen.unwrap_or(false),
            focused: v.focused.unwrap_or(true),
        }
    })
}

impl WindowFrame {
    /// Whether the system still draws the borderless window's shadow,
    /// border and rounded corners: on Windows (DWM, for winit's undecorated
    /// windows with their shadow), not on the Linux desktops.
    fn system_draws_edges(&self) -> bool {
        self.flavor == Flavor::Windows && cfg!(windows)
    }

    /// Whether the window needs a transparent background: the app rounds
    /// its corners and draws its outline (all but Windows' own frame).
    pub fn transparent(&self) -> bool {
        !self.system_draws_edges()
    }

    /// The window's corner radius now: none when maximized or full screen,
    /// or when the system rounds the corners.
    fn radius(&self, ctx: &Context) -> u8 {
        let state = window_state(ctx);
        if state.maximized || state.fullscreen || self.system_draws_edges() {
            0
        } else {
            self.flavor.look().radius
        }
    }

    /// The frame of the panel at the window's top: the panel's fill, the
    /// window's rounded top corners, and no margin above or at the sides
    /// (the title bar places its own buttons).
    pub fn top_panel_frame(&self, ui: &Ui) -> Frame {
        let r = self.radius(ui.ctx());
        Frame::side_top_panel(ui.style())
            .inner_margin(Margin {
                left: 0,
                right: 0,
                top: 0,
                bottom: 2,
            })
            .corner_radius(CornerRadius {
                nw: r,
                ne: r,
                sw: 0,
                se: 0,
            })
    }

    /// `frame` with the window's rounded bottom corners, for the panel at
    /// the window's bottom.
    pub fn bottom_corners(&self, ui: &Ui, frame: Frame) -> Frame {
        let r = self.radius(ui.ctx());
        frame.corner_radius(CornerRadius {
            nw: 0,
            ne: 0,
            sw: r,
            se: r,
        })
    }

    /// The title bar across `ui`'s width: the buttons at both ends, then
    /// `contents` (a menu bar) on the left and `title` in the middle where
    /// it fits. The bar's empty space moves the window.
    pub fn title_bar(&mut self, ui: &mut Ui, title: &str, contents: impl FnOnce(&mut Ui)) {
        let look = self.flavor.look();
        let ctx = ui.ctx().clone();
        let state = window_state(&ctx);
        let rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), look.height));
        // First, so that every widget on the bar is on top of it.
        let bar = ui.interact(
            rect,
            ui.make_persistent_id("title-bar"),
            Sense::click_and_drag(),
        );
        if self.icon.is_none() && self.layout_has(Control::Icon) {
            self.icon = Some(ctx.load_texture(
                "title-bar-icon",
                crate::branding::icon_image(),
                TextureOptions::LINEAR,
            ));
        }

        let inner = Rect::from_min_max(
            rect.min + vec2(look.left_edge, 0.0),
            rect.max - vec2(look.right_edge, 0.0),
        );
        let mut menu_requested = false;
        let layout = self.layout.clone();
        let left = self.controls(ui, inner, &layout.left, false, state, &mut menu_requested);
        let right = self.controls(ui, inner, &layout.right, true, state, &mut menu_requested);

        // The contents between the buttons, centred vertically.
        let gap = 8.0;
        let start = left.map_or(rect.left() + gap, |x| x + gap);
        let end = right.map_or(rect.right() - gap, |x| x - gap);
        let contents_rect = Rect::from_x_y_ranges(start..=end.max(start), rect.y_range());
        let mut contents_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(contents_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        contents_ui.set_clip_rect(contents_rect.intersect(ui.clip_rect()));
        // Windows: everything on the bar fades while the window is inactive.
        let windows = self.flavor == Flavor::Windows;
        if windows && !state.focused {
            contents_ui.multiply_opacity(WINDOWS_INACTIVE);
        }
        let used = egui::MenuBar::new()
            .ui(&mut contents_ui, |ui| ui.horizontal(contents).response.rect)
            .inner;

        // The title: centred on the bar, moved aside or cut short where the
        // contents or the buttons are in the way, left out where nothing fits.
        let min_x = used.right().max(start) + 12.0;
        let max_x = end - 4.0;
        if max_x - min_x > 24.0 {
            let galley = WidgetText::from(title).into_galley(
                ui,
                Some(TextWrapMode::Truncate),
                max_x - min_x,
                TextStyle::Body,
            );
            let width = galley.size().x;
            let x = (rect.center().x - width / 2.0).clamp(min_x, max_x - width);
            let pos = pos2(x, rect.center().y - galley.size().y / 2.0);
            let visuals = ui.visuals();
            let color = match (state.focused, self.flavor) {
                (false, Flavor::Windows) => {
                    visuals.strong_text_color().gamma_multiply(WINDOWS_INACTIVE)
                }
                (false, _) => visuals.weak_text_color(),
                (true, Flavor::Gnome | Flavor::Windows) => visuals.strong_text_color(),
                (true, Flavor::Kde) => visuals.text_color(),
            };
            ui.painter().galley(pos, galley, color);
        }
        ui.advance_cursor_after_rect(rect);

        // The bar's own clicks, as the desktop's settings say.
        let mut action = TitleAction::Nothing;
        if bar.drag_started_by(PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            // The compositor takes the pointer: the release never arrives.
            ctx.stop_dragging();
        } else if bar.double_clicked() {
            action = self.double_click;
        } else if bar.middle_clicked() {
            action = self.middle_click;
        } else if bar.secondary_clicked() {
            action = self.right_click;
        }
        if action == TitleAction::Menu {
            menu_requested = true;
        } else {
            perform(&ctx, action, state);
        }
        let open = if menu_requested {
            Some(SetOpenCommand::Bool(true))
        } else if bar.clicked() {
            Some(SetOpenCommand::Bool(false))
        } else {
            None
        };
        Popup::context_menu(&bar)
            .open_memory(open)
            .show(|ui| window_menu(ui, state));
    }

    fn layout_has(&self, control: Control) -> bool {
        self.layout.left.contains(&control) || self.layout.right.contains(&control)
    }

    /// Lays out `controls` from `inner`'s left (or right) end; returns the
    /// x where they end, if there are any.
    fn controls(
        &self,
        ui: &mut Ui,
        inner: Rect,
        controls: &[Control],
        right: bool,
        state: WindowState,
        menu_requested: &mut bool,
    ) -> Option<f32> {
        if controls.is_empty() {
            return None;
        }
        let look = self.flavor.look();
        let layout = if right {
            Layout::right_to_left(Align::Center)
        } else {
            Layout::left_to_right(Align::Center)
        };
        let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(layout));
        child.spacing_mut().item_spacing.x = look.gap;
        // Right to left: the list reads left to right.
        let ordered: Vec<Control> = if right {
            controls.iter().rev().copied().collect()
        } else {
            controls.to_vec()
        };
        for control in ordered {
            let response = self.button(&mut child, control, state);
            // Windows: double-clicking the icon closes the window.
            if control == Control::Icon
                && self.flavor == Flavor::Windows
                && response.double_clicked()
            {
                perform(ui.ctx(), TitleAction::Close, state);
            } else if response.clicked() {
                match control {
                    Control::Icon => *menu_requested = true,
                    Control::Minimize => perform(ui.ctx(), TitleAction::Minimize, state),
                    Control::Maximize => perform(ui.ctx(), TitleAction::ToggleMaximize, state),
                    Control::Close => perform(ui.ctx(), TitleAction::Close, state),
                }
            }
        }
        let used = child.min_rect();
        Some(if right { used.left() } else { used.right() })
    }

    /// One button, painted in the desktop's look.
    fn button(&self, ui: &mut Ui, control: Control, state: WindowState) -> Response {
        let look = self.flavor.look();
        let s = strings();
        let label = match control {
            Control::Icon => crate::branding::PRODUCT_NAME,
            Control::Minimize => s.window_minimize,
            Control::Maximize if state.maximized => s.window_restore,
            Control::Maximize => s.window_maximize,
            Control::Close => s.window_close,
        };
        // GNOME's icon is only a picture; KDE's opens the window menu.
        let sense = if control == Control::Icon && self.flavor == Flavor::Gnome {
            Sense::hover()
        } else {
            Sense::click()
        };
        let size = if control == Control::Icon {
            Vec2::splat(look.slot.y)
        } else {
            look.slot
        };
        let (rect, mut response) = ui.allocate_exact_size(size, sense);
        let role = if sense == Sense::hover() {
            Role::Image
        } else {
            Role::Button
        };
        response.widget_info(|| WidgetInfo::labeled(role, true, label));
        if matches!(self.flavor, Flavor::Kde | Flavor::Windows) {
            response = response.on_hover_text(label);
        }
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let paint = ButtonPaint {
            rect,
            hovered: response.hovered(),
            pressed: response.is_pointer_button_down_on(),
            state,
            fg: ui.visuals().text_color(),
        };
        if control == Control::Icon {
            if let Some(icon) = &self.icon {
                let size = if self.flavor == Flavor::Kde {
                    18.0
                } else {
                    16.0
                };
                let mut image = Image::from_texture((icon.id(), Vec2::splat(size)));
                if !state.focused {
                    image = image.tint(Color32::from_white_alpha(140));
                }
                image.paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(size)));
            }
        } else {
            match self.flavor {
                Flavor::Gnome => paint.adwaita(ui, control),
                Flavor::Kde => paint.breeze(ui, control),
                Flavor::Windows => {
                    // The Close button's backplate follows the window's
                    // top-right corner (Close is the last button).
                    let corner = if control == Control::Close {
                        self.radius(ui.ctx())
                    } else {
                        0
                    };
                    paint.windows(ui, control, corner);
                }
            }
        }
        response
    }

    /// The window's edges: resize handles (unless maximized or full
    /// screen) and a thin outline (unless the system draws the window's
    /// border). Call after the window's other contents.
    pub fn edges(&self, ctx: &Context) {
        let state = window_state(ctx);
        if state.maximized || state.fullscreen {
            return;
        }
        let window = ctx.viewport_rect();
        if !self.system_draws_edges() {
            let radius = f32::from(self.radius(ctx));
            let outline = if ctx.global_style().visuals.dark_mode {
                Color32::from_white_alpha(28)
            } else {
                Color32::from_black_alpha(48)
            };
            ctx.layer_painter(egui::LayerId::new(
                Order::Foreground,
                Id::unique("window-outline"),
            ))
            .rect_stroke(
                window,
                radius,
                Stroke::new(1.0, outline),
                StrokeKind::Inside,
            );
        }

        let m = RESIZE_MARGIN;
        let strips = [
            Rect::from_min_max(window.min, pos2(window.right(), window.top() + m)),
            Rect::from_min_max(pos2(window.left(), window.bottom() - m), window.max),
            Rect::from_min_max(
                pos2(window.left(), window.top() + m),
                pos2(window.left() + m, window.bottom() - m),
            ),
            Rect::from_min_max(
                pos2(window.right() - m, window.top() + m),
                pos2(window.right(), window.bottom() - m),
            ),
        ];
        for (i, strip) in strips.into_iter().enumerate() {
            Area::new(Id::unique(("window-edge", i)))
                .order(Order::Foreground)
                .fixed_pos(strip.min)
                .constrain(false)
                .movable(false)
                .show(ctx, |ui| {
                    let (_, response) = ui.allocate_exact_size(strip.size(), Sense::drag());
                    let direction = response
                        .hover_pos()
                        .or_else(|| response.interact_pointer_pos())
                        .and_then(|p| resize_direction(window, p, m, RESIZE_CORNER));
                    let Some(direction) = direction else {
                        return;
                    };
                    let response = response.on_hover_cursor(cursor(direction));
                    if response.drag_started_by(PointerButton::Primary) {
                        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
                        ctx.stop_dragging();
                    }
                });
        }
    }
}

/// Carries out a title bar action (not [`TitleAction::Menu`]).
fn perform(ctx: &Context, action: TitleAction, state: WindowState) {
    let command = match action {
        TitleAction::ToggleMaximize => ViewportCommand::Maximized(!state.maximized),
        TitleAction::Minimize => ViewportCommand::Minimized(true),
        TitleAction::Close => ViewportCommand::Close,
        TitleAction::Menu | TitleAction::Nothing => return,
    };
    ctx.send_viewport_cmd(command);
}

/// The window menu: Minimize, Maximize or Restore, Close.
fn window_menu(ui: &mut Ui, state: WindowState) {
    let s = strings();
    let ctx = ui.ctx().clone();
    if ui.button(s.window_minimize).clicked() {
        perform(&ctx, TitleAction::Minimize, state);
        ui.close();
    }
    let maximize = if state.maximized {
        s.window_restore
    } else {
        s.window_maximize
    };
    if ui.button(maximize).clicked() {
        perform(&ctx, TitleAction::ToggleMaximize, state);
        ui.close();
    }
    ui.separator();
    if ui.button(s.window_close).clicked() {
        perform(&ctx, TitleAction::Close, state);
        ui.close();
    }
}

/// Which way a press at `pos` resizes `window`: within `margin` of an
/// edge, diagonally within `corner` of a corner.
fn resize_direction(window: Rect, pos: Pos2, margin: f32, corner: f32) -> Option<ResizeDirection> {
    if !window.contains(pos) {
        return None;
    }
    let near = |d: f32, reach: f32| d < reach;
    let (dn, ds) = (pos.y - window.top(), window.bottom() - pos.y);
    let (dw, de) = (pos.x - window.left(), window.right() - pos.x);
    let (n, s, w, e) = (
        near(dn, margin),
        near(ds, margin),
        near(dw, margin),
        near(de, margin),
    );
    let (cn, cs, cw, ce) = (
        near(dn, corner),
        near(ds, corner),
        near(dw, corner),
        near(de, corner),
    );
    Some(if (n && cw) || (w && cn) {
        ResizeDirection::NorthWest
    } else if (n && ce) || (e && cn) {
        ResizeDirection::NorthEast
    } else if (s && cw) || (w && cs) {
        ResizeDirection::SouthWest
    } else if (s && ce) || (e && cs) {
        ResizeDirection::SouthEast
    } else if n {
        ResizeDirection::North
    } else if s {
        ResizeDirection::South
    } else if w {
        ResizeDirection::West
    } else if e {
        ResizeDirection::East
    } else {
        return None;
    })
}

fn cursor(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::North => CursorIcon::ResizeNorth,
        ResizeDirection::South => CursorIcon::ResizeSouth,
        ResizeDirection::East => CursorIcon::ResizeEast,
        ResizeDirection::West => CursorIcon::ResizeWest,
        ResizeDirection::NorthEast => CursorIcon::ResizeNorthEast,
        ResizeDirection::NorthWest => CursorIcon::ResizeNorthWest,
        ResizeDirection::SouthEast => CursorIcon::ResizeSouthEast,
        ResizeDirection::SouthWest => CursorIcon::ResizeSouthWest,
    }
}

/// What painting a window button needs.
struct ButtonPaint {
    rect: Rect,
    hovered: bool,
    pressed: bool,
    state: WindowState,
    fg: Color32,
}

/// Breeze's colour for the close button under the pointer (Plasma's
/// "negative" red), and pressed.
const BREEZE_NEGATIVE: Color32 = Color32::from_rgb(0xda, 0x44, 0x53);
const BREEZE_NEGATIVE_PRESSED: Color32 = Color32::from_rgb(0xb0, 0x37, 0x43);

/// Windows 11's Close button under the pointer.
const WINDOWS_CLOSE: Color32 = Color32::from_rgb(0xc4, 0x2b, 0x1c);

/// How much of their colour the title, the glyphs and the menus keep on
/// Windows while the window is inactive (Microsoft: every element of the
/// title bar semi-transparent).
const WINDOWS_INACTIVE: f32 = 0.5;

impl ButtonPaint {
    /// Adwaita: a faint round button with a 16-point symbolic icon; the
    /// circle darkens under the pointer and when pressed; everything fades
    /// while the window is in the background.
    fn adwaita(&self, ui: &Ui, control: Control) {
        let painter = ui.painter();
        let fade = if self.state.focused { 1.0 } else { 0.5 };
        let fg = self.fg.gamma_multiply(fade);
        let fill = if self.pressed {
            0.30
        } else if self.hovered {
            0.15
        } else {
            0.10
        };
        let c = self.rect.center();
        painter.circle_filled(c, 12.0, self.fg.gamma_multiply(fill * fade));
        // The icon's 16-point grid.
        let o = c - vec2(8.0, 8.0);
        let p = |x: f32, y: f32| o + vec2(x, y);
        let stroke = Stroke::new(1.5, fg);
        match control {
            Control::Close => {
                painter.line_segment([p(4.0, 4.0), p(12.0, 12.0)], stroke);
                painter.line_segment([p(12.0, 4.0), p(4.0, 12.0)], stroke);
            }
            Control::Minimize => {
                painter.line_segment([p(4.0, 10.5), p(12.0, 10.5)], stroke);
            }
            Control::Maximize if self.state.maximized => {
                // Restore: a square in front of another.
                painter.rect_stroke(
                    Rect::from_min_max(p(4.0, 6.0), p(10.0, 12.0)),
                    1.0,
                    stroke,
                    StrokeKind::Middle,
                );
                painter.line(vec![p(6.0, 4.0), p(12.0, 4.0), p(12.0, 10.0)], stroke);
            }
            Control::Maximize => {
                painter.rect_stroke(
                    Rect::from_min_max(p(4.0, 4.0), p(12.0, 12.0)),
                    1.0,
                    stroke,
                    StrokeKind::Middle,
                );
            }
            Control::Icon => {}
        }
    }

    /// Breeze: thin glyphs on an 18-point grid with no background; a round
    /// background under the pointer (red behind a white cross for Close);
    /// greyed while the window is in the background.
    fn breeze(&self, ui: &Ui, control: Control) {
        let painter = ui.painter();
        let close = control == Control::Close;
        let active = self.hovered || self.pressed;
        let size = 20.0;
        let c = self.rect.center();
        if active {
            let fill = match (close, self.pressed) {
                (true, false) => BREEZE_NEGATIVE,
                (true, true) => BREEZE_NEGATIVE_PRESSED,
                (false, false) => self.fg.gamma_multiply(0.15),
                (false, true) => self.fg.gamma_multiply(0.30),
            };
            painter.circle_filled(c, size / 2.0, fill);
        }
        let fg = if close && active {
            Color32::WHITE
        } else if self.state.focused {
            self.fg
        } else {
            self.fg.gamma_multiply(0.55)
        };
        let scale = size / 18.0;
        let o = c - Vec2::splat(size / 2.0);
        let p = |x: f32, y: f32| o + vec2(x, y) * scale;
        let stroke = Stroke::new(1.2, fg);
        match control {
            Control::Close => {
                painter.line_segment([p(5.0, 5.0), p(13.0, 13.0)], stroke);
                painter.line_segment([p(13.0, 5.0), p(5.0, 13.0)], stroke);
            }
            Control::Minimize => {
                painter.line(vec![p(4.0, 7.0), p(9.0, 12.0), p(14.0, 7.0)], stroke);
            }
            Control::Maximize if self.state.maximized => {
                painter.add(egui::Shape::closed_line(
                    vec![p(4.5, 9.0), p(9.0, 4.5), p(13.5, 9.0), p(9.0, 13.5)],
                    stroke,
                ));
            }
            Control::Maximize => {
                painter.line(vec![p(4.0, 11.0), p(9.0, 6.0), p(14.0, 11.0)], stroke);
            }
            Control::Icon => {}
        }
    }

    /// Windows 11: a full-height backplate, invisible at rest; under the
    /// pointer a subtle fill (Close: red with a white glyph); pressed, a
    /// lighter fill and a dimmer glyph, as Fluent's subtle buttons. The
    /// glyphs are Segoe Fluent Icons' ChromeMinimize, ChromeMaximize,
    /// ChromeRestore and ChromeClose on their 10-point grid, faded while the
    /// window is inactive. `corner` rounds the backplate's top-right corner.
    fn windows(&self, ui: &Ui, control: Control, corner: u8) {
        let painter = ui.painter();
        let visuals = ui.visuals();
        let close = control == Control::Close;
        let subtle = |alpha| {
            if visuals.dark_mode {
                Color32::from_white_alpha(alpha)
            } else {
                Color32::from_black_alpha(alpha)
            }
        };
        let fill = match (close, self.hovered, self.pressed) {
            (_, false, false) => None,
            (true, _, false) => Some(WINDOWS_CLOSE),
            (true, _, true) => Some(WINDOWS_CLOSE.gamma_multiply(0.9)),
            (false, _, false) => Some(subtle(0x17)),
            (false, _, true) => Some(subtle(0x0f)),
        };
        if let Some(fill) = fill {
            let corners = CornerRadius {
                nw: 0,
                ne: corner,
                sw: 0,
                se: 0,
            };
            painter.rect_filled(self.rect, corners, fill);
        }
        let fg = match (close && fill.is_some(), self.pressed) {
            (true, false) => Color32::WHITE,
            (true, true) => Color32::WHITE.gamma_multiply(0.7),
            (false, true) => visuals.strong_text_color().gamma_multiply(0.62),
            (false, false) if self.hovered || self.state.focused => visuals.strong_text_color(),
            (false, false) => visuals.strong_text_color().gamma_multiply(WINDOWS_INACTIVE),
        };
        // The 10-point glyph box, its lines on pixel centres.
        let c = self.rect.center();
        let o = pos2(
            painter.round_to_pixel_center(c.x - 5.0),
            painter.round_to_pixel_center(c.y - 5.0),
        );
        let p = |x: f32, y: f32| o + vec2(x, y);
        let stroke = Stroke::new(1.0, fg);
        match control {
            Control::Minimize => {
                painter.line_segment([p(0.0, 5.0), p(10.0, 5.0)], stroke);
            }
            Control::Maximize if self.state.maximized => {
                // Restore: a square in front, the top-right of another
                // behind it.
                painter.rect_stroke(
                    Rect::from_min_max(p(0.0, 2.0), p(8.0, 10.0)),
                    1.0,
                    stroke,
                    StrokeKind::Middle,
                );
                painter.line(
                    vec![
                        p(2.0, 2.0),
                        p(2.0, 1.0),
                        p(3.0, 0.0),
                        p(9.0, 0.0),
                        p(10.0, 1.0),
                        p(10.0, 7.0),
                        p(9.0, 8.0),
                        p(8.0, 8.0),
                    ],
                    stroke,
                );
            }
            Control::Maximize => {
                painter.rect_stroke(
                    Rect::from_min_max(p(0.0, 0.0), p(10.0, 10.0)),
                    1.0,
                    stroke,
                    StrokeKind::Middle,
                );
            }
            Control::Close => {
                painter.line_segment([p(0.0, 0.0), p(10.0, 10.0)], stroke);
                painter.line_segment([p(10.0, 0.0), p(0.0, 10.0)], stroke);
            }
            Control::Icon => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktops_are_recognised_in_the_list() {
        assert_eq!(flavor_of("GNOME"), Some(Flavor::Gnome));
        assert_eq!(flavor_of("ubuntu:GNOME"), Some(Flavor::Gnome));
        assert_eq!(flavor_of("KDE"), Some(Flavor::Kde));
        assert_eq!(flavor_of("X-Cinnamon"), None);
        assert_eq!(flavor_of("sway"), None);
        assert_eq!(flavor_of(""), None);
    }

    #[test]
    fn gnome_layouts() {
        use Control::*;
        let l = gnome_layout("appmenu:minimize,close");
        assert_eq!((l.left, l.right), (vec![], vec![Minimize, Close]));
        let l = gnome_layout("close,minimize,maximize:");
        assert_eq!((l.left, l.right), (vec![Close, Minimize, Maximize], vec![]));
        let l = gnome_layout("icon:minimize,maximize,close");
        assert_eq!(
            (l.left, l.right),
            (vec![Icon], vec![Minimize, Maximize, Close])
        );
        // No colon: everything on the left; a button only once.
        let l = gnome_layout("close,spacer,close");
        assert_eq!((l.left, l.right), (vec![Close], vec![]));
        let l = gnome_layout("close:close");
        assert_eq!((l.left, l.right), (vec![Close], vec![]));
    }

    #[test]
    fn gnome_settings_are_read_from_gsettings_output() {
        let listing = "\
org.gnome.desktop.wm.preferences action-double-click-titlebar 'minimize'
org.gnome.desktop.wm.preferences action-middle-click-titlebar 'lower'
org.gnome.desktop.wm.preferences action-right-click-titlebar 'menu'
org.gnome.desktop.wm.preferences button-layout 'appmenu:minimize,maximize,close'
org.gnome.desktop.wm.preferences titlebar-font 'Cantarell Bold 11'
";
        let f = gnome_frame(listing);
        assert_eq!(f.flavor, Flavor::Gnome);
        assert_eq!(
            f.layout.right,
            vec![Control::Minimize, Control::Maximize, Control::Close]
        );
        assert_eq!(f.double_click, TitleAction::Minimize);
        assert_eq!(f.middle_click, TitleAction::Nothing);
        assert_eq!(f.right_click, TitleAction::Menu);

        // Without gsettings: GNOME's defaults.
        let f = WindowFrame::new(Flavor::Gnome);
        assert_eq!(
            (f.layout.left, f.layout.right),
            (vec![], vec![Control::Close])
        );
        assert_eq!(f.double_click, TitleAction::ToggleMaximize);
        assert_eq!(f.middle_click, TitleAction::Nothing);
        assert_eq!(f.right_click, TitleAction::Menu);
    }

    #[test]
    fn kde_settings_are_read_from_kwinrc() {
        use Control::*;
        // Plasma's defaults.
        let f = WindowFrame::new(Flavor::Kde);
        assert_eq!(
            (f.layout.left, f.layout.right),
            (vec![Icon], vec![Minimize, Maximize, Close])
        );
        assert_eq!(f.double_click, TitleAction::ToggleMaximize);
        assert_eq!(f.right_click, TitleAction::Menu);

        let kwinrc = "\
[Desktops]
Number=4

[MouseBindings]
CommandActiveTitlebar2=Close

[Windows]
TitlebarDoubleClickCommand=Shade

[org.kde.kdecoration2]
BorderSize=Normal
ButtonsOnLeft[$i]=XAI
ButtonsOnRight=HSM
";
        let f = kde_frame(kwinrc);
        assert_eq!(
            (f.layout.left, f.layout.right),
            (vec![Close, Maximize, Minimize], vec![Icon])
        );
        assert_eq!(f.double_click, TitleAction::Nothing);
        assert_eq!(f.middle_click, TitleAction::Close);
        assert_eq!(f.right_click, TitleAction::Menu);
        // A later file (the user's) overrides an earlier one (the system's).
        let both = format!("{kwinrc}\n[org.kde.kdecoration2]\nButtonsOnLeft=\n");
        let f = kde_frame(&both);
        assert_eq!(f.layout.left, vec![]);
        assert_eq!(f.layout.right, vec![Icon]);
    }

    /// The commands the last frame sent to the window.
    fn commands(h: &egui_kittest::Harness<'_>) -> Vec<ViewportCommand> {
        h.output()
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.commands.clone())
            .unwrap_or_default()
    }

    #[test]
    fn the_buttons_minimize_maximize_and_close_the_window() {
        use egui_kittest::kittest::Queryable;
        let s = strings();
        let mut frame = WindowFrame::new(Flavor::Gnome);
        frame.layout.right = vec![Control::Minimize, Control::Maximize, Control::Close];
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(700.0, 80.0))
            .build_ui(move |ui| {
                frame.title_bar(ui, "Title", |ui| {
                    ui.label("Menus");
                });
            });
        h.run();
        // Left to right, as the layout lists them.
        let x = |h: &egui_kittest::Harness<'_>, label| h.get_by_label(label).rect().min.x;
        assert!(x(&h, s.window_minimize) < x(&h, s.window_maximize));
        assert!(x(&h, s.window_maximize) < x(&h, s.window_close));
        assert!(x(&h, "Menus") < x(&h, s.window_minimize));
        for (label, command) in [
            (s.window_minimize, ViewportCommand::Minimized(true)),
            (s.window_maximize, ViewportCommand::Maximized(true)),
            (s.window_close, ViewportCommand::Close),
        ] {
            h.get_by_label(label).click();
            h.step();
            assert_eq!(commands(&h), vec![command], "{label}");
            h.run();
        }
    }

    #[test]
    fn kde_icon_opens_the_window_menu() {
        use egui_kittest::kittest::Queryable;
        let s = strings();
        let mut frame = WindowFrame::new(Flavor::Kde);
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(700.0, 80.0))
            .build_ui(move |ui| frame.title_bar(ui, "Title", |_| {}));
        h.run();
        assert_eq!(h.query_all_by_label(s.window_close).count(), 1);
        h.get_by_label(crate::branding::PRODUCT_NAME).click();
        h.run();
        // The menu's Close under the bar's.
        assert_eq!(h.query_all_by_label(s.window_close).count(), 2);
        let close = h
            .query_all_by_label(s.window_close)
            .max_by(|a, b| a.rect().min.y.total_cmp(&b.rect().min.y))
            .expect("the menu's Close");
        close.click();
        h.step();
        assert_eq!(commands(&h), vec![ViewportCommand::Close]);
    }

    #[test]
    fn the_variable_or_the_platform_chooses_the_flavor() {
        let windows = || Some(Flavor::Windows);
        assert_eq!(chosen_flavor("", windows), Some(Flavor::Windows));
        assert_eq!(chosen_flavor("", || None), None);
        assert_eq!(chosen_flavor("system", windows), None);
        assert_eq!(chosen_flavor(" Windows ", || None), Some(Flavor::Windows));
        assert_eq!(chosen_flavor("gnome", windows), Some(Flavor::Gnome));
        assert_eq!(chosen_flavor("KDE", windows), Some(Flavor::Kde));
        // An unknown value keeps the system's frame.
        assert_eq!(chosen_flavor("aqua", windows), None);
        if cfg!(windows) {
            assert_eq!(platform_flavor(), Some(Flavor::Windows));
        }
    }

    #[test]
    fn windows_has_fixed_buttons_and_clicks() {
        use Control::*;
        let f = WindowFrame::new(Flavor::Windows);
        assert_eq!(
            (f.layout.left.clone(), f.layout.right.clone()),
            (vec![Icon], vec![Minimize, Maximize, Close])
        );
        assert_eq!(f.double_click, TitleAction::ToggleMaximize);
        assert_eq!(f.middle_click, TitleAction::Nothing);
        assert_eq!(f.right_click, TitleAction::Menu);
        // On Windows the system rounds the corners of an opaque window; the
        // look forced elsewhere rounds them itself.
        assert_eq!(f.transparent(), !cfg!(windows));
        assert!(WindowFrame::new(Flavor::Gnome).transparent());
        assert!(WindowFrame::new(Flavor::Kde).transparent());
    }

    /// A harness drawing `frame`'s title bar with "Menus" as its contents,
    /// 800 points wide (the bar is 784: kittest adds an 8-point margin).
    /// Frames are 50 ms apart, so two clicks make a double-click.
    fn title_bar_harness(mut frame: WindowFrame) -> egui_kittest::Harness<'static> {
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(800.0, 80.0))
            .with_step_dt(0.05)
            .build_ui(move |ui| {
                frame.title_bar(ui, "Title", |ui| {
                    ui.label("Menus");
                });
            });
        h.run();
        h
    }

    #[test]
    fn windows_caption_buttons_are_46_by_32_against_the_right_edge() {
        use egui_kittest::kittest::Queryable;
        let s = strings();
        let mut h = title_bar_harness(WindowFrame::new(Flavor::Windows));
        let rect = |h: &egui_kittest::Harness<'_>, label| h.get_by_label(label).rect();
        let (min, max, close) = (
            rect(&h, s.window_minimize),
            rect(&h, s.window_maximize),
            rect(&h, s.window_close),
        );
        for r in [min, max, close] {
            assert_eq!(r.size(), vec2(46.0, 32.0));
            assert_eq!(r.top(), 8.0, "full height, from the bar's top");
        }
        // Side by side, Close against the bar's right edge.
        assert_eq!(min.right(), max.left());
        assert_eq!(max.right(), close.left());
        assert_eq!(close.right(), 792.0);
        // The icon: 16 points from the bar's left edge, the menus after it.
        let icon = rect(&h, crate::branding::PRODUCT_NAME);
        assert_eq!(icon.center().x - 8.0, 16.0 + 8.0);
        assert!(icon.right() < rect(&h, "Menus").left());
        assert!(rect(&h, "Menus").right() < min.left());

        for (label, command) in [
            (s.window_minimize, ViewportCommand::Minimized(true)),
            (s.window_maximize, ViewportCommand::Maximized(true)),
            (s.window_close, ViewportCommand::Close),
        ] {
            h.get_by_label(label).click();
            h.step();
            assert_eq!(commands(&h), vec![command], "{label}");
            h.run();
        }
    }

    #[test]
    fn windows_maximized_shows_restore() {
        use egui_kittest::kittest::Queryable;
        let s = strings();
        let mut h = title_bar_harness(WindowFrame::new(Flavor::Windows));
        h.input_mut()
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .maximized = Some(true);
        h.run();
        assert!(h.query_by_label(s.window_maximize).is_none());
        h.get_by_label(s.window_restore).click();
        h.step();
        assert_eq!(commands(&h), vec![ViewportCommand::Maximized(false)]);
    }

    /// Two clicks at `pos`, one frame per event (50 ms apart).
    fn double_click_at(h: &mut egui_kittest::Harness<'_>, pos: Pos2) {
        h.hover_at(pos);
        for pressed in [true, false, true, false] {
            h.event(egui::Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        h.step();
    }

    #[test]
    fn windows_double_clicks_maximize_on_the_bar_and_close_on_the_icon() {
        use egui_kittest::kittest::Queryable;
        let s = strings();
        let mut h = title_bar_harness(WindowFrame::new(Flavor::Windows));
        // The bar's empty space (the title is not in the way).
        double_click_at(&mut h, pos2(300.0, 20.0));
        assert_eq!(commands(&h), vec![ViewportCommand::Maximized(true)]);
        h.run();

        // One click on the icon opens the window menu, two close the window.
        h.get_by_label(crate::branding::PRODUCT_NAME).click();
        h.run();
        assert_eq!(h.query_all_by_label(s.window_close).count(), 2);
        h.key_press(egui::Key::Escape);
        h.run();
        let icon = h
            .get_by_label(crate::branding::PRODUCT_NAME)
            .rect()
            .center();
        double_click_at(&mut h, icon);
        assert_eq!(commands(&h), vec![ViewportCommand::Close]);
    }

    #[test]
    fn edges_resize_and_corners_resize_diagonally() {
        use ResizeDirection::*;
        let w = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let at = |x, y| resize_direction(w, pos2(x, y), 5.0, 16.0);
        assert_eq!(at(400.0, 300.0), None);
        assert_eq!(at(400.0, 1.0), Some(North));
        assert_eq!(at(400.0, 598.0), Some(South));
        assert_eq!(at(2.0, 300.0), Some(West));
        assert_eq!(at(799.0, 300.0), Some(East));
        assert_eq!(at(10.0, 2.0), Some(NorthWest));
        assert_eq!(at(2.0, 10.0), Some(NorthWest));
        assert_eq!(at(790.0, 2.0), Some(NorthEast));
        assert_eq!(at(2.0, 590.0), Some(SouthWest));
        assert_eq!(at(798.0, 598.0), Some(SouthEast));
        assert_eq!(at(10.0, 10.0), None);
        assert_eq!(at(900.0, 10.0), None);
    }
}
