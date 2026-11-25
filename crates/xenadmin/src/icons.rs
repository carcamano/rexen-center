//! Small vector icons painted with egui shapes (no bitmap assets, no
//! third-party logos).

use egui::{Color32, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};
use xapi::VmPowerState;

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

/// A VM: monitor outline with a power-state dot.
pub fn vm(ui: &mut Ui, state: &VmPowerState) {
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
    painter.circle_filled(screen.center(), 3.2, power_color(state));
}

/// A host: server box; green light when enabled, grey in maintenance.
pub fn host(ui: &mut Ui, enabled: bool) {
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
    let light = if enabled {
        Color32::from_rgb(0x2e, 0xa0, 0x43)
    } else {
        Color32::GRAY
    };
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
