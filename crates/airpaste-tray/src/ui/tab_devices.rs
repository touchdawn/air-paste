//! 设备 tab: the server's device list (online state, trust, last seen) plus pairing — pairing
//! lives here because a pair code's whole purpose is adding a device to this list.

use eframe::egui;

use crate::app::TrayApp;
use crate::theme;
use crate::ui::{
    badge, empty_state, hairline, hint, last_seen_text, preview, section_title, status_dot,
    status_line,
};

pub fn show(app: &mut TrayApp, ui: &mut egui::Ui) {
    let devices = app.agent.devices();
    let online_count = devices.iter().filter(|d| d.online).count();
    section_title(ui, &format!("设备 · {online_count}/{} 在线", devices.len()));

    if devices.is_empty() {
        empty_state(ui, "暂无设备", "连接服务器并完成配对后会显示在这里");
    } else {
        for device in &devices {
            ui.horizontal(|ui| {
                status_dot(
                    ui,
                    if device.online {
                        theme::SUCCESS
                    } else {
                        ui.visuals().weak_text_color()
                    },
                );
                ui.label(&device.name).on_hover_text(&device.device_id);
                if device.is_self {
                    hint(ui, "本机");
                }
                if !device.trusted {
                    badge(ui, "未信任", theme::WARNING);
                    if ui
                        .small_button("信任")
                        .on_hover_text("允许该设备加入同步(等效于配对成功)")
                        .clicked()
                    {
                        app.agent.trust_device(device.device_id.clone());
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    hint(ui, &last_seen_text(device));
                });
            });
        }
    }
    if let Some(result) = app.agent.trust_status() {
        match result {
            Ok(message) => status_line(ui, theme::SUCCESS, &message),
            Err(error) => status_line(ui, theme::DANGER, &format!("信任失败:{}", preview(&error))),
        }
    }

    hairline(ui);
    section_title(ui, "添加新设备");
    ui.horizontal(|ui| {
        if ui
            .add_enabled(app.agent.connected(), egui::Button::new("生成配对码"))
            .clicked()
        {
            app.agent.generate_pair_code();
        }
        if !app.agent.connected() {
            hint(ui, "(需先连接)");
        }
    });
    if let Some(result) = app.agent.pair_code() {
        match result {
            Ok(code) => {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&code).monospace().size(20.0).strong());
                    if ui.small_button("复制").clicked() {
                        ui.ctx().copy_text(code.clone());
                    }
                    if ui.small_button("清除").clicked() {
                        app.agent.clear_pair_code();
                    }
                });
                hint(
                    ui,
                    "10 分钟内有效 — 在新设备「设置」页的「配对码」中填入并连接。",
                );
            }
            Err(error) => {
                status_line(ui, theme::DANGER, &format!("生成失败:{}", preview(&error)));
            }
        }
    }
}
