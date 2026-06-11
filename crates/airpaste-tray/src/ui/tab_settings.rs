//! 设置 tab: connection settings as an aligned form, the option checkboxes grouped under one
//! label, and the save action pinned bottom-right (settings-dialog convention).

use eframe::egui;

use crate::app::TrayApp;
use crate::config::TrayConfig;
use crate::server::ServerStatus;
use crate::theme;
use crate::ui::{form_row, form_row_top, hairline, hint, preview, primary_button, status_line};

pub fn show(app: &mut TrayApp, ui: &mut egui::Ui) {
    form_row(ui, "服务器地址", |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut app.server_url_input)
                .desired_width(f32::INFINITY)
                .hint_text("http://主机:端口"),
        );
    });
    form_row(ui, "设备名称", |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut app.device_name_input)
                .desired_width(f32::INFINITY)
                .hint_text(format!(
                    "留空使用默认:{}",
                    airpaste_agent::default_device_name()
                )),
        )
        .on_hover_text("本机在各设备的设备列表中显示的名称。保存并连接后同步到服务器。");
    });
    form_row(ui, "配对码", |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut app.pair_code_input)
                .desired_width(f32::INFINITY)
                .hint_text("首次连接需要,从已有设备生成"),
        );
    });
    form_row(ui, "认证令牌", |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut app.auth_token_input)
                .password(true)
                .desired_width(f32::INFINITY)
                .hint_text("可选"),
        );
    });
    form_row(ui, "简单设备令牌", |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut app.simple_token_input)
                .password(true)
                .desired_width(f32::INFINITY)
                .hint_text("可选,iPhone 快捷指令等使用"),
        )
        .on_hover_text(
            "为本机内嵌服务器开启 /v1/simple 文本接口。简单设备凭此令牌上传/下载文本,\
             内容对服务器是明文(传输建议走 HTTPS)。留空则关闭。",
        );
    });

    hairline(ui);

    form_row_top(ui, "选项", |ui| {
        ui.vertical(|ui| {
            let mut autostart = app.autostart;
            if ui.checkbox(&mut autostart, "开机自启").changed() {
                match crate::autostart::set_autostart(autostart) {
                    Ok(()) => app.autostart = crate::autostart::is_autostart_enabled(),
                    Err(error) => eprintln!("airpaste-tray: failed to set autostart: {error}"),
                }
            }

            let mut isolated = app.agent.isolated();
            if ui
                .checkbox(&mut isolated, "隔离模式")
                .on_hover_text("与系统剪贴板分开,用热键收发")
                .changed()
            {
                app.agent.set_isolated(isolated);
            }

            ui.checkbox(&mut app.simple_mirror_input, "镜像给简单设备")
                .on_hover_text(
                    "Alt+C / 点「发送」的文本额外以明文存入服务器的简单设备收件箱,\
                     供 iPhone 快捷指令等读取。需要服务器配置简单设备令牌。\
                     保存并连接后生效。",
                );

            let mut run_server = app.server.is_running();
            if ui
                .checkbox(&mut run_server, "本机作为服务器")
                .on_hover_text("供其它设备连接本机")
                .changed()
            {
                if run_server {
                    app.server.start();
                } else {
                    app.server.stop();
                }
                let mut config = TrayConfig::load();
                config.run_server = run_server;
                let _ = config.save();
            }
            match app.server.status() {
                ServerStatus::Running => {
                    hint(
                        ui,
                        &format!(
                            "运行中:{}(其它设备用本机 IP:{} 连接)",
                            app.server.bind(),
                            app.server.bind().port()
                        ),
                    );
                }
                ServerStatus::Failed(error) => {
                    status_line(
                        ui,
                        theme::DANGER,
                        &format!("服务器启动失败:{}", preview(&error)),
                    );
                }
                ServerStatus::Off => {}
            }
        });
    });

    hairline(ui);

    ui.horizontal(|ui| {
        hint(ui, "保存后会重启应用以使用新配置");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let can_save = !app.server_url_input.trim().is_empty();
            if primary_button(ui, can_save, "保存并连接").clicked() {
                app.save_and_reconnect();
            }
        });
    });
}
