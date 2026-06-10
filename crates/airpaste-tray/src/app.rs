//! Cross-platform tray UI: an eframe (egui) window plus a tray-icon menu, driving an embedded
//! Air Paste agent. The agent runs on a background Tokio runtime; the UI observes it via an
//! `AgentHandle` and repaints on a steady cadence. This module owns the app state and the
//! window chrome (header / tab strip / footer); the per-tab views live in `crate::ui`, the
//! look & feel in `crate::theme`, and per-OS window/font behaviour in `crate::platform`.

use std::{path::PathBuf, sync::mpsc, thread, time::Duration};

use airpaste_agent::AgentHandle;
use eframe::egui;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    TrayIcon, TrayIconBuilder,
};

use crate::config::TrayConfig;
use crate::server::ServerController;
use crate::ui::{self, Tab};
use crate::{icon, platform, theme};

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
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
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
            .with_inner_size([460.0, 560.0])
            .with_min_inner_size([420.0, 480.0])
            .with_title("AirPaste")
            .with_icon(icon::window_icon()),
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

pub(crate) struct TrayApp {
    // Kept alive for the lifetime of the app so the icon stays in the menu bar / tray.
    _tray: TrayIcon,
    show_id: MenuId,
    quit_id: MenuId,
    pub(crate) agent: AgentHandle,
    // Set when the user picks Quit, so the window close that follows actually exits (a plain
    // window close just hides the window — the app keeps living in the tray).
    quitting: bool,
    pub(crate) tab: Tab,
    // Settings panel state.
    state_path: PathBuf,
    pub(crate) server_url_input: String,
    pub(crate) auth_token_input: String,
    pub(crate) pair_code_input: String,
    // Once connected, the one-shot pair code is cleared from the saved config (reusing a
    // consumed code is a hard error on the next connect). Done once per launch.
    pair_code_cleared: bool,
    // Cached "start at login" state (read once; refreshed when the checkbox is toggled).
    pub(crate) autostart: bool,
    // Embedded control-plane server (the "run a server on this machine" toggle).
    pub(crate) server: ServerController,
    // Manual "send text" panel: the draft, and whether the draft should be cleared once the
    // in-flight send reports Sent (kept on failure so the user can retry).
    pub(crate) send_input: String,
    pub(crate) send_clear_pending: bool,
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
        theme::apply(&cc.egui_ctx);

        let show = MenuItem::new("显示 AirPaste", true, None);
        let quit = MenuItem::new("退出 AirPaste", true, None);
        let menu = Menu::new();
        menu.append(&show).expect("append show");
        menu.append(&PredefinedMenuItem::separator())
            .expect("append sep");
        menu.append(&quit).expect("append quit");

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("AirPaste")
            .with_icon(icon::app_icon())
            .build()
            .expect("build tray icon");

        Self {
            _tray: tray,
            show_id: show.id().clone(),
            quit_id: quit.id().clone(),
            agent,
            quitting: false,
            tab: Tab::default(),
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
    pub(crate) fn save_and_reconnect(&self) {
        let server_url = self.server_url_input.trim().to_string();
        if server_url.is_empty() {
            return;
        }
        let mut config = TrayConfig::load();
        let server_changed = config.last_server_url.as_deref() != Some(server_url.as_str());
        config.server_url = Some(server_url.clone());
        config.auth_token =
            Some(self.auth_token_input.trim().to_string()).filter(|t: &String| !t.is_empty());
        config.pair_code =
            Some(self.pair_code_input.trim().to_string()).filter(|c: &String| !c.is_empty());
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
            self.tab = Tab::Send; // the send statuses live there
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

        // Persistent chrome: header + tab strip on top, build identity pinned at the bottom.
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin {
                    left: 14.0,
                    right: 14.0,
                    top: 12.0,
                    bottom: 8.0,
                }),
            )
            .show(ctx, |ui| {
                ui::header::show(self, ui);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, Tab::Send, "发送");
                    let inbox_label = if self.agent.pending_files().is_some() {
                        "收件箱 ●"
                    } else {
                        "收件箱"
                    };
                    ui.selectable_value(&mut self.tab, Tab::Inbox, inbox_label);
                    ui.selectable_value(&mut self.tab, Tab::Devices, "设备");
                    ui.selectable_value(&mut self.tab, Tab::Settings, "设置");
                });
            });

        egui::TopBottomPanel::bottom("version_bar").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(ui::version_line())
                    .size(11.0)
                    .color(ui.visuals().weak_text_color()),
            );
            ui.add_space(2.0);
        });

        let frame = egui::Frame::central_panel(&ctx.style())
            .inner_margin(egui::Margin::symmetric(14.0, 12.0));
        egui::CentralPanel::default()
            .frame(frame)
            .show(ctx, |ui| match self.tab {
                Tab::Send => ui::tab_send::show(self, ui),
                Tab::Inbox => ui::tab_inbox::show(self, ui),
                Tab::Devices => ui::tab_devices::show(self, ui),
                Tab::Settings => ui::tab_settings::show(self, ui),
            });
    }
}
