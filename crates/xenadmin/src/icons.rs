//! Small vector icons painted with egui shapes (no bitmap assets, no
//! third-party logos).

use egui::{Color32, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};
use xapi::VmPowerState;
use xen_core::objects::{SrIcon, VmIcon};

const SIZE: Vec2 = vec2(14.0, 14.0);

fn allocate(ui: &mut Ui) -> (Rect, egui::Painter) {
    let (rect, _) = ui.allocate_exact_size(SIZE, Sense::hover());
    (rect, ui.painter_at(rect))
}

/// Colour used for a VM power state.
pub fn power_color(state: &VmPowerState) -> Color32 {
    match state {
        VmPowerState::Running => Color32::from_rgb(0x2e, 0xa0, 0x43),
        VmPowerState::Halted => Color32::from_rgb(0xc0, 0x39, 0x2b),
        VmPowerState::Suspended => Color32::from_rgb(0xe6, 0x9b, 0x00),
        VmPowerState::Paused => Color32::from_rgb(0x2f, 0x6f, 0xd0),
        VmPowerState::Unknown(_) => Color32::GRAY,
    }
}

const BUSY: Color32 = Color32::from_rgb(0xe0, 0x7b, 0x00);
const WARN: Color32 = Color32::from_rgb(0xe6, 0x9b, 0x00);
const BROKEN: Color32 = Color32::from_rgb(0xc0, 0x39, 0x2b);
const OK: Color32 = Color32::from_rgb(0x2e, 0xa0, 0x43);

/// A VM, template or snapshot in the state the tree shows.
pub fn vm_state(ui: &mut Ui, icon: VmIcon) {
    match icon {
        VmIcon::Running => vm(ui, &VmPowerState::Running),
        VmIcon::Halted => vm(ui, &VmPowerState::Halted),
        VmIcon::Suspended => vm(ui, &VmPowerState::Suspended),
        VmIcon::Paused => vm(ui, &VmPowerState::Paused),
        VmIcon::Busy => monitor(ui, BUSY),
        VmIcon::Snapshot { with_memory } => snapshot(ui, with_memory),
        VmIcon::UserTemplate => template(ui, false),
        VmIcon::DefaultTemplate => template(ui, true),
    }
}

/// A VM: monitor outline with a power-state dot.
pub fn vm(ui: &mut Ui, state: &VmPowerState) {
    monitor(ui, power_color(state));
}

fn monitor(ui: &mut Ui, dot: Color32) {
    let (rect, painter) = allocate(ui);
    let stroke = Stroke::new(1.2, ui.visuals().text_color());
    let screen = Rect::from_min_max(rect.min + vec2(1.0, 2.0), rect.max - vec2(1.0, 4.0));
    painter.rect_stroke(screen, 1.5, stroke, StrokeKind::Inside);
    painter.line_segment(
        [
            pos2(rect.center().x, screen.max.y),
            pos2(rect.center().x, rect.max.y - 1.0),
        ],
        stroke,
    );
    painter.circle_filled(screen.center(), 3.2, dot);
}

/// A snapshot: a camera; filled lens when it includes memory.
fn snapshot(ui: &mut Ui, with_memory: bool) {
    let (rect, painter) = allocate(ui);
    let stroke = Stroke::new(1.2, ui.visuals().text_color());
    let body = Rect::from_min_max(rect.min + vec2(1.0, 4.0), rect.max - vec2(1.0, 2.0));
    painter.rect_stroke(body, 1.5, stroke, StrokeKind::Inside);
    let bump = Rect::from_min_size(pos2(rect.min.x + 4.0, rect.min.y + 2.0), vec2(4.0, 2.0));
    painter.rect_filled(bump, 0.5, ui.visuals().text_color());
    if with_memory {
        painter.circle_filled(body.center(), 2.6, ui.visuals().text_color());
    } else {
        painter.circle_stroke(body.center(), 2.6, stroke);
    }
}

/// A template: a page with a folded corner; built-in ones are filled.
fn template(ui: &mut Ui, builtin: bool) {
    let (rect, painter) = allocate(ui);
    let color = ui.visuals().text_color();
    let (l, t, r, b) = (
        rect.min.x + 2.5,
        rect.min.y + 1.0,
        rect.max.x - 2.5,
        rect.max.y - 1.0,
    );
    let points = vec![
        pos2(l, t),
        pos2(r - 4.0, t),
        pos2(r, t + 4.0),
        pos2(r, b),
        pos2(l, b),
    ];
    if builtin {
        painter.add(Shape::convex_polygon(
            points,
            color.gamma_multiply(0.45),
            Stroke::new(1.0, color),
        ));
    } else {
        painter.add(Shape::closed_line(points, Stroke::new(1.1, color)));
    }
    painter.line_segment(
        [pos2(l + 2.5, t + 6.0), pos2(r - 2.5, t + 6.0)],
        Stroke::new(1.0, color),
    );
    painter.line_segment(
        [pos2(l + 2.5, t + 9.0), pos2(r - 2.5, t + 9.0)],
        Stroke::new(1.0, color),
    );
}

/// How a host is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostState {
    Connected,
    /// Disabled or being evacuated.
    Maintenance,
    /// Not live, or the connection is down.
    Disconnected,
}

/// A host in the given state.
pub fn host_state(ui: &mut Ui, state: HostState) {
    match state {
        HostState::Connected => host(ui, true),
        HostState::Disconnected => host(ui, false),
        HostState::Maintenance => host_lights(ui, WARN),
    }
}

/// A storage repository: a disk cylinder, with a red cross when broken, a
/// green tick when it is the pool's default, greyed when disabled.
pub fn sr(ui: &mut Ui, icon: SrIcon) {
    let (rect, painter) = allocate(ui);
    let base = ui.visuals().text_color();
    let color = if icon == SrIcon::Disabled {
        base.gamma_multiply(0.4)
    } else {
        base
    };
    let stroke = Stroke::new(1.1, color);
    let body = Rect::from_min_max(rect.min + vec2(2.0, 3.0), rect.max - vec2(2.0, 2.0));
    painter.line_segment([body.left_top(), body.left_bottom()], stroke);
    painter.line_segment([body.right_top(), body.right_bottom()], stroke);
    let ellipse = |y: f32| {
        Shape::ellipse_stroke(
            pos2(body.center().x, y),
            vec2(body.width() / 2.0, 1.8),
            stroke,
        )
    };
    painter.add(ellipse(body.min.y));
    painter.add(ellipse(body.max.y));
    painter.add(ellipse(body.center().y));
    match icon {
        SrIcon::Broken => {
            let s = Stroke::new(1.6, BROKEN);
            let c = pos2(rect.max.x - 3.0, rect.max.y - 3.0);
            painter.line_segment([c + vec2(-2.5, -2.5), c + vec2(2.5, 2.5)], s);
            painter.line_segment([c + vec2(-2.5, 2.5), c + vec2(2.5, -2.5)], s);
        }
        SrIcon::Default => {
            let s = Stroke::new(1.6, OK);
            let c = pos2(rect.max.x - 3.5, rect.max.y - 3.0);
            painter.line_segment([c + vec2(-2.5, 0.0), c + vec2(-0.5, 2.0)], s);
            painter.line_segment([c + vec2(-0.5, 2.0), c + vec2(3.0, -2.5)], s);
        }
        SrIcon::Normal | SrIcon::Disabled => {}
    }
}

/// A host: server box; green lights when up, grey when not.
pub fn host(ui: &mut Ui, enabled: bool) {
    host_lights(ui, if enabled { OK } else { Color32::GRAY });
}

fn host_lights(ui: &mut Ui, light: Color32) {
    let (rect, painter) = allocate(ui);
    let stroke = Stroke::new(1.2, ui.visuals().text_color());
    let top = Rect::from_min_max(
        rect.min + vec2(1.0, 2.0),
        pos2(rect.max.x - 1.0, rect.center().y - 0.5),
    );
    let bottom = Rect::from_min_max(
        pos2(rect.min.x + 1.0, rect.center().y + 0.5),
        rect.max - vec2(1.0, 2.0),
    );
    painter.rect_stroke(top, 1.0, stroke, StrokeKind::Inside);
    painter.rect_stroke(bottom, 1.0, stroke, StrokeKind::Inside);
    painter.circle_filled(pos2(top.max.x - 3.0, top.center().y), 1.6, light);
    painter.circle_filled(pos2(bottom.max.x - 3.0, bottom.center().y), 1.6, light);
}

/// A pool: three stacked boxes.
pub fn pool(ui: &mut Ui) {
    let (rect, painter) = allocate(ui);
    let stroke = Stroke::new(1.1, ui.visuals().text_color());
    for i in 0..3 {
        let y = rect.min.y + 1.5 + i as f32 * 4.0;
        let r = Rect::from_min_size(pos2(rect.min.x + 1.0, y), vec2(rect.width() - 2.0, 3.2));
        painter.rect_stroke(r, 0.8, stroke, StrokeKind::Inside);
    }
}
