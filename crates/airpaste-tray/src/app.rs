//! Cross-platform tray UI: an eframe (egui) window plus a tray-icon menu, driving an embedded
//! Air Paste agent. The agent runs on a background Tokio runtime; the UI observes it via an
//! `AgentHandle` and repaints on a steady cadence. Per-OS window/font behaviour is in
//! `crate::platform`.

use std::{sync::mpsc, thread, time::Duration};

use airpaste_agent::AgentHandle;
use eframe::egui;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

use crate::platform;

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

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 420.0])
            .with_title("AirPaste")
            .with_icon(window_icon()),
        ..Default::default()
    };
    // Run as a tray-only app: macOS accessory (no Dock), Windows hidden from the taskbar.
    platform::apply_tray_window_policy(&mut options);

    eframe::run_native(
        "AirPaste",
        options,
        Box::new(move |cc| Ok(Box::new(TrayApp::new(cc, agent)))),
    )
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

        // A plain window close (red button / X) hides the window and keeps the app in the tray;
        // only the tray's Quit truly exits.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

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

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label("收件箱(隔离,最新在上):");
            let history = self.agent.inbox_history();
            if history.is_empty() {
                ui.weak("(暂无)");
            } else {
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (i, text) in history.iter().enumerate() {
                            ui.horizontal(|ui| {
                                if ui.button("复制").clicked() {
                                    ctx.copy_text(text.clone());
                                }
                                ui.label(preview(text));
                            });
                            if i + 1 < history.len() {
                                ui.separator();
                            }
                        }
                    });
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
