//! Centralized look & feel: the color tokens every view shares, plus `apply`, which tunes
//! egui's built-in dark/light themes (rounding, spacing, type scale) once at startup. Views
//! must not hardcode colors — add a token here instead.

use eframe::egui::{self, Color32};

/// Brand blue — the tray icon tile, primary buttons, the selected tab.
pub const ACCENT: Color32 = Color32::from_rgb(0x2e, 0x7d, 0xf6);
/// Positive states: connected, sent, downloaded.
pub const SUCCESS: Color32 = Color32::from_rgb(0x2e, 0xb8, 0x72);
/// Failures: connection / send / download errors.
pub const DANGER: Color32 = Color32::from_rgb(0xd9, 0x3a, 0x3a);
/// Attention without failure: untrusted devices.
pub const WARNING: Color32 = Color32::from_rgb(0xd9, 0x8e, 0x2b);

/// Tune both egui themes; the active one follows the OS (egui's default `ThemePreference`).
pub fn apply(ctx: &egui::Context) {
    use egui::{FontFamily, FontId, TextStyle};
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 5.0);
        style.spacing.interact_size.y = 26.0;
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(17.0, FontFamily::Proportional),
        );
        style
            .text_styles
            .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(14.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        );
    });
    ctx.style_mut_of(egui::Theme::Dark, |style| tune(&mut style.visuals, true));
    ctx.style_mut_of(egui::Theme::Light, |style| tune(&mut style.visuals, false));
}

fn tune(v: &mut egui::Visuals, dark: bool) {
    let rounding = egui::Rounding::same(6.0);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.rounding = rounding;
    }
    v.window_rounding = egui::Rounding::same(10.0);
    // Selected tabs / text selection: translucent accent so it reads in both themes.
    v.selection.bg_fill = ACCENT.gamma_multiply(if dark { 0.45 } else { 0.30 });
    v.selection.stroke = egui::Stroke::new(
        1.0,
        if dark {
            Color32::WHITE
        } else {
            Color32::from_rgb(0x0b, 0x3d, 0x91)
        },
    );
    v.hyperlink_color = ACCENT;
    if dark {
        v.panel_fill = Color32::from_rgb(0x1d, 0x1e, 0x21);
        v.extreme_bg_color = Color32::from_rgb(0x15, 0x16, 0x19);
        v.faint_bg_color = Color32::from_rgb(0x27, 0x28, 0x2c);
    } else {
        v.panel_fill = Color32::from_rgb(0xf6, 0xf6, 0xf8);
        v.extreme_bg_color = Color32::WHITE;
        v.faint_bg_color = Color32::from_rgb(0xec, 0xec, 0xef);
    }
}
