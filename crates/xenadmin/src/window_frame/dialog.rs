//! The dialogs in the desktop's look (this console's own; the original's
//! dialogs were Windows Forms windows in the system's frame).
//!
//! The dialogs stay inside the main window as egui windows (on Wayland a
//! window of its own can be neither centred on its parent nor attached to
//! it). With the app's own title bar ([`WindowFrame`]) they take the same
//! desktop's look, as the desktops' own in-window dialogs do:
//!
//! - GNOME: libadwaita's floating dialog. A card with 15-point corners, a
//!   flat 46-point header with the title in the middle and Adwaita's round
//!   close button where `button-layout` puts the window's close button.
//! - KDE Plasma: a dialog in Breeze's decoration. A 32-point title bar over
//!   a separator, the title in the middle and Breeze's close glyph (red
//!   under the pointer) where `kwinrc` puts the close button.
//! - Windows: WinUI's content dialog in the window's look. The window's
//!   8-point corners, a 32-point header with the title on the left and
//!   Windows 11's Close button in the top-right corner (red under the
//!   pointer), over a darker smoke layer.
//!
//! The window behind a dialog is dimmed and takes no clicks or keys while
//! the dialog is open, as the parent of a modal dialog on these desktops;
//! its title bar still moves it and its edges still resize it. Without the
//! app's own title bar (other desktops and platforms, or
//! `REXENCENTER_TITLE_BAR=system`) the dialogs keep egui's look and do not
//! dim the window.
//!
//! Every dialog keeps its id, its size (the area its contents get is the
//! same in every look), its flags and its buttons. A dialog has a close
//! button only when it is given `open`, as in egui's look. A collapsible
//! window (the error list) keeps egui's look everywhere: only egui's title
//! bar folds it.

use egui::{
    Align2, Area, Color32, Context, CornerRadius, Frame, Id, InnerResponse, LayerId, Margin, Order,
    PointerButton, Pos2, Rect, Response, Role, Sense, Stroke, TextStyle, TextWrapMode, Ui, UiKind,
    Vec2, ViewportCommand, WidgetInfo, WidgetText, pos2, vec2,
};

use super::{ButtonPaint, Control, Flavor, WindowFrame, outline_color, window_state};
use crate::i18n::strings;

/// What the dialogs look like this frame: set by [`style_dialogs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DialogLook {
    flavor: Flavor,
    /// The close button at the header's left end (where the window's is).
    close_left: bool,
}

fn look_id() -> Id {
    Id::unique("rexen-dialog-look")
}

/// Makes the dialogs take `frame`'s look, or egui's with `None`. The app
/// calls it every frame, before it shows any dialog.
pub fn style_dialogs(ctx: &Context, frame: Option<&WindowFrame>) {
    let look = frame.map(|f| DialogLook {
        flavor: f.flavor,
        close_left: f.layout.left.contains(&Control::Close),
    });
    ctx.data_mut(|d| d.insert_temp(look_id(), look));
}

fn dialog_look(ctx: &Context) -> Option<DialogLook> {
    ctx.data(|d| d.get_temp::<Option<DialogLook>>(look_id()))
        .flatten()
}

/// The parts of a dialog that differ between the desktops.
struct DialogSpec {
    /// The card's corner radius.
    radius: u8,
    /// Around the contents, under the header.
    margin: Margin,
    /// Over the window behind the dialog, in the light and the dark theme.
    dim_light: Color32,
    dim_dark: Color32,
    /// A line under the header.
    separator: bool,
    /// The title at the header's left, in line with the contents, instead
    /// of in the middle.
    left_title: bool,
}

impl Flavor {
    fn dialog(self) -> DialogSpec {
        match self {
            // libadwaita: `$dialog_radius` (15px) and the dimming behind a
            // floating sheet, twice `--shade-color`: rgb(0 0 6) at 14% in
            // the light style and 50% in the dark one. Its header bar is
            // flat; the contents keep 12 points from the card's edges.
            Flavor::Gnome => DialogSpec {
                radius: 15,
                margin: Margin {
                    left: 12,
                    right: 12,
                    top: 0,
                    bottom: 12,
                },
                dim_light: Color32::from_rgba_unmultiplied(0, 0, 6, 36),
                dim_dark: Color32::from_rgba_unmultiplied(0, 0, 6, 128),
                separator: false,
                left_title: false,
            },
            // Breeze: the window's own corners and a title bar over the
            // contents, which keep Breeze's 6-point layout margins (a little
            // more under the bar).
            Flavor::Kde => DialogSpec {
                radius: self.look().radius,
                margin: Margin {
                    left: 8,
                    right: 8,
                    top: 6,
                    bottom: 8,
                },
                dim_light: Color32::from_black_alpha(48),
                dim_dark: Color32::from_black_alpha(96),
                separator: true,
                left_title: false,
            },
            // WinUI's `ContentDialog`: the window's 8-point corners and its
            // smoke layer, black at about 30% in the light theme and 50% in
            // the dark one. The header is the title bar's height, with the
            // title on the left and the Close caption button in the corner.
            Flavor::Windows => DialogSpec {
                radius: self.look().radius,
                margin: Margin {
                    left: 12,
                    right: 12,
                    top: 4,
                    bottom: 12,
                },
                dim_light: Color32::from_black_alpha(77),
                dim_dark: Color32::from_black_alpha(128),
                separator: false,
                left_title: true,
            },
        }
    }
}

/// A dialog: an [`egui::Window`] built the same way, drawn in the
/// desktop's look when the app draws its own title bar (see the module's
/// documentation). The settings mean what they mean on [`egui::Window`];
/// the sizes are the window's in egui's look, and the other looks keep the
/// area its contents get.
#[must_use = "call .show() to draw the dialog"]
pub struct DialogWindow<'open> {
    title: String,
    window: egui::Window<'open>,
    open: Option<&'open mut bool>,
    collapsible: bool,
    resizable: bool,
    margin: Option<Margin>,
    fixed_size: Option<Vec2>,
    default_width: Option<f32>,
    min_width: Option<f32>,
    max_width: Option<f32>,
}

/// A dialog titled `title` (egui's [`egui::Window::new`], in the desktop's
/// look).
pub fn dialog_window<'open>(title: impl Into<String>) -> DialogWindow<'open> {
    let title = title.into();
    DialogWindow {
        window: egui::Window::new(title.clone()),
        title,
        open: None,
        // egui's defaults.
        collapsible: true,
        resizable: true,
        margin: None,
        fixed_size: None,
        default_width: None,
        min_width: None,
        max_width: None,
    }
}

impl<'open> DialogWindow<'open> {
    pub fn id(mut self, id: Id) -> Self {
        self.window = self.window.id(id);
        self
    }

    /// A close button that clears `open` (egui's X, or the desktop's).
    pub fn open(mut self, open: &'open mut bool) -> Self {
        self.open = Some(open);
        self
    }

    pub fn collapsible(mut self, collapsible: bool) -> Self {
        self.collapsible = collapsible;
        self.window = self.window.collapsible(collapsible);
        self
    }

    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self.window = self.window.resizable(resizable);
        self
    }

    pub fn anchor(mut self, align: Align2, offset: impl Into<Vec2>) -> Self {
        self.window = self.window.anchor(align, offset);
        self
    }

    pub fn pivot(mut self, pivot: Align2) -> Self {
        self.window = self.window.pivot(pivot);
        self
    }

    pub fn default_pos(mut self, pos: impl Into<Pos2>) -> Self {
        self.window = self.window.default_pos(pos);
        self
    }

    /// The space around the contents (egui's window margin by default; the
    /// desktop's in its look).
    pub fn inner_margin(mut self, margin: Margin) -> Self {
        self.margin = Some(margin);
        self
    }

    pub fn fixed_size(mut self, size: impl Into<Vec2>) -> Self {
        self.fixed_size = Some(size.into());
        self
    }

    pub fn default_width(mut self, width: f32) -> Self {
        self.default_width = Some(width);
        self
    }

    pub fn min_width(mut self, width: f32) -> Self {
        self.min_width = Some(width);
        self
    }

    pub fn max_width(mut self, width: f32) -> Self {
        self.max_width = Some(width);
        self
    }

    /// Draws the dialog. Returns what [`egui::Window::show`] does.
    pub fn show<R>(
        self,
        ctx: &Context,
        add_contents: impl FnOnce(&mut Ui) -> R,
    ) -> Option<InnerResponse<Option<R>>> {
        match dialog_look(ctx) {
            Some(look) if !self.collapsible => self.show_in(ctx, look, add_contents),
            _ => self.show_plain(ctx, add_contents),
        }
    }

    /// egui's look, as the dialogs had before.
    fn show_plain<R>(
        self,
        ctx: &Context,
        add_contents: impl FnOnce(&mut Ui) -> R,
    ) -> Option<InnerResponse<Option<R>>> {
        let mut window = self.window;
        if let Some(margin) = self.margin {
            window = window.frame(Frame::window(&ctx.global_style()).inner_margin(margin));
        }
        if let Some(open) = self.open {
            window = window.open(open);
        }
        if let Some(size) = self.fixed_size {
            window = window.fixed_size(size);
        }
        if let Some(width) = self.default_width {
            window = window.default_width(width);
        }
        if let Some(width) = self.min_width {
            window = window.min_width(width);
        }
        if let Some(width) = self.max_width {
            window = window.max_width(width);
        }
        window.show(ctx, add_contents)
    }

    /// The desktop's look.
    fn show_in<R>(
        self,
        ctx: &Context,
        look: DialogLook,
        add_contents: impl FnOnce(&mut Ui) -> R,
    ) -> Option<InnerResponse<Option<R>>> {
        let spec = look.flavor.dialog();
        let bar = look.flavor.look();
        let style = ctx.global_style();
        let margin = self.margin.unwrap_or(spec.margin);
        let stroke = Stroke::new(1.0, outline_color(style.visuals.dark_mode));
        let frame = Frame::window(&style)
            .inner_margin(0)
            .corner_radius(spec.radius)
            .stroke(stroke);

        // The window's size around the area its contents get, in egui's
        // look (stroke, title bar, margins) and in this one (stroke, header,
        // margins): the sizes given for egui's look grow by the difference.
        let plain_margin = self.margin.unwrap_or(style.spacing.window_margin);
        let plain_stroke = style.visuals.window_stroke().width;
        let heading = ctx.fonts_mut(|f| f.row_height(&TextStyle::Heading.resolve(&style)));
        let plain = plain_margin.sum()
            + Vec2::splat(2.0 * plain_stroke)
            + vec2(0.0, heading + plain_margin.sum().y + plain_stroke);
        let styled = margin.sum()
            + Vec2::splat(2.0 * stroke.width)
            + vec2(0.0, bar.height + style.spacing.item_spacing.y);
        let grow = styled - plain;

        let mut window = self.window.title_bar(false).frame(frame);
        let closable = self.open.is_some();
        let showing = self.open.as_deref().is_none_or(|open| *open);
        if let Some(open) = self.open {
            window = window.open(open);
        }
        if let Some(size) = self.fixed_size {
            window = window.fixed_size(size + grow);
        }
        if let Some(width) = self.default_width {
            window = window.default_width(width + grow.x);
        }
        if let Some(width) = self.min_width {
            window = window.min_width(width + grow.x);
        }
        if let Some(width) = self.max_width {
            window = window.max_width(width + grow.x);
        }
        let auto_sized = self.fixed_size.is_none() && !self.resizable;
        if showing {
            backdrop(ctx, look, &spec);
        }

        let title = self.title;
        window.show(ctx, |ui| {
            let top_left = ui.max_rect().min;
            // The header's place; it is drawn once the contents' width is
            // known. An auto-sized dialog is at least wide enough for the
            // title between two buttons' room.
            let min_width = if auto_sized {
                let galley = WidgetText::from(title.as_str()).into_galley(
                    ui,
                    Some(TextWrapMode::Extend),
                    f32::INFINITY,
                    TextStyle::Body,
                );
                let edge = bar.left_edge.max(bar.right_edge);
                galley.size().x + 2.0 * (edge + bar.slot.x + 8.0)
            } else {
                0.0
            };
            ui.allocate_exact_size(vec2(min_width, bar.height), Sense::hover());
            let inner = Frame::NONE
                .inner_margin(margin)
                .show(ui, add_contents)
                .inner;
            let width = if auto_sized {
                ui.min_rect().width()
            } else {
                ui.max_rect().width()
            };
            let rect = Rect::from_min_size(top_left, vec2(width, bar.height));
            header(ui, look, &spec, rect, &title, closable);
            inner
        })
    }
}

/// Paints the header in `rect`: the title in the middle and, for a dialog
/// that can be closed, the close button at the window's close button's end.
fn header(
    ui: &mut Ui,
    look: DialogLook,
    spec: &DialogSpec,
    rect: Rect,
    title: &str,
    closable: bool,
) {
    let bar = look.flavor.look();
    let state = window_state(ui.ctx());
    if spec.separator {
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        ui.painter().hline(rect.x_range(), rect.bottom(), stroke);
    }

    // A centred title keeps clear of the button on both sides, so that it
    // stays in the middle; a title on the left keeps clear of it on the
    // right.
    let mut room = bar.left_edge.max(bar.right_edge) + 8.0;
    let mut right_room = 8.0;
    if closable {
        let x = if look.close_left {
            rect.left() + bar.left_edge + bar.slot.x / 2.0
        } else {
            rect.right() - bar.right_edge - bar.slot.x / 2.0
        };
        let slot = Rect::from_center_size(pos2(x, rect.center().y), bar.slot);
        let response = close_button(ui, look.flavor, spec.radius, slot, state);
        if response.clicked() {
            ui.close_kind(UiKind::Window);
        }
        room += bar.slot.x;
        right_room += bar.right_edge + bar.slot.x;
    }

    let left = rect.left() + f32::from(spec.margin.left);
    let width = if spec.left_title {
        rect.right() - right_room - left
    } else {
        rect.width() - 2.0 * room
    };
    if width > 24.0 {
        let galley = WidgetText::from(title).into_galley(
            ui,
            Some(TextWrapMode::Truncate),
            width,
            TextStyle::Body,
        );
        let pos = if spec.left_title {
            pos2(left, rect.center().y - galley.size().y / 2.0)
        } else {
            rect.center() - galley.size() / 2.0
        };
        let visuals = ui.visuals();
        let color = match (state.focused, look.flavor) {
            (false, _) => visuals.weak_text_color(),
            (true, Flavor::Gnome | Flavor::Windows) => visuals.strong_text_color(),
            (true, Flavor::Kde) => visuals.text_color(),
        };
        ui.painter().galley(pos, galley, color);
    }
}

/// The header's close button in `slot`, painted as the title bar's;
/// `corner` is the dialog's radius, for Windows' button in the corner.
fn close_button(
    ui: &mut Ui,
    flavor: Flavor,
    corner: u8,
    slot: Rect,
    state: super::WindowState,
) -> Response {
    let label = strings().dialog_close;
    let mut response = ui.interact(slot, ui.scope_id().with("dialog-close"), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), label));
    if matches!(flavor, Flavor::Kde | Flavor::Windows) {
        response = response.on_hover_text(label);
    }
    let paint = ButtonPaint {
        rect: slot,
        hovered: response.hovered(),
        pressed: response.is_pointer_button_down_on(),
        state,
        fg: ui.visuals().text_color(),
    };
    match flavor {
        Flavor::Gnome => paint.adwaita(ui, Control::Close),
        Flavor::Kde => paint.breeze(ui, Control::Close),
        Flavor::Windows => paint.windows(ui, Control::Close, corner),
    }
    response
}

/// Dims the window behind the dialogs and keeps clicks and keys from it,
/// once a frame however many dialogs are open. The dialogs, menus, tooltips
/// and the window's edges stay above it; a drag on the dimmed title bar
/// still moves the window.
fn backdrop(ctx: &Context, look: DialogLook, spec: &DialogSpec) {
    let id = Id::unique("rexen-dialog-backdrop");
    let pass = ctx.cumulative_pass_nr();
    let first = ctx.data_mut(|d| {
        let last = d.get_temp_mut_or(id, u64::MAX);
        let first = *last != pass;
        *last = pass;
        first
    });
    if !first {
        return;
    }
    let window = ctx.viewport_rect();
    let dim = if ctx.global_style().visuals.dark_mode {
        spec.dim_dark
    } else {
        spec.dim_light
    };
    let radius = CornerRadius::same(super::window_radius(look.flavor, ctx));
    let bar = look.flavor.look();
    let area = Area::new(id)
        .order(Order::Background)
        .fixed_pos(window.min)
        .constrain(false)
        .movable(false)
        .interactable(true)
        .show(ctx, |ui| {
            let (_, response) = ui.allocate_exact_size(window.size(), Sense::click_and_drag());
            ui.painter().rect_filled(window, radius, dim);
            response
        });
    // Right above the panels, under every window.
    ctx.set_sublayer(LayerId::background(), area.response.layer_id);
    ctx.memory_mut(|m| m.set_modal_layer(area.response.layer_id));
    let response = area.inner;
    let on_bar = response
        .interact_pointer_pos()
        .is_some_and(|p| p.y < window.top() + bar.height);
    if on_bar && response.drag_started_by(PointerButton::Primary) {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        ctx.stop_dragging();
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    const SIZE: Vec2 = vec2(900.0, 700.0);

    /// A dialog titled "Title" with a body that records the area it gets;
    /// `open` gives it a close button.
    struct Shown {
        frame: Option<WindowFrame>,
        open: Option<bool>,
        fixed: Option<Vec2>,
        margin: Option<Margin>,
        body: Rect,
        behind_clicks: usize,
    }

    fn shown(frame: Option<WindowFrame>, open: Option<bool>) -> Shown {
        Shown {
            frame,
            open,
            fixed: None,
            margin: None,
            body: Rect::NOTHING,
            behind_clicks: 0,
        }
    }

    fn harness(state: Shown) -> Harness<'static, Shown> {
        let mut h = Harness::builder().with_size(SIZE).build_ui_state(
            |ui, s: &mut Shown| {
                let ctx = &ui.ctx().clone();
                style_dialogs(ctx, s.frame.as_ref());
                egui::CentralPanel::default().show(ui, |ui| {
                    if ui.button("Behind").clicked() {
                        s.behind_clicks += 1;
                    }
                });
                let mut open = s.open.unwrap_or(true);
                let mut dialog = dialog_window("Title")
                    .id(Id::unique("test-dialog"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, [0.0, 0.0]);
                if s.open.is_some() {
                    dialog = dialog.open(&mut open);
                }
                if let Some(size) = s.fixed {
                    dialog = dialog.fixed_size(size);
                }
                if let Some(margin) = s.margin {
                    dialog = dialog.inner_margin(margin);
                }
                let mut body = Rect::NOTHING;
                dialog.show(ctx, |ui| {
                    if s.fixed.is_some() {
                        body = ui.max_rect();
                    } else {
                        ui.label("Contents");
                        body = ui.min_rect();
                    }
                    let _ = ui.button("OK");
                });
                if body.is_positive() {
                    s.body = body;
                }
                if s.open.is_some() {
                    s.open = Some(open);
                }
            },
            state,
        );
        h.run();
        h
    }

    fn frames() -> [Option<WindowFrame>; 4] {
        [
            None,
            Some(WindowFrame::new(Flavor::Gnome)),
            Some(WindowFrame::new(Flavor::Kde)),
            Some(WindowFrame::new(Flavor::Windows)),
        ]
    }

    #[test]
    fn without_a_frame_dialogs_keep_egui_s_title_bar() {
        let h = harness(shown(None, Some(true)));
        h.get_by_label("Close window");
        assert!(h.query_by_label(strings().dialog_close).is_none());
        // The title names the window, once.
        h.get_by_label("Title");
    }

    #[test]
    fn the_desktop_s_close_button_closes_the_dialog() {
        for frame in frames().into_iter().flatten() {
            let mut h = harness(shown(Some(frame), Some(true)));
            assert!(h.query_by_label("Close window").is_none());
            h.get_by_label("Title");
            h.get_by_label(strings().dialog_close).click();
            h.run();
            assert_eq!(h.state().open, Some(false));
            assert!(h.query_by_label("OK").is_none(), "closed");
        }
    }

    #[test]
    fn a_dialog_without_open_has_no_close_button() {
        for frame in frames() {
            let h = harness(shown(frame, None));
            assert!(h.query_by_label(strings().dialog_close).is_none());
            assert!(h.query_by_label("Close window").is_none());
            h.get_by_label("OK");
        }
    }

    #[test]
    fn the_close_button_is_where_the_window_s_is() {
        let s = strings();
        let mut left = WindowFrame::new(Flavor::Gnome);
        left.layout.left = vec![Control::Close, Control::Minimize];
        left.layout.right = vec![];
        for (frame, on_left) in [
            (WindowFrame::new(Flavor::Gnome), false),
            (WindowFrame::new(Flavor::Kde), false),
            (WindowFrame::new(Flavor::Windows), false),
            (left, true),
        ] {
            let h = harness(shown(Some(frame), Some(true)));
            let close = h.get_by_label(s.dialog_close).rect().center().x;
            let dialog = h.get_by_label("Title").rect();
            assert_eq!(close < dialog.center().x, on_left);
            assert!(dialog.contains(egui::pos2(close, dialog.top() + 10.0)));
        }
    }

    #[test]
    fn the_contents_get_the_same_area_in_every_look() {
        for margin in [None, Some(Margin::symmetric(20, 12))] {
            let sizes: Vec<Vec2> = frames()
                .into_iter()
                .map(|frame| {
                    let mut state = shown(frame, Some(true));
                    state.fixed = Some(vec2(500.0, 320.0));
                    state.margin = margin;
                    let h = harness(state);
                    h.state().body.size()
                })
                .collect();
            for size in &sizes[1..] {
                assert!(
                    (*size - sizes[0]).length() < 0.6,
                    "{sizes:?} with margin {margin:?}"
                );
            }
        }
    }

    #[test]
    fn the_window_behind_a_dialog_takes_no_clicks() {
        // egui's look: the window behind stays usable, as before.
        let mut h = harness(shown(None, Some(true)));
        h.get_by_label("Behind").click();
        h.run();
        assert_eq!(h.state().behind_clicks, 1);
        for frame in frames().into_iter().flatten() {
            let mut h = harness(shown(Some(frame), Some(true)));
            h.get_by_label("Behind").click();
            h.run();
            assert_eq!(h.state().behind_clicks, 0);
            // The dialog still takes them.
            h.get_by_label(strings().dialog_close).click();
            h.run();
            assert_eq!(h.state().open, Some(false));
            // And once it is closed, the window does.
            h.get_by_label("Behind").click();
            h.run();
            assert_eq!(h.state().behind_clicks, 1);
        }
    }
}
