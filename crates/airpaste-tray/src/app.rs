//! Cross-platform tray UI: an eframe (egui) window plus a tray-icon menu, driving an embedded
//! Air Paste agent. The agent runs on a background Tokio runtime; the UI observes it via an
//! `AgentHandle` and repaints on a steady cadence. Per-OS window/font behaviour is in
//! `crate::platform`.

use std::{path::PathBuf, sync::mpsc, thread, time::Duration};

use airpaste_agent::{AgentHandle, FileDownloadState, InboxEntry, SendStatus};
use eframe::egui;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

use crate::config::TrayConfig;
use crate::platform;
use crate::server::{ServerController, ServerStatus};

pub fn run() -> eframe::Result<()> {
    // The tray is the isolated-mode UX, so default to isolated unless the user overrides it.
    // Starting isolated registers both global hotkeys, which keeps the runtime toggle fully
    // functional (hotkeys cannot be re-registered after launch).
    if std::env::var_os("AIRPASTE_CLIPBOARD_MODE").is_none() {
        std::env::set_var("AIRPASTE_CLIPBOARD_MODE", "isolated");
    }
    airpaste_agent::init_tracing();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        commit = env!("AIRPASTE_GIT_HASH"),
        date = env!("AIRPASTE_GIT_DATE"),
        "airpaste-tray starting"
    );
    let mut args = airpaste_agent::parse_args();

    // Overlay persisted tray config where the parsed args are still at their defaults, so
    // explicit CLI flags / env vars always win (keeps the smoke scripts working), then the
    // saved config, then the agent default.
    let config = TrayConfig::load();
    if args.server_url == airpaste_agent::DEFAULT_SERVER_URL {
        if let Some(url) = config.server_url.clone().filter(|u| !u.trim().is_empty()) {
            args.server_url = url;
        }
    }
    if args.pair_code.is_none() {
        args.pair_code = config.pair_code.clone().filter(|c| !c.trim().is_empty());
    }
    if args.auth_token.is_none() {
        args.auth_token = config.auth_token.clone().filter(|t| !t.is_empty());
    }

    // Snapshot the values the settings panel needs before `args` is moved into the agent.
    let settings = Settings {
        state_path: args.state_path(),
        server_url: args.server_url.clone(),
        auth_token: args.auth_token.clone().unwrap_or_default(),
        pair_code: args.pair_code.clone().unwrap_or_default(),
    };

    let run_server = config.run_server;

    // eframe owns the main thread, so the Tokio runtime + agent live on a background thread.
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("airpaste-agent-rt".to_string())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(error) => {
                    eprintln!("failed to start agent runtime: {error}");
                    return;
                }
            };
            rt.block_on(async move {
                let server = ServerController::new(tokio::runtime::Handle::current());
                // Start the embedded server BEFORE the agent and wait for it to listen, so an
                // agent pointed at localhost does not race the bind (it does not retry register).
                if run_server {
                    server.start();
                    server.wait_until_ready().await;
                }
                let handle = airpaste_agent::spawn_embedded(args);
                let _ = tx.send((handle, server));
                // Keep the runtime (and the agent task) alive for the life of the process.
                std::future::pending::<()>().await;
            });
        })
        .expect("spawn agent runtime thread");

    let (agent, server) = rx.recv().expect("agent handle from runtime thread");

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([440.0, 520.0])
            .with_title("AirPaste")
            .with_icon(window_icon()),
        ..Default::default()
    };
    // Run as a tray-only app: macOS accessory (no Dock), Windows hidden from the taskbar.
    platform::apply_tray_window_policy(&mut options);

    eframe::run_native(
        "AirPaste",
        options,
        Box::new(move |cc| Ok(Box::new(TrayApp::new(cc, agent, settings, server)))),
    )
}

/// Connection settings captured at launch to seed the settings panel.
struct Settings {
    state_path: PathBuf,
    server_url: String,
    auth_token: String,
    pair_code: String,
}

struct TrayApp {
    // Kept alive for the lifetime of the app so the icon stays in the menu bar / tray.
    _tray: TrayIcon,
    show_id: MenuId,
    quit_id: MenuId,
    agent: AgentHandle,
    // Set when the user picks Quit, so the window close that follows actually exits (a plain
    // window close just hides the window — the app keeps living in the tray).
    quitting: bool,
    // Settings panel state.
    state_path: PathBuf,
    server_url_input: String,
    auth_token_input: String,
    pair_code_input: String,
    // Once connected, the one-shot pair code is cleared from the saved config (reusing a
    // consumed code is a hard error on the next connect). Done once per launch.
    pair_code_cleared: bool,
    // Cached "start at login" state (read once; refreshed when the checkbox is toggled).
    autostart: bool,
    // Embedded control-plane server (the "run a server on this machine" toggle).
    server: ServerController,
    // Manual "send text" panel: the draft, and whether the draft should be cleared once the
    // in-flight send reports Sent (kept on failure so the user can retry).
    send_input: String,
    send_clear_pending: bool,
}

/// Install a CJK-capable font as the primary UI font so Chinese text renders (egui's default
/// fonts have no CJK glyphs). Best-effort: if no candidate is found, labels still render in
/// whatever glyphs the defaults provide.
fn install_cjk_font(ctx: &egui::Context) {
    let Some(bytes) = platform::CJK_FONT_CANDIDATES
        .iter()
        .find_map(|path| std::fs::read(path).ok())
    else {
        eprintln!("airpaste-tray: no CJK font found; UI text may show missing glyphs");
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("cjk".to_owned(), egui::FontData::from_owned(bytes));
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "cjk".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("cjk".to_owned());
    ctx.set_fonts(fonts);
}

impl TrayApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        agent: AgentHandle,
        settings: Settings,
        server: ServerController,
    ) -> Self {
        install_cjk_font(&cc.egui_ctx);

        let show = MenuItem::new("显示 AirPaste", true, None);
        let quit = MenuItem::new("退出 AirPaste", true, None);
        let menu = Menu::new();
        menu.append(&show).expect("append show");
        menu.append(&PredefinedMenuItem::separator()).expect("append sep");
        menu.append(&quit).expect("append quit");

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("AirPaste")
            .with_icon(app_icon())
            .build()
            .expect("build tray icon");

        Self {
            _tray: tray,
            show_id: show.id().clone(),
            quit_id: quit.id().clone(),
            agent,
            quitting: false,
            state_path: settings.state_path,
            server_url_input: settings.server_url,
            auth_token_input: settings.auth_token,
            pair_code_input: settings.pair_code,
            pair_code_cleared: false,
            autostart: crate::autostart::is_autostart_enabled(),
            server,
            send_input: String::new(),
            send_clear_pending: false,
        }
    }

    /// Persist the entered connection settings and relaunch so the embedded agent connects with
    /// them. We re-exec the process (rather than restart the agent in place) so the OS reclaims
    /// the agent's bound peer port, global hotkeys, and mDNS registration — no leaked tasks.
    fn save_and_reconnect(&self) {
        let server_url = self.server_url_input.trim().to_string();
        if server_url.is_empty() {
            return;
        }
        let mut config = TrayConfig::load();
        let server_changed = config.last_server_url.as_deref() != Some(server_url.as_str());
        config.server_url = Some(server_url.clone());
        config.auth_token = Some(self.auth_token_input.trim().to_string())
            .filter(|t: &String| !t.is_empty());
        config.pair_code = Some(self.pair_code_input.trim().to_string())
            .filter(|c: &String| !c.is_empty());
        config.last_server_url = Some(server_url);
        if let Err(error) = config.save() {
            eprintln!("airpaste-tray: failed to save config: {error}");
        }
        // Switching servers invalidates the device id registered on the old server; clearing it
        // forces a clean re-registration on the new one (keys are preserved).
        if server_changed {
            clear_device_id(&self.state_path);
        }
        relaunch();
    }
}

/// Clear the cached `device_id` in the agent state file (keeping the identity keys) so the next
/// launch re-registers. Best-effort; a missing/locked file is ignored.
fn clear_device_id(state_path: &std::path::Path) {
    let state_file = airpaste_agent::StateFile::new(state_path.to_path_buf());
    if let Ok(mut state) = state_file.load() {
        if state.device_id.is_some() {
            state.device_id = None;
            let _ = state_file.save(&state);
        }
    }
}

/// Relaunch this executable (which will read the freshly saved tray config) and exit. Passing no
/// connection flags means the new process picks up the persisted config.
fn relaunch() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    std::process::exit(0);
}

impl eframe::App for TrayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll the tray menu channel and refresh the live status on a steady cadence; tray
        // clicks and agent state changes do not arrive as winit events.
        ctx.request_repaint_after(Duration::from_millis(200));
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.quit_id {
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if event.id == self.show_id {
                // Make it reliably reappear in front: unhide, restore if minimized, then focus.
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }

        // A plain window close (red button / X) hides the window and keeps the app in the tray;
        // only the tray's Quit truly exits.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        // OS drag-and-drop: files dropped anywhere on the window are published as a file
        // manifest (recipients pull them with the remote-paste hotkey, like a copied file).
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.agent.send_files(dropped);
        }
        // While files hover over the window, dim it with a drop hint.
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("file-drop-overlay"),
            ));
            let rect = ctx.screen_rect();
            painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(160));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "松开以发送文件",
                egui::FontId::proportional(22.0),
                egui::Color32::WHITE,
            );
        }

        // Once the (re)connection succeeds, drop the one-shot pair code from the saved config so
        // a later relaunch does not re-confirm an already-consumed code (a hard error).
        if !self.pair_code_cleared && self.agent.connected() {
            self.pair_code_cleared = true;
            let mut config = TrayConfig::load();
            if config.pair_code.is_some() {
                config.pair_code = None;
                let _ = config.save();
            }
            self.pair_code_input.clear();
        }

        // Pinned footer with the build identity, so "which build am I running?" is always visible.
        egui::TopBottomPanel::bottom("version_bar").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.weak(version_line());
            ui.add_space(2.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("AirPaste");
            ui.add_space(4.0);

            if self.agent.connected() {
                ui.colored_label(egui::Color32::from_rgb(0x2e, 0xb8, 0x72), "● 已连接");
            } else if let Some(error) = self.agent.last_error() {
                ui.colored_label(
                    egui::Color32::from_rgb(0xd9, 0x3a, 0x3a),
                    format!("✕ 连接失败:{}", preview(&error)),
                );
            } else {
                ui.colored_label(egui::Color32::GRAY, "○ 连接中…");
            }

            ui.add_space(6.0);
            egui::Grid::new("agent-info")
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    ui.label("设备");
                    ui.label(self.agent.device_name());
                    ui.end_row();

                    ui.label("设备 ID");
                    ui.label(self.agent.device_id().unwrap_or_else(|| "—".to_string()));
                    ui.end_row();
                });

            ui.add_space(6.0);
            let mut isolated = self.agent.isolated();
            if ui
                .checkbox(&mut isolated, "隔离模式(与系统剪贴板分开)")
                .changed()
            {
                self.agent.set_isolated(isolated);
            }

            let mut autostart = self.autostart;
            if ui.checkbox(&mut autostart, "开机自启").changed() {
                match crate::autostart::set_autostart(autostart) {
                    Ok(()) => self.autostart = crate::autostart::is_autostart_enabled(),
                    Err(error) => eprintln!("airpaste-tray: failed to set autostart: {error}"),
                }
            }

            let mut run_server = self.server.is_running();
            if ui
                .checkbox(&mut run_server, "本机作为服务器(供其它设备连接)")
                .changed()
            {
                if run_server {
                    self.server.start();
                } else {
                    self.server.stop();
                }
                let mut config = TrayConfig::load();
                config.run_server = run_server;
                let _ = config.save();
            }
            match self.server.status() {
                ServerStatus::Running => {
                    ui.weak(format!("服务器运行中:{}(其它设备用本机 IP:{} 连接)", self.server.bind(), self.server.bind().port()));
                }
                ServerStatus::Failed(error) => {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xd9, 0x3a, 0x3a),
                        format!("服务器启动失败:{}", preview(&error)),
                    );
                }
                ServerStatus::Off => {}
            }

            ui.add_space(6.0);
            let devices = self.agent.devices();
            let online_count = devices.iter().filter(|d| d.online).count();
            egui::CollapsingHeader::new(format!("设备({}/{} 在线)", online_count, devices.len()))
                .default_open(self.server.is_running())
                .show(ui, |ui| {
                    if devices.is_empty() {
                        ui.weak("(暂无 — 需先连接并完成配对)");
                    } else {
                        for device in &devices {
                            ui.horizontal(|ui| {
                                if device.online {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(0x2e, 0xb8, 0x72),
                                        "●",
                                    );
                                } else {
                                    ui.colored_label(egui::Color32::GRAY, "○");
                                }
                                let name = if device.is_self {
                                    format!("{}(本机)", device.name)
                                } else {
                                    device.name.clone()
                                };
                                ui.label(name).on_hover_text(&device.device_id);
                                if !device.trusted {
                                    ui.weak("· 未信任");
                                }
                                ui.weak(format!("· {}", last_seen_text(device)));
                            });
                        }
                    }
                });

            ui.add_space(6.0);
            egui::CollapsingHeader::new("设置 / 连接")
                .default_open(!self.agent.connected())
                .show(ui, |ui| {
                    egui::Grid::new("settings")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("服务器地址");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.server_url_input)
                                    .hint_text("http://主机:端口"),
                            );
                            ui.end_row();

                            ui.label("配对码");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.pair_code_input)
                                    .hint_text("首次连接需要"),
                            );
                            ui.end_row();

                            ui.label("认证令牌");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.auth_token_input)
                                    .hint_text("可选"),
                            );
                            ui.end_row();
                        });
                    ui.add_space(4.0);
                    if ui.button("保存并连接").clicked() {
                        self.save_and_reconnect();
                    }
                    ui.weak("保存后会重启应用以使用新配置。");

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label("给新设备配对:");
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(self.agent.connected(), egui::Button::new("生成配对码"))
                            .clicked()
                        {
                            self.agent.generate_pair_code();
                        }
                        if !self.agent.connected() {
                            ui.weak("(需先连接)");
                        }
                    });
                    if let Some(result) = self.agent.pair_code() {
                        match result {
                            Ok(code) => {
                                ui.horizontal(|ui| {
                                    ui.monospace(&code);
                                    if ui.button("复制").clicked() {
                                        ctx.copy_text(code.clone());
                                    }
                                    if ui.button("清除").clicked() {
                                        self.agent.clear_pair_code();
                                    }
                                });
                                ui.weak("在新设备的「配对码」里填入此码并连接(10 分钟内有效)。");
                            }
                            Err(error) => {
                                ui.colored_label(
                                    egui::Color32::from_rgb(0xd9, 0x3a, 0x3a),
                                    format!("生成失败:{}", preview(&error)),
                                );
                            }
                        }
                    }
                });

            if let Some(progress) = self.agent.transfer_progress() {
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                let fraction = if progress.total > 0 {
                    progress.done as f32 / progress.total as f32
                } else {
                    0.0
                };
                ui.label(format!(
                    "下载中 {}/{}:{}",
                    progress.done,
                    progress.total,
                    preview(&progress.current)
                ));
                ui.add(egui::ProgressBar::new(fraction).desired_width(360.0));
            }

            if let Some(pending) = self.agent.pending_files() {
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.label(format!(
                    "待接收文件:{} 个(共 {}) — 按 {}+V 接收",
                    pending.count,
                    human_size(pending.total_size),
                    airpaste_agent::HOTKEY_MOD_NAME,
                ));
                egui::ScrollArea::vertical()
                    .id_salt("pending-files")
                    .max_height(80.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for name in &pending.names {
                            ui.weak(preview(name));
                        }
                    });
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label("发送文字到其它设备:");
            ui.add(
                egui::TextEdit::multiline(&mut self.send_input)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("输入或粘贴要发送的文字…"),
            );
            ui.horizontal(|ui| {
                let can_send = self.agent.connected() && !self.send_input.trim().is_empty();
                if ui.add_enabled(can_send, egui::Button::new("发送")).clicked() {
                    self.agent.send_text(self.send_input.clone());
                    self.send_clear_pending = true;
                }
                if !self.agent.connected() {
                    ui.weak("(需先连接)");
                }
                match self.agent.send_text_status() {
                    Some(SendStatus::Sending) => {
                        ui.weak("发送中…");
                    }
                    Some(SendStatus::Sent) => {
                        if self.send_clear_pending {
                            self.send_clear_pending = false;
                            self.send_input.clear();
                        }
                        ui.colored_label(egui::Color32::from_rgb(0x2e, 0xb8, 0x72), "✓ 已发送");
                    }
                    Some(SendStatus::Failed(error)) => {
                        self.send_clear_pending = false;
                        ui.colored_label(
                            egui::Color32::from_rgb(0xd9, 0x3a, 0x3a),
                            format!("✗ 发送失败:{}", preview(&error)),
                        );
                    }
                    None => {}
                }
            });
            ui.weak("提示:把文件或文件夹拖进窗口即可发送文件。");
            match self.agent.send_files_status() {
                Some(SendStatus::Sending) => {
                    ui.weak("文件处理中…(大文件需要先计算校验和)");
                }
                Some(SendStatus::Sent) => {
                    ui.colored_label(
                        egui::Color32::from_rgb(0x2e, 0xb8, 0x72),
                        format!(
                            "✓ 文件已发出(对方按 {}+V 接收)",
                            airpaste_agent::HOTKEY_MOD_NAME
                        ),
                    );
                }
                Some(SendStatus::Failed(error)) => {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xd9, 0x3a, 0x3a),
                        format!("✗ 文件发送失败:{}", preview(&error)),
                    );
                }
                None => {}
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label("收件箱(最新在上):");
            let history = self.agent.inbox_history();
            if history.is_empty() {
                ui.weak("(暂无)");
            } else {
                egui::ScrollArea::vertical()
                    .id_salt("inbox-history")
                    .max_height(180.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (i, entry) in history.iter().enumerate() {
                            match entry {
                                InboxEntry::Text(text) => {
                                    ui.horizontal(|ui| {
                                        if ui.button("复制").clicked() {
                                            ctx.copy_text(text.clone());
                                        }
                                        ui.label(preview(text));
                                    });
                                }
                                InboxEntry::Files {
                                    id,
                                    count,
                                    total_size,
                                    names,
                                    state,
                                } => {
                                    ui.horizontal(|ui| {
                                        match state {
                                            FileDownloadState::Idle => {
                                                if ui.button("下载").clicked() {
                                                    self.agent.download_inbox_files(*id);
                                                }
                                            }
                                            FileDownloadState::Downloading => {
                                                ui.add_enabled(
                                                    false,
                                                    egui::Button::new("下载中…"),
                                                );
                                            }
                                            // Downloaded: the button re-copies the local file
                                            // references to the clipboard.
                                            FileDownloadState::Done(_) => {
                                                if ui.button("复制").clicked() {
                                                    self.agent.copy_inbox_files(*id);
                                                }
                                            }
                                            FileDownloadState::Failed(_) => {
                                                if ui.button("重试").clicked() {
                                                    self.agent.download_inbox_files(*id);
                                                }
                                            }
                                        }
                                        ui.label(format!(
                                            "🗂 {} 个文件(共 {}):{}",
                                            count,
                                            human_size(*total_size),
                                            preview(&names.join(", "))
                                        ));
                                    });
                                    match state {
                                        FileDownloadState::Done(_) => {
                                            ui.weak("已下载,文件引用已放入剪贴板,可直接粘贴。");
                                        }
                                        FileDownloadState::Failed(error) => {
                                            ui.colored_label(
                                                egui::Color32::from_rgb(0xd9, 0x3a, 0x3a),
                                                format!("✗ 下载失败:{}", preview(error)),
                                            );
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            if i + 1 < history.len() {
                                ui.separator();
                            }
                        }
                    });
            }
        });
    }
}

/// Build identity for the footer / startup log: `v<crate-version> · <git-hash> · <git-date>`.
/// The hash and date come from `build.rs`; a trailing `+` on the hash means the tree was dirty.
fn version_line() -> String {
    format!(
        "v{} · {} · {}",
        env!("CARGO_PKG_VERSION"),
        env!("AIRPASTE_GIT_HASH"),
        env!("AIRPASTE_GIT_DATE"),
    )
}

/// Human-readable "last seen" for a device row: "在线", "从未连接", or "N 秒/分钟/小时/天前".
fn last_seen_text(device: &airpaste_agent::DeviceInfo) -> String {
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
fn human_size(bytes: u64) -> String {
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
fn preview(text: &str) -> String {
    let flattened: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let max = 160;
    if flattened.chars().count() > max {
        let truncated: String = flattened.chars().take(max).collect();
        format!("{truncated}…")
    } else {
        flattened
    }
}

/// The tray-menu icon.
fn app_icon() -> Icon {
    let (rgba, size) = icon_rgba();
    Icon::from_rgba(rgba, size, size).expect("valid icon")
}

/// The window / taskbar icon (same artwork as the tray icon).
fn window_icon() -> egui::IconData {
    let (rgba, size) = icon_rgba();
    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}

/// Render the AirPaste app icon as RGBA: a white paper-plane on a rounded blue tile. Drawn in
/// code so we ship no image asset; `icon_rgba` is the single swap point if a designed PNG is
/// added later (decode it to RGBA via the workspace `image` crate).
fn icon_rgba() -> (Vec<u8>, u32) {
    const S: u32 = 64;
    const BLUE: [u8; 4] = [0x2e, 0x7d, 0xf6, 0xff];
    const WHITE: [u8; 4] = [0xff, 0xff, 0xff, 0xff];
    let mut rgba = vec![0u8; (S * S * 4) as usize];
    for y in 0..S {
        for x in 0..S {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let px = if !in_rounded_rect(fx, fy, S as f32, 13.0) {
                [0, 0, 0, 0]
            } else if in_paper_plane(fx, fy) {
                WHITE
            } else {
                BLUE
            };
            let i = ((y * S + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&px);
        }
    }
    (rgba, S)
}

/// Point-in-rounded-square test for a `size`×`size` tile (2px transparent margin, corner `r`).
fn in_rounded_rect(x: f32, y: f32, size: f32, r: f32) -> bool {
    let (lo, hi) = (2.0, size - 2.0);
    if x < lo || x > hi || y < lo || y > hi {
        return false;
    }
    let cx = x.clamp(lo + r, hi - r);
    let cy = y.clamp(lo + r, hi - r);
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= r * r
}

/// A paper-plane dart (tip top-right, two wings, concave trailing edge) for a 64px tile.
fn in_paper_plane(x: f32, y: f32) -> bool {
    let tip = (50.0, 13.0);
    let wing_upper = (13.0, 31.0);
    let wing_lower = (33.0, 51.0);
    let fold = (31.0, 34.0); // pulled toward the tip so the tail reads as two wings
    in_triangle(x, y, tip, wing_upper, fold) || in_triangle(x, y, tip, fold, wing_lower)
}

fn in_triangle(x: f32, y: f32, a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let edge = |p: (f32, f32), q: (f32, f32)| (x - q.0) * (p.1 - q.1) - (p.0 - q.0) * (y - q.1);
    let (d1, d2, d3) = (edge(a, b), edge(b, c), edge(c, a));
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}
