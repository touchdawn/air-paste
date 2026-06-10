//! 收件箱 tab: a pending-files banner (files waiting for the receive hotkey), the live
//! download progress, and the received history with per-entry copy/download actions.

use airpaste_agent::{FileDownloadState, InboxEntry};
use eframe::egui;

use crate::app::TrayApp;
use crate::theme;
use crate::ui::{empty_state, hint, human_size, list_row, preview, status_line};

pub fn show(app: &mut TrayApp, ui: &mut egui::Ui) {
    let mod_name = airpaste_agent::HOTKEY_MOD_NAME;

    if let Some(pending) = app.agent.pending_files() {
        egui::Frame::none()
            .fill(theme::ACCENT.gamma_multiply(0.12))
            .rounding(egui::Rounding::same(8.0))
            .inner_margin(egui::Margin::symmetric(10.0, 8.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(format!(
                    "{} 个文件待接收(共 {}) — 按 {}+V 接收",
                    pending.count,
                    human_size(pending.total_size),
                    mod_name,
                ));
                hint(ui, &preview(&pending.names.join("、")));
            });
        ui.add_space(2.0);
    }

    if let Some(progress) = app.agent.transfer_progress() {
        let fraction = if progress.total > 0 {
            progress.done as f32 / progress.total as f32
        } else {
            0.0
        };
        ui.add(
            egui::ProgressBar::new(fraction)
                .desired_height(6.0)
                .desired_width(f32::INFINITY),
        );
        hint(
            ui,
            &format!(
                "下载中 {}/{}:{}",
                progress.done,
                progress.total,
                preview(&progress.current)
            ),
        );
        ui.add_space(2.0);
    }

    let history = app.agent.inbox_history();
    if history.is_empty() {
        empty_state(
            ui,
            "还没有收到内容",
            &format!("其它设备发送、或按 {mod_name}+C 推送后会显示在这里"),
        );
        return;
    }

    egui::ScrollArea::vertical()
        .id_salt("inbox-history")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, entry) in history.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                match entry {
                    InboxEntry::Text(text) => {
                        list_row(
                            ui,
                            64.0,
                            |ui| {
                                ui.add(egui::Label::new(preview(text)).truncate());
                            },
                            |ui| {
                                if ui.button("复制").clicked() {
                                    ui.ctx().copy_text(text.clone());
                                }
                            },
                        );
                    }
                    InboxEntry::Files {
                        id,
                        count,
                        total_size,
                        names,
                        state,
                    } => {
                        list_row(
                            ui,
                            76.0,
                            |ui| {
                                ui.add(
                                    egui::Label::new(format!(
                                        "🗂 {} 个文件 · {} · {}",
                                        count,
                                        human_size(*total_size),
                                        preview(&names.join("、")),
                                    ))
                                    .truncate(),
                                );
                            },
                            |ui| match state {
                                FileDownloadState::Idle => {
                                    if ui.button("下载").clicked() {
                                        app.agent.download_inbox_files(*id);
                                    }
                                }
                                FileDownloadState::Downloading => {
                                    ui.add_enabled(false, egui::Button::new("下载中…"));
                                }
                                // Downloaded: the button re-copies the local file references.
                                FileDownloadState::Done(_) => {
                                    if ui.button("复制").clicked() {
                                        app.agent.copy_inbox_files(*id);
                                    }
                                }
                                FileDownloadState::Failed(_) => {
                                    if ui.button("重试").clicked() {
                                        app.agent.download_inbox_files(*id);
                                    }
                                }
                            },
                        );
                        match state {
                            FileDownloadState::Done(_) => {
                                hint(ui, "已下载,文件引用已放入剪贴板,可直接粘贴。");
                            }
                            FileDownloadState::Failed(error) => {
                                status_line(
                                    ui,
                                    theme::DANGER,
                                    &format!("✗ 下载失败:{}", preview(error)),
                                );
                            }
                            _ => {}
                        }
                    }
                }
            }
        });
}
