//! 设置 tab: connection settings as an aligned form, the option checkboxes grouped under one
//! label, and the save action pinned bottom-right (settings-dialog convention).

use eframe::egui;

use crate::app::{HotkeyField, TrayApp};
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
    form_row(ui, "发送热键", |ui| {
        hotkey_recorder(app, ui, HotkeyField::Copy);
    });
    form_row(ui, "粘贴热键", |ui| {
        hotkey_recorder(app, ui, HotkeyField::Paste);
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

    let hotkey_error = app.hotkey_inputs_error();
    if let Some(error) = &hotkey_error {
        status_line(ui, theme::DANGER, error);
    }

    ui.horizontal(|ui| {
        hint(ui, "保存后会重启应用以使用新配置");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let can_save = !app.server_url_input.trim().is_empty() && hotkey_error.is_none();
            if primary_button(ui, can_save, "保存并连接").clicked() {
                app.save_and_reconnect();
            }
        });
    });
}

/// Click-to-record hotkey field: click the chord button, press the new combination (Esc or a
/// click elsewhere cancels), and the captured chord lands in the config string. "重置" clears
/// back to empty, i.e. the default chord.
fn hotkey_recorder(app: &mut TrayApp, ui: &mut egui::Ui, field: HotkeyField) {
    let (default_spec, hover) = match field {
        HotkeyField::Copy => (
            airpaste_agent::DEFAULT_COPY_HOTKEY,
            "把当前剪贴板发送到 AirPaste 的全局热键。点击后按下新组合键:至少一个修饰键\
             (Ctrl/Shift/Option/Cmd)加一个字母、数字或 F1–F12,Esc 取消。\
             当前正在生效的热键会被系统拦截,录不进来。保存并连接后生效。",
        ),
        HotkeyField::Paste => (
            airpaste_agent::DEFAULT_PASTE_HOTKEY,
            "从 AirPaste 粘贴远端内容的全局热键,录制方式同发送热键。保存并连接后生效。",
        ),
    };

    // Capture before drawing so the frame that catches the chord already shows the result.
    if app.hotkey_recording == Some(field) {
        match ui.input(capture_chord) {
            ChordCapture::Pending => {}
            ChordCapture::Cancelled => app.hotkey_recording = None,
            ChordCapture::Chord(spec) => {
                match field {
                    HotkeyField::Copy => app.hotkey_copy_input = spec,
                    HotkeyField::Paste => app.hotkey_paste_input = spec,
                }
                app.hotkey_recording = None;
            }
        }
    }

    let recording = app.hotkey_recording == Some(field);
    let current = match field {
        HotkeyField::Copy => app.hotkey_copy_input.trim(),
        HotkeyField::Paste => app.hotkey_paste_input.trim(),
    }
    .to_string();

    let text = if recording {
        let held = held_modifier_label(ui.input(|i| i.modifiers));
        if held.is_empty() {
            "按下组合键…(Esc 取消)".to_string()
        } else {
            format!("{held}+…")
        }
    } else if current.is_empty() {
        format!("默认:{}", chord_label(default_spec))
    } else {
        chord_label(&current)
    };

    let mut rich = egui::RichText::new(text);
    if recording {
        rich = rich.color(theme::ACCENT);
    } else if current.is_empty() {
        rich = rich.color(ui.visuals().weak_text_color());
    }
    let mut button = egui::Button::new(rich);
    if recording {
        button = button.stroke(egui::Stroke::new(1.0, theme::ACCENT));
    }
    let reset_width = 56.0;
    let chord_width = (ui.available_width() - reset_width - ui.spacing().item_spacing.x).max(0.0);
    let response = ui
        .add_sized([chord_width, ui.spacing().interact_size.y], button)
        .on_hover_text(hover);
    if response.clicked() {
        app.hotkey_recording = if recording { None } else { Some(field) };
        // Drop focus so Space/Enter during recording aren't treated as another button click.
        response.surrender_focus();
    } else if recording && response.clicked_elsewhere() {
        app.hotkey_recording = None;
    }

    if ui
        .add_enabled(!current.is_empty(), egui::Button::new("重置"))
        .on_hover_text("恢复默认热键")
        .clicked()
    {
        match field {
            HotkeyField::Copy => app.hotkey_copy_input.clear(),
            HotkeyField::Paste => app.hotkey_paste_input.clear(),
        }
    }
}

enum ChordCapture {
    Pending,
    Cancelled,
    Chord(String),
}

/// Scan this frame's key presses for a recordable chord. Esc cancels; presses that can't form a
/// valid global hotkey (no modifier, unsupported key) are ignored so recording continues.
fn capture_chord(input: &egui::InputState) -> ChordCapture {
    for event in &input.events {
        if let egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } = event
        {
            if *key == egui::Key::Escape {
                return ChordCapture::Cancelled;
            }
            // Build the config-format spec ("ctrl+shift+f9"); HotkeySpec::parse stays the
            // single source of truth for what's allowed.
            let mut spec = String::new();
            for (held, name) in [
                (modifiers.ctrl, "ctrl"),
                (modifiers.shift, "shift"),
                (modifiers.alt, "alt"),
                (modifiers.mac_cmd, "cmd"),
            ] {
                if held {
                    spec.push_str(name);
                    spec.push('+');
                }
            }
            spec.push_str(&key.name().to_ascii_lowercase());
            if airpaste_agent::HotkeySpec::parse(&spec).is_ok() {
                return ChordCapture::Chord(spec);
            }
        }
    }
    ChordCapture::Pending
}

/// Platform label ("Option+C") for a stored chord string, falling back to the raw string.
fn chord_label(spec: &str) -> String {
    airpaste_agent::HotkeySpec::parse(spec)
        .map(|s| s.label())
        .unwrap_or_else(|_| spec.to_string())
}

/// Live "Ctrl+Shift" preview of the held modifiers while recording, in `HotkeySpec::label`
/// order and platform names.
fn held_modifier_label(modifiers: egui::Modifiers) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if modifiers.ctrl {
        parts.push("Ctrl");
    }
    if modifiers.shift {
        parts.push("Shift");
    }
    if modifiers.alt {
        parts.push(if cfg!(target_os = "macos") {
            "Option"
        } else {
            "Alt"
        });
    }
    if modifiers.mac_cmd {
        parts.push("Cmd");
    }
    parts.join("+")
}
