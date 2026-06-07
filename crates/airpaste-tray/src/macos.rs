//! macOS menu-bar UI: an eframe (egui) window plus a tray-icon menu, driving an embedded
//! Air Paste agent. The agent runs on a background Tokio runtime; the UI observes it via an
//! `AgentHandle` and repaints on a steady cadence.

use std::{sync::mpsc, thread, time::Duration};

use airpaste_agent::AgentHandle;
use eframe::egui;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

pub fn run() -> eframe::Result<()> {
    // The tray is the isolated-mode UX, so default to isolated unless the user overrides it.
    // Starting isolated registers both global hotkeys, which keeps the runtime toggle fully
    // functional (hotkeys cannot be re-registered after launch).
    if std::env::var_os("AIRPASTE_CLIPBOARD_MODE").is_none() {
        std::env::set_var("AIRPASTE_CLIPBOARD_MODE", "isolated");
    }
    airpaste_agent::init_tracing();
    let args = airpaste_agent::parse_args();

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
                let handle = airpaste_agent::spawn_embedded(args);
                let _ = tx.send(handle);
                // Keep the runtime (and the agent task) alive for the life of the process.
                std::future::pending::<()>().await;
            });
        })
        .expect("spawn agent runtime thread");

    let agent = rx.recv().expect("agent handle from runtime thread");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 320.0])
            .with_title("AirPaste"),
        // Run as a menu-bar (accessory) app: no Dock icon, no app menu bar.
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            builder.with_activation_policy(ActivationPolicy::Accessory);
        })),
        ..Default::default()
    };
    eframe::run_native(
        "AirPaste",
        options,
        Box::new(move |cc| Ok(Box::new(TrayApp::new(cc, agent)))),
    )
}

struct TrayApp {
    // Kept alive for the lifetime of the app so the icon stays in the menu bar.
    _tray: TrayIcon,
    show_id: MenuId,
    quit_id: MenuId,
    agent: AgentHandle,
    // Set when the user picks Quit, so the window close that follows actually exits (a plain
    // window close just hides the window — the app keeps living in the menu bar).
    quitting: bool,
}

/// macOS CJK fonts to try, in order. Arial Unicode is a single-face .ttf (loads cleanly);
/// the .ttc collections are fallbacks (egui loads face 0).
const CJK_FONT_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/STHeiti Light.ttc",
];

/// Install a CJK-capable font as the primary UI font so Chinese text renders (egui's default
/// fonts have no CJK glyphs). Best-effort: if no candidate is found, labels still render in
/// whatever glyphs the defaults provide.
fn install_cjk_font(ctx: &egui::Context) {
    let Some(bytes) = CJK_FONT_CANDIDATES
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
    fn new(cc: &eframe::CreationContext<'_>, agent: AgentHandle) -> Self {
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
        }
    }
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
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }

        // A plain window close (red button) hides the window and keeps the app in the menu
        // bar; only the tray's Quit truly exits.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("AirPaste");
            ui.add_space(4.0);

            if self.agent.connected() {
                ui.colored_label(egui::Color32::from_rgb(0x2e, 0xb8, 0x72), "● 已连接");
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

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label("最近收到(隔离收件箱):");
            match self.agent.latest_inbox() {
                Some(text) => {
                    ui.label(preview(&text));
                    if ui.button("复制到剪贴板").clicked() {
                        ctx.copy_text(text);
                    }
                }
                None => {
                    ui.weak("(暂无)");
                }
            }
        });
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

/// A simple 32x32 RGBA icon (filled blue disc) so we do not need to ship an image asset yet.
fn app_icon() -> Icon {
    const SIZE: u32 = 32;
    let center = (SIZE / 2) as i32;
    let radius = 14i32;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE as i32 {
        for x in 0..SIZE as i32 {
            let dx = x - center;
            let dy = y - center;
            if dx * dx + dy * dy <= radius * radius {
                let i = ((y as u32 * SIZE + x as u32) * 4) as usize;
                rgba[i] = 0x2e;
                rgba[i + 1] = 0x7d;
                rgba[i + 2] = 0xf6;
                rgba[i + 3] = 0xff;
            }
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid icon")
}
