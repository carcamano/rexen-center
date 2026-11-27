//! Help > About: the product's icon, name, subtitle, version and build, a
//! short description, and the credits and licence the console carries
//! (the original's `AboutDialog`, redesigned: everything on one centred
//! axis, a fixed width, and only the credits scroll).

use egui::{
    Align2, Context, CornerRadius, Frame, Image, Margin, RichText, ScrollArea, Sense, StrokeKind,
    TextStyle, TextWrapMode, TextureHandle, TextureOptions, Ui, Vec2, WidgetInfo, WidgetText,
};

use crate::branding;
use crate::i18n::{fill, strings};

/// The width of the dialog's content (points).
pub const WIDTH: f32 = 420.0;
/// The icon's size at the top (points).
pub const ICON_SIZE: f32 = 112.0;
/// The tallest the credits and licence get before they scroll (points).
const CREDITS_HEIGHT: f32 = 140.0;

/// The About window's state.
#[derive(Default)]
pub struct About {
    pub open: bool,
    show_license: bool,
    icon: Option<TextureHandle>,
}

impl About {
    /// Draws the window while it is open: centred on the screen when it
    /// opens, its content centred on one axis.
    pub fn show(&mut self, ctx: &Context) {
        if !self.open {
            return;
        }
        let s = strings();
        let icon = self
            .icon
            .get_or_insert_with(|| {
                ctx.load_texture("about-icon", branding::icon_image(), TextureOptions::LINEAR)
            })
            .clone();
        let mut open = self.open;
        let mut close = false;
        egui::Window::new(format!("{} {}", s.about_title, branding::PRODUCT_NAME))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .frame(Frame::window(&ctx.global_style()).inner_margin(Margin::symmetric(20, 12)))
            .min_width(WIDTH)
            .max_width(WIDTH)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    let weak = ui.visuals().weak_text_color();
                    ui.add_space(10.0);
                    ui.add(
                        Image::from_texture((icon.id(), Vec2::splat(ICON_SIZE)))
                            .alt_text(fill(s.about_icon, &[("product", branding::PRODUCT_NAME)])),
                    );
                    ui.add_space(10.0);
                    ui.label(RichText::new(branding::PRODUCT_NAME).size(26.0).strong());
                    ui.add_space(2.0);
                    ui.label(RichText::new(s.about_tagline).italics().color(weak));
                    ui.add_space(14.0);
                    ui.label(fill(
                        s.about_summary,
                        &[("platform", branding::PLATFORM_NAME)],
                    ));
                    ui.add_space(8.0);
                    pill(ui, &version_line());
                    ui.add_space(12.0);
                    ui.label(RichText::new(s.about_disclaimer).small().color(weak));
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(RichText::new(s.about_credits_heading).strong());
                    ui.add_space(2.0);
                    ScrollArea::vertical()
                        .id_salt("about-credits")
                        .max_height(CREDITS_HEIGHT)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.label(RichText::new(s.about_credits).color(weak));
                                ui.add_space(8.0);
                                let heading = fill(
                                    s.about_license_heading,
                                    &[("license", branding::LICENSE_ID)],
                                );
                                if ui
                                    .add(
                                        egui::Button::new(RichText::new(heading).small())
                                            .selected(self.show_license),
                                    )
                                    .clicked()
                                {
                                    self.show_license = !self.show_license;
                                }
                                if self.show_license {
                                    ui.add_space(4.0);
                                    license(ui);
                                }
                            });
                        });
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(6.0);
                    close = ui
                        .add(egui::Button::new(s.close).min_size(egui::vec2(104.0, 28.0)))
                        .clicked();
                    ui.add_space(4.0);
                });
            });
        self.open = open && !close;
    }
}

/// `text` in a rounded outline sized to the text (a label to assistive
/// technology).
fn pill(ui: &mut Ui, text: &str) {
    let galley = WidgetText::from(RichText::new(text).small()).into_galley(
        ui,
        Some(TextWrapMode::Extend),
        f32::INFINITY,
        TextStyle::Body,
    );
    let padding = egui::vec2(12.0, 4.0);
    let (rect, response) = ui.allocate_exact_size(galley.size() + 2.0 * padding, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(egui::accesskit::Role::Label, true, text));
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        ui.painter().rect(
            rect,
            CornerRadius::same((rect.height() / 2.0) as u8),
            visuals.faint_bg_color,
            visuals.widgets.noninteractive.bg_stroke,
            StrokeKind::Inside,
        );
        ui.painter()
            .galley(rect.min + padding, galley, visuals.text_color());
    }
}

/// The licence text, in a box of its own (left-aligned: it is a legal text).
fn license(ui: &mut Ui) {
    Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.label(RichText::new(branding::LICENSE_TEXT).monospace().small());
            });
        });
}

/// "Version 0.1.1 · Release build for macOS (aarch64)".
pub fn version_line() -> String {
    let s = strings();
    let build = if cfg!(debug_assertions) {
        s.about_build_debug
    } else {
        s.about_build_release
    };
    format!(
        "{} · {}",
        fill(s.about_version, &[("version", branding::VERSION)]),
        fill(
            build,
            &[("os", os_name()), ("arch", std::env::consts::ARCH)]
        )
    )
}

/// The operating system's name as people write it.
fn os_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        "freebsd" => "FreeBSD",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    fn harness(theme: egui::Theme) -> Harness<'static, About> {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 760.0))
            .with_theme(theme)
            .build_ui_state(
                |ui, about: &mut About| about.show(ui.ctx()),
                About {
                    open: true,
                    ..About::default()
                },
            );
        harness.run();
        harness
    }

    /// Everything the window shows, top to bottom.
    fn content() -> Vec<String> {
        let s = strings();
        vec![
            fill(s.about_icon, &[("product", branding::PRODUCT_NAME)]),
            branding::PRODUCT_NAME.to_owned(),
            s.about_tagline.to_owned(),
            fill(s.about_summary, &[("platform", branding::PLATFORM_NAME)]),
            version_line(),
            s.about_disclaimer.to_owned(),
            s.about_credits_heading.to_owned(),
            s.about_credits.to_owned(),
            fill(
                s.about_license_heading,
                &[("license", branding::LICENSE_ID)],
            ),
            s.close.to_owned(),
        ]
    }

    /// The content, the credits and the licence are all there, in order,
    /// centred on one vertical axis, at the dialog's fixed width and inside
    /// the window, in the light and the dark theme.
    #[test]
    fn content_is_centred_in_both_themes() {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let harness = harness(theme);
            assert_eq!(harness.ctx.theme(), theme);
            assert_eq!(
                harness.ctx.global_style().visuals.dark_mode,
                theme == egui::Theme::Dark
            );
            let s = strings();
            harness.get_by_label(&format!("{} {}", s.about_title, branding::PRODUCT_NAME));
            // Under the name, the subtitle without the name again.
            assert_eq!(s.about_tagline, "XCP-ng Center, born again in Rust");
            assert_eq!(
                branding::SUBTITLE,
                format!("{} — {}", branding::PRODUCT_NAME, s.about_tagline)
            );
            assert!(harness.query_by_label(branding::SUBTITLE).is_none());
            assert_eq!(
                fill(s.about_summary, &[("platform", branding::PLATFORM_NAME)]),
                "A desktop console for XCP-ng."
            );
            // The credits and attribution are kept.
            for name in [
                "XCP-ng Center",
                "XenAdmin",
                "Citrix Systems, Inc.",
                "Cloud Software Group, Inc.",
                "CREDITS.md",
            ] {
                assert!(s.about_credits.contains(name), "{name}");
            }
            let rects: Vec<egui::Rect> = content()
                .iter()
                .map(|label| harness.get_by_label(label).rect())
                .collect();
            let axis = rects[0].center().x;
            let screen = harness.ctx.content_rect();
            assert!(
                (axis - screen.center().x).abs() < 2.0,
                "{theme:?}: the window is centred"
            );
            for (label, rect) in content().iter().zip(&rects) {
                assert!(
                    (rect.center().x - axis).abs() < 1.0,
                    "{theme:?}: {label:?} is centred ({} vs {axis})",
                    rect.center().x
                );
                assert!(
                    screen.contains_rect(*rect),
                    "{theme:?}: {label:?} is on screen"
                );
            }
            for pair in rects.windows(2) {
                assert!(
                    pair[0].bottom() <= pair[1].top() + 0.5,
                    "{theme:?}: in order"
                );
            }
            assert_eq!(rects[0].size(), Vec2::splat(ICON_SIZE));
            // Wrapped texts use the dialog's width, and no more.
            let credits = rects[7];
            assert!(
                credits.width() > WIDTH * 0.8 && credits.width() <= WIDTH,
                "{credits:?}"
            );
        }
    }

    /// The licence opens below the credits, and Close closes the window.
    #[test]
    fn license_and_close() {
        let mut harness = harness(egui::Theme::Light);
        let s = strings();
        let first_line = branding::LICENSE_TEXT.lines().next().unwrap();
        assert!(harness.query_by_label_contains(first_line).is_none());
        harness
            .get_by_label(&fill(
                s.about_license_heading,
                &[("license", branding::LICENSE_ID)],
            ))
            .click();
        harness.run();
        harness.get_by_label_contains(first_line);
        // The close button stays where it was: the licence scrolls with the credits.
        harness.get_by_label(s.close).click();
        harness.run();
        assert!(!harness.state().open);
        assert!(harness.query_by_label(s.close).is_none());
    }

    /// The icon decodes: 256 px square, transparent corners (the rounded
    /// square), opaque middle.
    #[test]
    fn icon_loads() {
        let icon = branding::icon_data().expect("icon decodes");
        assert_eq!((icon.width, icon.height), (256, 256));
        let alpha = |x: u32, y: u32| icon.rgba[((y * 256 + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(128, 128), 255);
        let image = branding::icon_image();
        assert_eq!(image.size, [256, 256]);
    }
}
