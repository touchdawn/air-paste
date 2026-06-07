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
}

impl TrayApp {
    fn new(_cc: &eframe::CreationContext<'_>, agent: AgentHandle) -> Self {
        let show = MenuItem::new("Show AirPaste", true, None);
        let quit = MenuItem::new("Quit AirPaste", true, None);
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
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if event.id == self.show_id {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("AirPaste");
            ui.add_space(4.0);

            if self.agent.connected() {
                ui.colored_label(egui::Color32::from_rgb(0x2e, 0xb8, 0x72), "● Connected");
            } else {
                ui.colored_label(egui::Color32::GRAY, "○ Connecting…");
            }

            ui.add_space(6.0);
            egui::Grid::new("agent-info")
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Device");
                    ui.label(self.agent.device_name());
                    ui.end_row();

                    ui.label("Device ID");
                    ui.label(self.agent.device_id().unwrap_or_else(|| "—".to_string()));
                    ui.end_row();

                    ui.label("Mode");
                    ui.label(if self.agent.isolated() {
                        "isolated"
                    } else {
                        "system"
                    });
                    ui.end_row();
                });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label("Latest received (isolated inbox):");
            match self.agent.latest_inbox() {
                Some(text) => {
                    ui.label(preview(&text));
                    if ui.button("Copy to clipboard").clicked() {
                        ctx.copy_text(text);
                    }
                }
                None => {
                    ui.weak("(nothing yet)");
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
