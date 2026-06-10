//! The tray window's views: a persistent header, a tab strip, and one module per tab. This
//! module holds the pieces they share — the `Tab` enum and the small form/list primitives that
//! keep every page aligned the same way.

pub mod header;
pub mod tab_devices;
pub mod tab_inbox;
pub mod tab_send;
pub mod tab_settings;
pub mod toast;

use eframe::egui;

use crate::theme;

/// The four pages of the window, by usage frequency.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Send,
    Inbox,
    Devices,
    Settings,
}

/// Width of the right-aligned label column in form rows; shared so all pages line up.
const FORM_LABEL_WIDTH: f32 = 92.0;

/// A settings-style form row: fixed-width right-aligned label, then the control(s).
pub fn form_row(ui: &mut egui::Ui, label: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        form_label(ui, label);
        add_contents(ui);
    });
}

/// `form_row` for multi-line content (e.g. a checkbox group): label aligns with the first line.
pub fn form_row_top(ui: &mut egui::Ui, label: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal_top(|ui| {
        form_label(ui, label);
        add_contents(ui);
    });
}

fn form_label(ui: &mut egui::Ui, label: &str) {
    ui.allocate_ui_with_layout(
        egui::vec2(FORM_LABEL_WIDTH, ui.spacing().interact_size.y),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            ui.label(egui::RichText::new(label).color(ui.visuals().weak_text_color()));
        },
    );
}

/// An 8px status dot (online/offline, connection state).
pub fn status_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// A small tinted pill, e.g. the "未信任" device badge.
pub fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::none()
        .fill(color.gamma_multiply(0.18))
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(7.0, 1.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(11.0).color(color));
        });
}

/// The accent-filled primary action button (发送 / 保存并连接).
pub fn primary_button(ui: &mut egui::Ui, enabled: bool, text: &str) -> egui::Response {
    let fill = if enabled {
        theme::ACCENT
    } else {
        theme::ACCENT.gamma_multiply(0.55)
    };
    let text_color = egui::Color32::WHITE.gamma_multiply(if enabled { 1.0 } else { 0.85 });
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(text_color))
            .fill(fill)
            .sense(sense),
    )
}

/// A muted 12px section caption ("添加新设备").
pub fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(12.0)
            .color(ui.visuals().weak_text_color()),
    );
}

/// Muted 12px helper text (hints, statuses).
pub fn hint(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(
        egui::RichText::new(text)
            .size(12.0)
            .color(ui.visuals().weak_text_color()),
    )
}

/// A 12px status line in a semantic color (success/danger).
pub fn status_line(ui: &mut egui::Ui, color: egui::Color32, text: &str) {
    ui.label(egui::RichText::new(text).size(12.0).color(color));
}

/// A breathing separator between page sections.
pub fn hairline(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);
}

/// Centered placeholder for an empty list.
pub fn empty_state(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new(title).color(ui.visuals().weak_text_color()));
        if !subtitle.is_empty() {
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(subtitle)
                    .size(12.0)
                    .color(ui.visuals().weak_text_color()),
            );
        }
    });
}

/// A list row with truncating content on the left and trailing controls on the right.
/// `controls_width` reserves space so the content never pushes the buttons off-window.
pub fn list_row(
    ui: &mut egui::Ui,
    controls_width: f32,
    content: impl FnOnce(&mut egui::Ui),
    controls: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        let content_width = (ui.available_width() - controls_width).max(0.0);
        ui.allocate_ui_with_layout(
            egui::vec2(content_width, ui.spacing().interact_size.y),
            egui::Layout::left_to_right(egui::Align::Center),
            content,
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), controls);
    });
}

/// Build identity for the footer / startup log: `v<crate-version> · <git-hash> · <git-date>`.
/// The hash and date come from `build.rs`; a trailing `+` on the hash means the tree was dirty.
pub fn version_line() -> String {
    format!(
        "v{} · {} · {}",
        env!("CARGO_PKG_VERSION"),
        env!("AIRPASTE_GIT_HASH"),
        env!("AIRPASTE_GIT_DATE"),
    )
}

/// Human-readable "last seen" for a device row: "在线", "从未连接", or "N 秒/分钟/小时/天前".
pub fn last_seen_text(device: &airpaste_agent::DeviceInfo) -> String {
    if device.online {
        return "在线".to_string();
    }
    match device.last_seen_secs {
        None => "从未连接".to_string(),
        Some(secs) => {
            let secs = secs.max(0);
            if secs < 60 {
                format!("{secs} 秒前")
            } else if secs < 3600 {
                format!("{} 分钟前", secs / 60)
            } else if secs < 86_400 {
                format!("{} 小时前", secs / 3600)
            } else {
                format!("{} 天前", secs / 86_400)
            }
        }
    }
}

/// Format a byte count as a short human-readable size (B/KB/MB/GB).
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A short, single-line preview of inbox text for display.
pub fn preview(text: &str) -> String {
    let flattened: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let max = 160;
    if flattened.chars().count() > max {
        let truncated: String = flattened.chars().take(max).collect();
        format!("{truncated}…")
    } else {
        flattened
    }
}
