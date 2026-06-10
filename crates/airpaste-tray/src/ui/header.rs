//! The persistent window header: logo + app name on the left, live connection state on the
//! right. Always visible regardless of the active tab.

use eframe::egui;

use crate::app::TrayApp;
use crate::theme;
use crate::ui::status_dot;

pub fn show(app: &TrayApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        crate::icon::paint_logo(ui, 22.0);
        ui.label(egui::RichText::new("AirPaste").size(16.0).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let weak = ui.visuals().weak_text_color();
            if app.agent.connected() {
                ui.label(
                    egui::RichText::new(format!("已连接 · {}", app.agent.device_name()))
                        .size(12.0)
                        .color(weak),
                )
                .on_hover_text(format!(
                    "设备 ID:{}",
                    app.agent.device_id().unwrap_or_else(|| "—".to_string())
                ));
                status_dot(ui, theme::SUCCESS);
            } else if let Some(error) = app.agent.last_error() {
                ui.label(
                    egui::RichText::new("连接失败")
                        .size(12.0)
                        .color(theme::DANGER),
                )
                .on_hover_text(error);
                status_dot(ui, theme::DANGER);
            } else {
                ui.label(egui::RichText::new("连接中…").size(12.0).color(weak));
                status_dot(ui, weak);
            }
        });
    });
}
