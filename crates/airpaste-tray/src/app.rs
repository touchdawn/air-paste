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
    airpaste_agent::install_panic_logger();
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
    if args.device_name.is_none() {
        args.device_name = config.device_name.clone().filter(|n| !n.trim().is_empty());
    }
    if args.hotkey_copy.is_none() {
        args.hotkey_copy = config.hotkey_copy.clone().filter(|h| !h.trim().is_empty());
    }
    if args.hotkey_paste.is_none() {
        args.hotkey_paste = config.hotkey_paste.clone().filter(|h| !h.trim().is_empty());
    }
    // An explicit --simple-mirror=true (or env) wins; otherwise the saved checkbox applies.
    args.simple_mirror = args.simple_mirror || config.simple_mirror;

    // Snapshot the values the settings panel needs before `args` is moved into the agent.
    let settings = Settings {
        state_path: args.state_path(),
        server_url: args.server_url.clone(),
        auth_token: args.auth_token.clone().unwrap_or_default(),
        pair_code: args.pair_code.clone().unwrap_or_default(),
        device_name: args.device_name.clone().unwrap_or_default(),
        hotkey_copy: args.hotkey_copy.clone().unwrap_or_default(),
        hotkey_paste: args.hotkey_paste.clone().unwrap_or_default(),
        simple_token: config.simple_token.clone().unwrap_or_default(),
        simple_mirror: args.simple_mirror,
    };

    let run_server = config.run_server;
    let simple_token = config.simple_token.clone();

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
                let server = ServerController::new(tokio::runtime::Handle::current(), simple_token);
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
    device_name: String,
    hotkey_copy: String,
    hotkey_paste: String,
    simple_token: String,
    simple_mirror: bool,
}

pub(crate) struct TrayApp {
    // Kept alive for the lifetime of the app so the icon stays in the menu bar / tray.
    _tray: TrayIcon,
    // Tray-menu clicks, forwarded by the handler installed in `new` (which also wakes the
    // event loop — see there). Drained once per `logic` tick.
    menu_events: mpsc::Receiver<MenuEvent>,
    show_id: MenuId,
    quit_id: MenuId,
    pub(crate) agent: AgentHandle,
    // Set when the user picks Quit, so the window close that follows actually exits (a plain
    // window close just hides/minimizes the window — the app keeps living in the tray).
    quitting: bool,
    pub(crate) tab: Tab,
    // Settings panel state.
    state_path: PathBuf,
    pub(crate) server_url_input: String,
    pub(crate) auth_token_input: String,
    pub(crate) pair_code_input: String,
    // Custom device name; empty means "use the platform default".
    pub(crate) device_name_input: String,
    // Custom hotkey chords; empty means "use the defaults" (Alt+C / Alt+V).
    pub(crate) hotkey_copy_input: String,
    pub(crate) hotkey_paste_input: String,
    // Which hotkey field (if any) is currently capturing a chord from the keyboard.
    pub(crate) hotkey_recording: Option<HotkeyField>,
    // Simple-device access (e.g. iPhone Shortcuts): embedded-server token + mirror toggle.
    // Both apply on save-and-relaunch like the connection settings.
    pub(crate) simple_token_input: String,
    pub(crate) simple_mirror_input: bool,
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
    // Image pasted into the send tab (Cmd+V), staged for preview until the user confirms.
    pub(crate) pasted_image: Option<PendingImage>,
    // Failure from the paste capture or the off-thread PNG staging; shown on the send tab.
    pub(crate) pasted_image_error: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    // Hotkey-feedback HUD currently on screen (see `ui::toast`).
    pub(crate) toast: Option<ui::toast::ActiveToast>,
}

/// The two hotkey settings fields, for tracking which one is recording a chord.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HotkeyField {
    Copy,
    Paste,
}

/// A clipboard bitmap staged on the send tab: the raw pixels for the eventual PNG encode plus
/// the texture that previews it.
pub(crate) struct PendingImage {
    pub(crate) rgba: Vec<u8>,
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) texture: egui::TextureHandle,
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
    fonts.font_data.insert(
        "cjk".to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
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
        platform::install_paste_monitor();

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

        // Forward menu events through our own channel and wake the event loop on each click.
        // While the window is hidden eframe only re-runs `logic` when a repaint is requested,
        // so without this nudge a tray click could sit unnoticed in the queue (and the menu
        // would feel laggy even when visible, since the idle cadence is 200–500ms).
        let menu_events = {
            let ctx = cc.egui_ctx.clone();
            let (tx, rx) = mpsc::channel();
            MenuEvent::set_event_handler(Some(move |event| {
                let _ = tx.send(event);
                ctx.request_repaint();
            }));
            rx
        };

        Self {
            _tray: tray,
            menu_events,
            show_id: show.id().clone(),
            quit_id: quit.id().clone(),
            agent,
            quitting: false,
            tab: Tab::default(),
            state_path: settings.state_path,
            server_url_input: settings.server_url,
            auth_token_input: settings.auth_token,
            pair_code_input: settings.pair_code,
            device_name_input: settings.device_name,
            hotkey_copy_input: settings.hotkey_copy,
            hotkey_paste_input: settings.hotkey_paste,
            hotkey_recording: None,
            simple_token_input: settings.simple_token,
            simple_mirror_input: settings.simple_mirror,
            pair_code_cleared: false,
            autostart: crate::autostart::is_autostart_enabled(),
            server,
            send_input: String::new(),
            send_clear_pending: false,
            pasted_image: None,
            pasted_image_error: std::sync::Arc::new(std::sync::Mutex::new(None)),
            toast: None,
        }
    }

    /// A paste chord landed while the send tab is up: route file and image pastes, which the
    /// TextEdit cannot take, to the agent. Plain text falls through to the TextEdit's own
    /// paste handling.
    fn handle_send_tab_paste(&mut self, ctx: &egui::Context) {
        let clipboard = airpaste_agent::clipboard::Clipboard::new();

        // Files first: Finder writes the file names as a text alternate alongside the URLs,
        // so "has text" would otherwise shadow a copied file. Sending matches the drop zone;
        // the synthesized text paste is dropped so the names don't land in the draft.
        if let Ok(Some(paths)) = clipboard.get_files() {
            if !paths.is_empty() {
                ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Paste(_))));
                self.agent.send_files(paths);
                return;
            }
        }

        if matches!(clipboard.get_text(), Ok(Some(text)) if !text.is_empty()) {
            return;
        }

        match clipboard.get_image() {
            Ok(Some(image)) => {
                *self.pasted_image_error.lock().unwrap() = None;
                let texture = ctx.load_texture(
                    "pasted-image",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [image.width, image.height],
                        &image.rgba,
                    ),
                    egui::TextureOptions::LINEAR,
                );
                self.pasted_image = Some(PendingImage {
                    rgba: image.rgba,
                    width: image.width,
                    height: image.height,
                    texture,
                });
            }
            Ok(None) => {}
            Err(error) => {
                *self.pasted_image_error.lock().unwrap() = Some(format!("{error:#}"));
            }
        }
    }

    /// Validate the hotkey inputs (empty = default chord); `Err` holds a user-facing message.
    /// The same parse runs in the agent at startup — this just fails before the relaunch.
    pub(crate) fn hotkey_inputs_error(&self) -> Option<String> {
        let parse = |input: &str, default: &str| {
            let input = input.trim();
            airpaste_agent::HotkeySpec::parse(if input.is_empty() { default } else { input })
        };
        let copy = match parse(&self.hotkey_copy_input, airpaste_agent::DEFAULT_COPY_HOTKEY) {
            Ok(spec) => spec,
            Err(error) => return Some(format!("发送热键无效:{error}")),
        };
        let paste = match parse(
            &self.hotkey_paste_input,
            airpaste_agent::DEFAULT_PASTE_HOTKEY,
        ) {
            Ok(spec) => spec,
            Err(error) => return Some(format!("粘贴热键无效:{error}")),
        };
        if copy == paste {
            return Some(format!("发送和粘贴热键不能相同({})", copy.label()));
        }
        None
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
        config.device_name =
            Some(self.device_name_input.trim().to_string()).filter(|n: &String| !n.is_empty());
        config.hotkey_copy =
            Some(self.hotkey_copy_input.trim().to_string()).filter(|h: &String| !h.is_empty());
        config.hotkey_paste =
            Some(self.hotkey_paste_input.trim().to_string()).filter(|h: &String| !h.is_empty());
        config.simple_token =
            Some(self.simple_token_input.trim().to_string()).filter(|t: &String| !t.is_empty());
        config.simple_mirror = self.simple_mirror_input;
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
    /// The background tick. eframe 0.34 splits the per-frame callback: `logic` runs before
    /// every `ui` pass AND keeps running while the window is hidden or minimized (`ui` is
    /// skipped for invisible windows — emilk/egui#7950). Everything that must stay alive in
    /// the background lives here: the tray-menu poll, the hotkey toasts, and the repaint
    /// cadence — for a hidden window eframe only calls `logic` again when a repaint was
    /// requested, so the `request_repaint_after` below is also what keeps this ticking.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll the tray menu channel and refresh the live status on a steady cadence; tray
        // clicks and agent state changes do not arrive as winit events. While hidden or
        // minimized nothing is on screen, so tick slower — just often enough that the tray
        // menu and hotkey toasts stay responsive.
        let backgrounded = ctx.input(|i| {
            let viewport = i.viewport();
            viewport.minimized.unwrap_or(false) || !viewport.visible().unwrap_or(true)
        });
        ctx.request_repaint_after(Duration::from_millis(if backgrounded { 500 } else { 200 }));
        while let Ok(event) = self.menu_events.try_recv() {
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

        // Hotkey-feedback HUD (Alt+C / Alt+V): shown as a transient always-on-top floater.
        // Driven from `logic` so it appears even while the main window is hidden.
        ui::toast::update(self, ctx);

        // A plain window close (red button / X) hides the window — the app keeps living in
        // the tray, and the tray menu's Show brings it back; only the tray's Quit truly
        // exits. Hiding (rather than minimizing) is safe again on Windows: eframe 0.29 spun
        // a full core on hidden windows with the update loop stopped (the close→minimize
        // workaround in 31b5882), but 0.34 handles invisible windows properly
        // (emilk/egui#7905, #7950) — measured ~0% CPU with `logic` still ticking.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        // Native paste chord (Cmd+V): drained every tick so a paste on another tab cannot
        // fire later, but only the send tab accepts rich (file/image) pastes.
        if platform::take_paste_request() && self.tab == Tab::Send {
            self.handle_send_tab_paste(ctx);
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
    }

    /// The visible-window pass: everything that draws. Only runs while the window is shown.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

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
            let rect = ctx.content_rect();
            painter.rect_filled(
                rect,
                egui::CornerRadius::ZERO,
                egui::Color32::from_black_alpha(160),
            );
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "松开以发送文件",
                egui::FontId::proportional(22.0),
                egui::Color32::WHITE,
            );
        }

        // Persistent chrome: header + tab strip on top, build identity pinned at the bottom.
        egui::Panel::top("header")
            .frame(
                egui::Frame::side_top_panel(&ctx.global_style()).inner_margin(egui::Margin {
                    left: 14,
                    right: 14,
                    top: 12,
                    bottom: 8,
                }),
            )
            .show_inside(ui, |ui| {
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

        egui::Panel::bottom("version_bar").show_inside(ui, |ui| {
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(ui::version_line())
                    .size(11.0)
                    .color(ui.visuals().weak_text_color()),
            );
            ui.add_space(2.0);
        });

        let frame = egui::Frame::central_panel(&ctx.global_style())
            .inner_margin(egui::Margin::symmetric(14, 12));
        egui::CentralPanel::default()
            .frame(frame)
            .show_inside(ui, |ui| match self.tab {
                Tab::Send => ui::tab_send::show(self, ui),
                Tab::Inbox => ui::tab_inbox::show(self, ui),
                Tab::Devices => ui::tab_devices::show(self, ui),
                Tab::Settings => ui::tab_settings::show(self, ui),
            });
    }
}
