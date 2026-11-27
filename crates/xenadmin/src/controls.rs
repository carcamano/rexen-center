//! Controls laid out as the desktops' guidelines ask (this console's own;
//! the original was a Windows Forms application).
//!
//! - [`dialog_buttons`]: a dialog's buttons at its bottom right, in the
//!   platform's order. macOS, GNOME and the other Linux desktops put the main
//!   action at the right end and Cancel before it; Windows and KDE Plasma put
//!   the main action first and Cancel at the right end.
//! - [`tab_view`] and [`segmented`]: tabs drawn as macOS draws a tab view, a
//!   segmented control on the top edge of the pane, so that they no longer
//!   look like the push buttons around them.

use std::sync::OnceLock;

use egui::{
    Align, Color32, CornerRadius, Id, Layout, Rect, Response, Role, ScrollArea, Sense, Stroke,
    StrokeKind, TextStyle, TextWrapMode, Ui, UiBuilder, Vec2, Widget, WidgetInfo, WidgetText, pos2,
    vec2,
};

use crate::window_frame::Flavor;

// ---- dialog buttons ---------------------------------------------------------

/// Where the platform puts a dialog's main action among its buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonOrder {
    /// Cancel, then the main action at the right end (macOS, GNOME).
    PrimaryLast,
    /// The main action, then Cancel at the right end (Windows, KDE Plasma).
    PrimaryFirst,
}

impl ButtonOrder {
    /// The order of the desktop whose look the window takes (`flavor`, from
    /// the app's own title bar), or the platform's.
    pub fn for_flavor(flavor: Option<Flavor>) -> Self {
        match flavor {
            Some(Flavor::Gnome) => Self::PrimaryLast,
            Some(Flavor::Kde | Flavor::Windows) => Self::PrimaryFirst,
            None => Self::platform(),
        }
    }

    /// Windows' and KDE Plasma's order on those, macOS' and GNOME's
    /// elsewhere (the GTK desktops follow GNOME).
    fn platform() -> Self {
        static ORDER: OnceLock<ButtonOrder> = OnceLock::new();
        *ORDER.get_or_init(|| {
            let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
            if cfg!(windows) || crate::window_frame::flavor_of(&desktop) == Some(Flavor::Kde) {
                Self::PrimaryFirst
            } else {
                Self::PrimaryLast
            }
        })
    }

    /// Where a button of `role` goes, left to right; buttons of one role
    /// keep the order they are added in.
    fn rank(self, role: ButtonRole) -> u8 {
        match (self, role) {
            (Self::PrimaryLast, ButtonRole::Cancel) => 0,
            (Self::PrimaryLast, ButtonRole::Other) => 1,
            (Self::PrimaryLast, ButtonRole::Back) => 2,
            (Self::PrimaryLast, ButtonRole::Primary) => 3,
            (Self::PrimaryFirst, ButtonRole::Back) => 0,
            (Self::PrimaryFirst, ButtonRole::Primary) => 1,
            (Self::PrimaryFirst, ButtonRole::Other) => 2,
            (Self::PrimaryFirst, ButtonRole::Cancel) => 3,
        }
    }
}

fn order_id() -> Id {
    Id::unique("rexen-button-order")
}

/// Sets the order of the dialogs' buttons; the app calls it every frame
/// with its title bar's flavor (see [`crate::window_frame::style_dialogs`]).
pub fn set_button_order(ctx: &egui::Context, order: ButtonOrder) {
    ctx.data_mut(|d| d.insert_temp(order_id(), order));
}

fn button_order(ctx: &egui::Context) -> ButtonOrder {
    ctx.data(|d| d.get_temp(order_id()))
        .unwrap_or_else(ButtonOrder::platform)
}

/// What a dialog's button does, which decides where it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonRole {
    /// The dialog's main action: OK, Yes, Next, Finish, Connect, Delete...
    Primary,
    /// A wizard's Previous, next to the main action.
    Back,
    /// Another action, such as Apply, between the main action and Cancel.
    Other,
    /// Leaves without the dialog's action: Cancel, No, Close.
    Cancel,
}

/// A dialog's row of buttons, added with [`DialogButtons::add`].
pub struct DialogButtons<'u> {
    ui: &'u mut Ui,
    id: Id,
    row: Rect,
    /// Where each button goes, by the order they are added in, as laid out
    /// last frame (`None` while the row has not been laid out).
    planned: Option<(Vec<ButtonRole>, Vec<f32>)>,
    added: Vec<(ButtonRole, f32)>,
    /// Where the next [`DialogButtons::add_leading`] item goes.
    leading: f32,
    height: f32,
}

/// What a row remembers between frames: each button's role and width, and
/// the row's height.
#[derive(Clone, Default, PartialEq)]
struct RowMemory {
    buttons: Vec<(ButtonRole, f32)>,
    height: f32,
}

/// A dialog's buttons at the right end of a row as wide as the dialog, in
/// the platform's order (see [`ButtonOrder`]). Add each with its role, in
/// any order; the row places them where the platform expects:
///
/// ```ignore
/// dialog_buttons(ui, |row| {
///     if row.add_enabled(ButtonRole::Primary, valid, Button::new(s.ok)).clicked() { ... }
///     if row.button(ButtonRole::Cancel, s.cancel).clicked() { ... }
/// });
/// ```
///
/// The places come from the buttons' widths last frame: the first frame of
/// a row (or one whose buttons changed) is laid out again before it shows.
/// Two rows in one [`Ui`] need [`Ui::push_id`] around one of them.
pub fn dialog_buttons<R>(ui: &mut Ui, add: impl FnOnce(&mut DialogButtons<'_>) -> R) -> R {
    let id = ui.scope_id().with("rexen-dialog-buttons");
    let order = button_order(ui.ctx());
    let spacing = ui.spacing().item_spacing.x;
    let memory: Option<RowMemory> = ui.data(|d| d.get_temp(id));
    let height = memory
        .as_ref()
        .map_or(ui.spacing().interact_size.y, |m| m.height);
    let total = memory.as_ref().map_or(0.0, |m| {
        m.buttons.iter().map(|b| b.1).sum::<f32>()
            + spacing * m.buttons.len().saturating_sub(1) as f32
    });
    let (row, _) = ui.allocate_exact_size(
        vec2(ui.available_width().max(total), height),
        Sense::hover(),
    );
    let planned = memory.as_ref().map(|m| {
        let roles: Vec<ButtonRole> = m.buttons.iter().map(|b| b.0).collect();
        let widths: Vec<f32> = m.buttons.iter().map(|b| b.1).collect();
        (
            roles.clone(),
            place(order, &roles, &widths, row.right(), spacing),
        )
    });
    let mut buttons = DialogButtons {
        ui,
        id,
        row,
        planned,
        added: Vec::new(),
        leading: row.left(),
        height: 0.0,
    };
    let inner = add(&mut buttons);
    let now = RowMemory {
        buttons: buttons.added,
        height: buttons.height.max(buttons.ui.spacing().interact_size.y),
    };
    let changed = memory.is_none_or(|m| {
        m.buttons.len() != now.buttons.len()
            || m.buttons
                .iter()
                .zip(&now.buttons)
                .any(|(a, b)| a.0 != b.0 || (a.1 - b.1).abs() > 0.5)
            || (m.height - now.height).abs() > 0.5
    });
    if changed {
        buttons.ui.data_mut(|d| d.insert_temp(buttons.id, now));
        buttons.ui.ctx().request_discard("dialog buttons laid out");
    }
    inner
}

/// The left edge of each button (in the order given) in a row ending at
/// `right`, the buttons sorted by `order` and kept in the given order
/// within a role.
fn place(
    order: ButtonOrder,
    roles: &[ButtonRole],
    widths: &[f32],
    right: f32,
    spacing: f32,
) -> Vec<f32> {
    let mut visual: Vec<usize> = (0..roles.len()).collect();
    visual.sort_by_key(|&i| order.rank(roles[i]));
    let mut left = vec![0.0; roles.len()];
    let mut x = right;
    for &i in visual.iter().rev() {
        x -= widths[i];
        left[i] = x;
        x -= spacing;
    }
    left
}

impl DialogButtons<'_> {
    /// Adds `widget` as a button of `role`.
    pub fn add(&mut self, role: ButtonRole, widget: impl Widget) -> Response {
        self.add_enabled(role, true, widget)
    }

    /// Adds `widget` as a button of `role`, greyed out unless `enabled`.
    pub fn add_enabled(
        &mut self,
        role: ButtonRole,
        enabled: bool,
        widget: impl Widget,
    ) -> Response {
        let index = self.added.len();
        let left = match &self.planned {
            Some((roles, left)) if roles.get(index) == Some(&role) => left[index],
            // Not laid out yet: one after the other, for this frame only.
            _ => {
                self.row.left()
                    + self
                        .added
                        .iter()
                        .map(|b| b.1 + self.ui.spacing().item_spacing.x)
                        .sum::<f32>()
            }
        };
        let rect = Rect::from_min_max(
            pos2(left, self.row.top()),
            pos2(f32::INFINITY, self.row.bottom()),
        );
        let response = self
            .ui
            .scope_builder(
                UiBuilder::new()
                    .max_rect(rect)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| ui.add_enabled(enabled, widget),
            )
            .inner;
        self.added.push((role, response.rect.width()));
        self.height = self.height.max(response.rect.height());
        response
    }

    /// Adds `widget` at the row's left end, after the others added there:
    /// a spinner or a short status while the dialog works, where macOS
    /// shows a sheet's progress. It takes no part in the buttons' order.
    pub fn add_leading(&mut self, widget: impl Widget) -> Response {
        let rect = Rect::from_min_max(
            pos2(self.leading, self.row.top()),
            pos2(self.row.right(), self.row.bottom()),
        );
        let response = self
            .ui
            .scope_builder(
                UiBuilder::new()
                    .max_rect(rect)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| ui.add(widget),
            )
            .inner;
        self.leading = response.rect.right() + self.ui.spacing().item_spacing.x;
        self.height = self.height.max(response.rect.height());
        response
    }

    /// The dialog's [`Ui`], for reading input; widgets added to it directly
    /// are not part of the row.
    pub fn ui(&mut self) -> &mut Ui {
        self.ui
    }

    /// Adds a text button of `role`.
    pub fn button(&mut self, role: ButtonRole, text: impl Into<WidgetText>) -> Response {
        self.add(role, egui::Button::new(text))
    }
}

// ---- tabs -------------------------------------------------------------------

/// The space between a segment's text and its edges.
const SEGMENT_PADDING: f32 = 12.0;
/// Between the track's edge and the selected segment.
const SEGMENT_INSET: f32 = 2.0;
const TRACK_RADIUS: u8 = 7;

/// Tabs as macOS draws a tab view's: a segmented control in the middle of
/// the pane's top edge, a line, with the selected tab raised. They stay at
/// the left and scroll when the pane is too narrow for them. Returns `true`
/// when a tab was chosen.
pub fn tab_view<T, S>(ui: &mut Ui, selected: &mut T, tabs: impl IntoIterator<Item = (T, S)>) -> bool
where
    T: PartialEq + Clone,
    S: Into<WidgetText>,
{
    let segments = Segments::new(ui, tabs);
    let size = segments.size();
    let width = ui.available_width();
    let (row, _) = ui.allocate_exact_size(vec2(width, size.y), Sense::hover());
    let line = ui.visuals().widgets.noninteractive.bg_stroke;
    ui.painter().hline(row.x_range(), row.center().y, line);
    let mut changed = false;
    if size.x <= width {
        let rect = Rect::from_center_size(row.center(), size);
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            changed = segments.show(ui, selected);
        });
    } else {
        ui.scope_builder(UiBuilder::new().max_rect(row), |ui| {
            ScrollArea::horizontal()
                .id_salt("rexen-tab-view")
                .show(ui, |ui| changed = segments.show(ui, selected));
        });
    }
    changed
}

/// A segmented control choosing one of `items`, where it is in the layout
/// (the look of [`tab_view`]'s tabs, without the line). Returns `true` when
/// another item was chosen.
pub fn segmented<T, S>(
    ui: &mut Ui,
    selected: &mut T,
    items: impl IntoIterator<Item = (T, S)>,
) -> bool
where
    T: PartialEq + Clone,
    S: Into<WidgetText>,
{
    Segments::new(ui, items).show(ui, selected)
}

/// [`segmented`] as wide as the space left, the segments sharing it
/// equally: tabs over a pane as wide as they are.
pub fn segmented_filling<T, S>(
    ui: &mut Ui,
    selected: &mut T,
    items: impl IntoIterator<Item = (T, S)>,
) -> bool
where
    T: PartialEq + Clone,
    S: Into<WidgetText>,
{
    let width = ui.available_width();
    Segments::filling(ui, width, items).show(ui, selected)
}

/// The segments of a [`tab_view`] or a [`segmented`] control.
struct Segments<T> {
    items: Vec<(T, std::sync::Arc<egui::Galley>, String)>,
    height: f32,
    /// Each segment's width when they share a given width equally.
    equal: Option<f32>,
}

impl<T: PartialEq + Clone> Segments<T> {
    fn new<S: Into<WidgetText>>(ui: &Ui, items: impl IntoIterator<Item = (T, S)>) -> Self {
        Self::build(ui, items, None)
    }

    /// Segments sharing `width` equally, their text cut short if it does
    /// not fit.
    fn filling<S: Into<WidgetText>>(
        ui: &Ui,
        width: f32,
        items: impl IntoIterator<Item = (T, S)>,
    ) -> Self {
        let items: Vec<(T, S)> = items.into_iter().collect();
        let each = ((width - 2.0 * SEGMENT_INSET) / items.len().max(1) as f32).max(1.0);
        Self::build(ui, items, Some(each))
    }

    fn build<S: Into<WidgetText>>(
        ui: &Ui,
        items: impl IntoIterator<Item = (T, S)>,
        equal: Option<f32>,
    ) -> Self {
        let (wrap, width) = match equal {
            Some(each) => (TextWrapMode::Truncate, (each - 8.0).max(1.0)),
            None => (TextWrapMode::Extend, f32::INFINITY),
        };
        let items = items
            .into_iter()
            .map(|(value, text)| {
                let text: WidgetText = text.into();
                let label = text.text().to_owned();
                let galley = text.into_galley(ui, Some(wrap), width, TextStyle::Button);
                (value, galley, label)
            })
            .collect();
        Self {
            items,
            height: ui.spacing().interact_size.y + 2.0 * SEGMENT_INSET + 2.0,
            equal,
        }
    }

    fn widths(&self) -> impl Iterator<Item = f32> + '_ {
        self.items.iter().map(|(_, galley, _)| {
            self.equal
                .unwrap_or(galley.size().x + 2.0 * SEGMENT_PADDING)
        })
    }

    fn size(&self) -> Vec2 {
        vec2(
            self.widths().sum::<f32>() + 2.0 * SEGMENT_INSET,
            self.height,
        )
    }

    fn show(self, ui: &mut Ui, selected: &mut T) -> bool {
        let size = self.size();
        let (track, _) = ui.allocate_exact_size(size, Sense::hover());
        let visuals = ui.visuals().clone();
        let dark = visuals.dark_mode;
        let painter = ui.painter().clone();
        let track_fill = if dark {
            Color32::from_gray(48)
        } else {
            Color32::from_gray(226)
        };
        painter.rect(
            track,
            TRACK_RADIUS,
            track_fill,
            Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
            StrokeKind::Inside,
        );

        let mut rects = Vec::with_capacity(self.items.len());
        let mut x = track.left() + SEGMENT_INSET;
        for width in self.widths() {
            rects.push(Rect::from_min_size(
                pos2(x, track.top() + SEGMENT_INSET),
                vec2(width, track.height() - 2.0 * SEGMENT_INSET),
            ));
            x += width;
        }
        let chosen = self.items.iter().position(|(v, ..)| v == selected);

        // The dividers between two segments that are not selected.
        let divider = Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color);
        for (i, r) in rects.iter().enumerate().skip(1) {
            if chosen != Some(i) && chosen != Some(i - 1) {
                let y = r.y_range();
                painter.vline(r.left(), (y.min + 5.0)..=(y.max - 5.0), divider);
            }
        }

        let mut changed = false;
        let radius = CornerRadius::same(TRACK_RADIUS - SEGMENT_INSET as u8);
        for (i, ((value, galley, label), rect)) in self.items.into_iter().zip(rects).enumerate() {
            let is_selected = chosen == Some(i);
            let id = ui.scope_id().with(("rexen-segment", i));
            let response = ui.interact(rect, id, Sense::click());
            response.widget_info(|| WidgetInfo::labeled(Role::Tab, true, &label));
            // A tab is selected, not toggled (egui's `selected` in
            // `WidgetInfo`).
            ui.ctx()
                .accesskit_node_builder(id, |node| node.set_selected(is_selected));
            if is_selected {
                let fill = if dark {
                    Color32::from_gray(92)
                } else {
                    Color32::WHITE
                };
                // A soft shadow under the raised segment.
                painter.rect_filled(
                    rect.translate(vec2(0.0, 0.5)),
                    radius,
                    Color32::from_black_alpha(if dark { 60 } else { 28 }),
                );
                painter.rect(
                    rect,
                    radius,
                    fill,
                    Stroke::new(0.5, Color32::from_black_alpha(if dark { 80 } else { 36 })),
                    StrokeKind::Inside,
                );
            } else if response.is_pointer_button_down_on() {
                painter.rect_filled(
                    rect,
                    radius,
                    Color32::from_black_alpha(if dark { 40 } else { 24 }),
                );
            } else if response.hovered() {
                painter.rect_filled(
                    rect,
                    radius,
                    Color32::from_black_alpha(if dark { 24 } else { 12 }),
                );
            }
            if response.has_focus() {
                painter.rect_stroke(rect, radius, visuals.selection.stroke, StrokeKind::Inside);
            }
            let color = if is_selected {
                visuals.strong_text_color()
            } else {
                visuals.text_color()
            };
            let pos = rect.center() - galley.size() / 2.0;
            painter.galley(pos, galley, color);
            if response.clicked() && !is_selected {
                *selected = value;
                changed = true;
            }
        }
        changed
    }
}

// ---- tables -----------------------------------------------------------------

/// A table's column heading, as a macOS table header draws it: the title as
/// plain text (no button frame), brighter under the pointer, and a chevron
/// after it on the column the rows are sorted by (`sort`: `Some(true)`
/// ascending). It fills its cell, which a click sorts by; the label stays
/// `title` whatever the order.
pub fn column_heading(ui: &mut Ui, title: &str, sort: Option<bool>) -> Response {
    let galley = WidgetText::from(title).into_galley(
        ui,
        Some(TextWrapMode::Extend),
        f32::INFINITY,
        TextStyle::Body,
    );
    let chevron = 12.0;
    let width = (galley.size().x + chevron).max(ui.available_width());
    let height = galley.size().y.max(ui.spacing().interact_size.y);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::ColumnHeader, true, title));
    if let Some(ascending) = sort {
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_sort_direction(if ascending {
                egui::accesskit::SortDirection::Ascending
            } else {
                egui::accesskit::SortDirection::Descending
            });
        });
    }
    let visuals = ui.visuals();
    let color = if response.hovered() || sort.is_some() {
        visuals.strong_text_color()
    } else {
        visuals.text_color()
    };
    let painter = ui.painter();
    let text_pos = pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
    let text_right = rect.left() + galley.size().x;
    painter.galley(text_pos, galley, color);
    if let Some(ascending) = sort {
        let c = pos2(text_right + 8.0, rect.center().y);
        let (w, h) = (3.5, 2.0);
        let (tip, base) = if ascending { (-h, h) } else { (h, -h) };
        painter.line(
            vec![
                pos2(c.x - w, c.y + base),
                pos2(c.x, c.y + tip),
                pos2(c.x + w, c.y + base),
            ],
            Stroke::new(1.5, color),
        );
    }
    response
}

/// The line under a table's headings, across the table (`table`, the
/// grid's rectangle) just below `headings_bottom`.
pub fn paint_heading_rule(ui: &Ui, table: Rect, headings_bottom: f32) {
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    ui.painter()
        .hline(table.x_range(), headings_bottom + 2.0, stroke);
}

// ---- a dialog's pages -------------------------------------------------------

/// The space between a page's text and its row's edges.
const PAGE_PADDING: Vec2 = vec2(8.0, 4.0);
const PAGE_RADIUS: u8 = 6;

/// A Properties dialog's list of pages, as the sidebar of macOS' System
/// Settings: one row per page as wide as the column, the title over a
/// smaller, dimmer summary (none when `subtitle` is empty), the selected
/// row filled with the selection colour. Returns the page clicked, if
/// another than `selected`.
pub fn page_list<T, A, B>(
    ui: &mut Ui,
    selected: &T,
    pages: impl IntoIterator<Item = (T, A, B)>,
) -> Option<T>
where
    T: PartialEq,
    A: AsRef<str>,
    B: AsRef<str>,
{
    let mut chosen = None;
    let width = ui.available_width();
    let wrap = width - 2.0 * PAGE_PADDING.x;
    for (value, title, subtitle) in pages {
        let (title, subtitle) = (title.as_ref(), subtitle.as_ref());
        let is_selected = value == *selected;
        let title_galley = WidgetText::from(title).into_galley(
            ui,
            Some(TextWrapMode::Wrap),
            wrap,
            TextStyle::Body,
        );
        let sub_galley = (!subtitle.is_empty()).then(|| {
            WidgetText::from(subtitle).into_galley(
                ui,
                Some(TextWrapMode::Wrap),
                wrap,
                TextStyle::Small,
            )
        });
        let text_height =
            title_galley.size().y + sub_galley.as_ref().map_or(0.0, |g| g.size().y + 1.0);
        let (rect, response) = ui.allocate_exact_size(
            vec2(width, text_height + 2.0 * PAGE_PADDING.y),
            Sense::click(),
        );
        let label = if subtitle.is_empty() {
            title.to_owned()
        } else {
            format!("{title}\n{subtitle}")
        };
        response.widget_info(|| WidgetInfo::labeled(Role::Tab, true, &label));
        ui.ctx()
            .accesskit_node_builder(response.id, |node| node.set_selected(is_selected));

        let visuals = ui.visuals();
        let (fill, title_color, sub_color) = if is_selected {
            let on = visuals.selection.stroke.color;
            (visuals.selection.bg_fill, on, on.gamma_multiply(0.8))
        } else {
            let fill = if response.hovered() {
                visuals.widgets.hovered.weak_bg_fill
            } else {
                Color32::TRANSPARENT
            };
            (fill, visuals.text_color(), visuals.weak_text_color())
        };
        let painter = ui.painter();
        painter.rect_filled(rect, PAGE_RADIUS, fill);
        if response.has_focus() {
            painter.rect_stroke(
                rect,
                PAGE_RADIUS,
                visuals.selection.stroke,
                StrokeKind::Inside,
            );
        }
        let mut pos = rect.min + PAGE_PADDING;
        let title_height = title_galley.size().y;
        painter.galley(pos, title_galley, title_color);
        if let Some(g) = sub_galley {
            pos.y += title_height + 1.0;
            painter.galley(pos, g, sub_color);
        }
        if response.clicked() && !is_selected {
            chosen = Some(value);
        }
    }
    chosen
}

/// Leaves the gap for a line between a dialog's page list and its page in a
/// row (`Ui::horizontal_top`); returns where the line goes, for
/// [`paint_divider`] once the row is done. A separator there would grow to
/// the screen's height in a dialog sized to its contents.
pub fn divider_gap(ui: &mut Ui) -> f32 {
    let gap = 8.0;
    ui.add_space(gap);
    let x = ui.cursor().left();
    ui.add_space(gap);
    x
}

/// Draws the line [`divider_gap`] left room for, at `x` down the `row`
/// that `Ui::horizontal_top` returned.
pub fn paint_divider(ui: &Ui, x: f32, row: Rect) {
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    ui.painter().vline(x, row.y_range(), stroke);
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::*;

    #[test]
    fn the_platforms_orders() {
        use ButtonRole::{Back, Cancel, Other, Primary};
        let arrange = |order: ButtonOrder, roles: &[ButtonRole]| -> Vec<usize> {
            let left = place(order, roles, &vec![10.0; roles.len()], 100.0, 0.0);
            let mut visual: Vec<usize> = (0..roles.len()).collect();
            visual.sort_by(|a, b| left[*a].total_cmp(&left[*b]));
            visual
        };
        // OK, Cancel.
        assert_eq!(
            arrange(ButtonOrder::PrimaryLast, &[Primary, Cancel]),
            [1, 0]
        );
        assert_eq!(
            arrange(ButtonOrder::PrimaryFirst, &[Primary, Cancel]),
            [0, 1]
        );
        // Previous, Next, Cancel.
        assert_eq!(
            arrange(ButtonOrder::PrimaryLast, &[Back, Primary, Cancel]),
            [2, 0, 1]
        );
        assert_eq!(
            arrange(ButtonOrder::PrimaryFirst, &[Back, Primary, Cancel]),
            [0, 1, 2]
        );
        // OK, Apply, Cancel; added in any order.
        assert_eq!(
            arrange(ButtonOrder::PrimaryLast, &[Cancel, Other, Primary]),
            [0, 1, 2]
        );
        assert_eq!(
            arrange(ButtonOrder::PrimaryFirst, &[Cancel, Other, Primary]),
            [2, 1, 0]
        );
    }

    #[test]
    fn the_flavors_orders() {
        assert_eq!(
            ButtonOrder::for_flavor(Some(Flavor::Gnome)),
            ButtonOrder::PrimaryLast
        );
        assert_eq!(
            ButtonOrder::for_flavor(Some(Flavor::Kde)),
            ButtonOrder::PrimaryFirst
        );
        assert_eq!(
            ButtonOrder::for_flavor(Some(Flavor::Windows)),
            ButtonOrder::PrimaryFirst
        );
    }

    /// A 400-point panel with OK and Cancel in `order`; the state counts
    /// the clicks on each.
    fn buttons(order: ButtonOrder) -> Harness<'static, [usize; 2]> {
        let mut h = Harness::builder()
            .with_size(vec2(400.0, 120.0))
            .build_ui_state(
                move |ui, clicks: &mut [usize; 2]| {
                    set_button_order(ui.ctx(), order);
                    ui.label("Contents");
                    dialog_buttons(ui, |row| {
                        if row.button(ButtonRole::Primary, "OK").clicked() {
                            clicks[0] += 1;
                        }
                        if row.button(ButtonRole::Cancel, "Cancel").clicked() {
                            clicks[1] += 1;
                        }
                    });
                },
                [0, 0],
            );
        h.run();
        h
    }

    #[test]
    fn dialog_buttons_sit_at_the_right_in_the_platform_s_order() {
        for (order, ok_first) in [
            (ButtonOrder::PrimaryLast, false),
            (ButtonOrder::PrimaryFirst, true),
        ] {
            let mut h = buttons(order);
            let ok = h.get_by_label("OK").rect();
            let cancel = h.get_by_label("Cancel").rect();
            let right = ok.right().max(cancel.right());
            assert!(right > 380.0, "at the right edge: {right}");
            assert!(
                ok.left() > 200.0 && cancel.left() > 200.0,
                "{ok:?} {cancel:?}"
            );
            assert_eq!(ok.left() < cancel.left(), ok_first, "{order:?}");
            assert!((ok.center().y - cancel.center().y).abs() < 0.5);
            h.get_by_label("OK").click();
            h.run();
            h.get_by_label("Cancel").click();
            h.run();
            assert_eq!(*h.state(), [1, 1]);
        }
    }

    #[test]
    fn tabs_choose_one_and_say_which() {
        #[derive(Clone, Copy, PartialEq, Debug)]
        enum Page {
            One,
            Two,
            Three,
        }
        let mut h = Harness::builder()
            .with_size(vec2(500.0, 100.0))
            .build_ui_state(
                |ui, page: &mut Page| {
                    tab_view(
                        ui,
                        page,
                        [
                            (Page::One, "One"),
                            (Page::Two, "Two"),
                            (Page::Three, "Three"),
                        ],
                    );
                },
                Page::One,
            );
        h.run();
        assert!(h.get_by_label("One").accesskit_node().is_selected() == Some(true));
        assert!(h.get_by_label("Two").accesskit_node().is_selected() == Some(false));
        // In the middle of the pane.
        let one = h.get_by_label("One").rect();
        let three = h.get_by_label("Three").rect();
        let middle = (one.left() + three.right()) / 2.0;
        assert!((middle - 250.0).abs() < 10.0, "centred: {middle}");
        h.get_by_label("Three").click();
        h.run();
        assert_eq!(*h.state(), Page::Three);
        assert!(h.get_by_label("Three").accesskit_node().is_selected() == Some(true));
    }

    #[test]
    fn page_lists_choose_a_page_and_name_it_with_its_summary() {
        let mut h = Harness::builder()
            .with_size(vec2(300.0, 200.0))
            .build_ui_state(
                |ui, page: &mut u8| {
                    ui.set_width(160.0);
                    let pages = [(1, "General", "web-01"), (2, "Custom Fields", "")];
                    if let Some(p) = page_list(ui, page, pages) {
                        *page = p;
                    }
                },
                1,
            );
        h.run();
        let general = h.get_by_label("General\nweb-01");
        assert_eq!(general.accesskit_node().is_selected(), Some(true));
        // A row is as wide as its column, not its text.
        assert!(general.rect().width() >= 159.0, "{:?}", general.rect());
        h.get_by_label("Custom Fields").click();
        h.run();
        assert_eq!(*h.state(), 2);
        assert_eq!(
            h.get_by_label("Custom Fields")
                .accesskit_node()
                .is_selected(),
            Some(true)
        );
    }

    #[test]
    fn column_headings_keep_their_title_and_say_the_order() {
        let mut h = Harness::builder()
            .with_size(vec2(300.0, 100.0))
            .build_ui_state(
                |ui, sorted: &mut Option<bool>| {
                    ui.horizontal(|ui| {
                        if column_heading(ui, "Name", *sorted).clicked() {
                            *sorted = Some(!sorted.unwrap_or(false));
                        }
                    });
                },
                None,
            );
        h.run();
        h.get_by_label("Name").click();
        h.run();
        assert_eq!(*h.state(), Some(true));
        let node = h.get_by_label("Name");
        assert_eq!(
            node.accesskit_node().sort_direction(),
            Some(egui::accesskit::SortDirection::Ascending)
        );
    }
}
