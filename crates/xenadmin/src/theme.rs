//! Light and dark themes (this console's own; the original had one look).
//!
//! The theme is egui's light or dark visuals, or the operating system's
//! choice ([`ThemeSetting::System`]), saved in the settings and applied by
//! the app each frame it changes. Widgets that paint their own colours
//! (status texts, alert badges, tree icons, graph lines, memory and GPU bars)
//! take them from here, so each colour has a variant that reads on the
//! light and on the dark background. The tests check the contrast of every
//! one against both themes.

use egui::{Color32, ThemePreference, Visuals};
use xen_core::ThemeSetting;

/// egui's theme preference for the setting.
pub fn preference(setting: ThemeSetting) -> ThemePreference {
    match setting {
        ThemeSetting::System => ThemePreference::System,
        ThemeSetting::Light => ThemePreference::Light,
        ThemeSetting::Dark => ThemePreference::Dark,
    }
}

/// A meaning a text or a mark is coloured by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Running, succeeded, healthy (green).
    Ok,
    /// Halted, failed, broken (red).
    Error,
    /// Maintenance mode, suspended, a major alert (orange).
    Warning,
    /// A warning-priority alert (yellow).
    Caution,
    /// Paused, informational (blue).
    Info,
    /// An operation in progress (amber).
    Busy,
}

impl Tone {
    #[cfg(test)]
    pub const ALL: [Tone; 6] = [
        Tone::Ok,
        Tone::Error,
        Tone::Warning,
        Tone::Caution,
        Tone::Info,
        Tone::Busy,
    ];
}

/// The colour of `tone` on the given background brightness: a darker shade
/// on light panels, a lighter one on dark panels, both readable as text.
pub fn tone_for(dark: bool, tone: Tone) -> Color32 {
    let (light, dark_variant) = match tone {
        Tone::Ok => (rgb(0x1e7b34), rgb(0x4ac26b)),
        Tone::Error => (rgb(0xc0392b), rgb(0xff6b5b)),
        Tone::Warning => (rgb(0xb35400), rgb(0xff9a4d)),
        Tone::Caution => (rgb(0x8a6d00), rgb(0xe8c547)),
        Tone::Info => (rgb(0x1f63c6), rgb(0x6aa8ff)),
        Tone::Busy => (rgb(0xa85d00), rgb(0xf0a030)),
    };
    if dark { dark_variant } else { light }
}

/// The colour of `tone` in the visuals `ui` uses.
pub fn tone(ui: &egui::Ui, tone: Tone) -> Color32 {
    tone_for(ui.visuals().dark_mode, tone)
}

/// A neutral grey for marks (disabled hosts, unknown states, event lines).
pub fn muted(visuals: &Visuals) -> Color32 {
    if visuals.dark_mode {
        Color32::from_gray(140)
    } else {
        Color32::from_gray(112)
    }
}

/// Graph line colours, by position (the d3 "category10" set).
const SERIES: [Color32; 10] = [
    Color32::from_rgb(0x1f, 0x77, 0xb4),
    Color32::from_rgb(0xff, 0x7f, 0x0e),
    Color32::from_rgb(0x2c, 0xa0, 0x2c),
    Color32::from_rgb(0xd6, 0x27, 0x28),
    Color32::from_rgb(0x94, 0x67, 0xbd),
    Color32::from_rgb(0x8c, 0x56, 0x4b),
    Color32::from_rgb(0xe3, 0x77, 0xc2),
    Color32::from_rgb(0x7f, 0x7f, 0x7f),
    Color32::from_rgb(0xbc, 0xbd, 0x22),
    Color32::from_rgb(0x17, 0xbe, 0xcf),
];

/// The `index`th graph line colour, adjusted to stand out (3:1) from the
/// plot background of `visuals`.
pub fn series(visuals: &Visuals, index: usize) -> Color32 {
    readable(SERIES[index % SERIES.len()], visuals.extreme_bg_color, 3.0)
}

/// Black or white, whichever reads better on `fill`.
pub fn text_on(fill: Color32) -> Color32 {
    if contrast(Color32::BLACK, fill) > contrast(Color32::WHITE, fill) {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

/// A bar segment's fill, lightened on dark backgrounds until it stands out
/// from `background` (1.5:1).
pub fn segment_fill(fill: Color32, background: Color32) -> Color32 {
    readable(fill, background, 1.5)
}

/// `color` moved towards black or white (away from `background`) until its
/// contrast with `background` is at least `min`.
pub fn readable(color: Color32, background: Color32, min: f32) -> Color32 {
    let target = if luminance(background) > 0.18 {
        Color32::BLACK
    } else {
        Color32::WHITE
    };
    let mut c = color;
    let mut step = 0;
    while contrast(c, background) < min && step < 20 {
        step += 1;
        c = mix(color, target, step as f32 / 20.0);
    }
    c
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// WCAG relative luminance.
fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG contrast ratio of two colours (1 to 21).
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn both() -> [Visuals; 2] {
        [Visuals::light(), Visuals::dark()]
    }

    /// Status texts and alert badges read as body text (4.5:1) on panels
    /// and windows in both themes.
    #[test]
    fn tones_read_as_text_in_both_themes() {
        for v in both() {
            for t in Tone::ALL {
                let c = tone_for(v.dark_mode, t);
                for bg in [v.panel_fill, v.window_fill, v.extreme_bg_color] {
                    let r = contrast(c, bg);
                    assert!(r >= 4.5, "{t:?} dark={} on {bg:?}: {r:.2}", v.dark_mode);
                }
            }
            // Marks (icons, event lines) need 3:1.
            assert!(contrast(muted(&v), v.panel_fill) >= 3.0);
            assert!(contrast(muted(&v), v.extreme_bg_color) >= 3.0);
        }
    }

    /// Graph lines stand out from the plot background in both themes.
    #[test]
    fn graph_lines_stand_out_in_both_themes() {
        for v in both() {
            for i in 0..SERIES.len() {
                let r = contrast(series(&v, i), v.extreme_bg_color);
                assert!(r >= 3.0, "series {i} dark={}: {r:.2}", v.dark_mode);
            }
            // Distinct colours stay distinct after the adjustment.
            let all: std::collections::BTreeSet<_> = (0..SERIES.len())
                .map(|i| series(&v, i).to_array())
                .collect();
            assert_eq!(all.len(), SERIES.len());
        }
    }

    /// Captions on bar segments (memory, GPU) read on every fill, and the
    /// segments stand out from the bar's background in both themes.
    #[test]
    fn bar_segments_and_captions_read_in_both_themes() {
        let fills = [
            crate::tabs::memory::XEN_COLOR,
            crate::tabs::memory::DOM0_COLOR,
            crate::tabs::memory::VM_COLORS[0],
            crate::tabs::memory::VM_COLORS[1],
            crate::tabs::memory::USED_COLOR,
            crate::dev_ui::SEGMENT_COLORS[0],
            crate::dev_ui::SEGMENT_COLORS[1],
        ];
        for v in both() {
            for fill in fills {
                let f = segment_fill(fill, v.extreme_bg_color);
                assert!(contrast(f, v.extreme_bg_color) >= 1.5, "{fill:?}");
                assert!(
                    contrast(text_on(f), f) >= 4.5,
                    "{fill:?} dark={}",
                    v.dark_mode
                );
            }
        }
        // The GPU bar is black in both themes.
        for fill in crate::dev_ui::SEGMENT_COLORS {
            let f = segment_fill(fill, Color32::BLACK);
            assert!(contrast(f, Color32::BLACK) >= 1.5);
            assert!(contrast(text_on(f), f) >= 4.5);
        }
    }

    #[test]
    fn contrast_and_adjustment() {
        assert!((contrast(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.01);
        assert!((contrast(Color32::RED, Color32::RED) - 1.0).abs() < 0.01);
        let c = readable(Color32::from_rgb(0xbc, 0xbd, 0x22), Color32::WHITE, 3.0);
        assert!(contrast(c, Color32::WHITE) >= 3.0);
        // Already readable: unchanged.
        let navy = Color32::from_rgb(25, 25, 112);
        assert_eq!(readable(navy, Color32::WHITE, 3.0), navy);
        assert_eq!(preference(ThemeSetting::Dark), ThemePreference::Dark);
        assert_eq!(preference(ThemeSetting::System), ThemePreference::System);
    }
}
