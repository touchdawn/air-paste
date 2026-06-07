//! macOS menu-bar UI: an eframe (egui) window plus a tray-icon menu in the same event loop.
//!
//! Scaffold stage — the agent is wired in a later step; for now this validates the menu-bar +
//! window plumbing.

use std::time::Duration;

use eframe::egui;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([380.0, 260.0])
            .with_title("AirPaste"),
        ..Default::default()
    };
    eframe::run_native(
        "AirPaste",
        options,
        Box::new(|cc| Ok(Box::new(TrayApp::new(cc)))),
    )
}

struct TrayApp {
    // Kept alive for the lifetime of the app so the icon stays in the menu bar.
    _tray: TrayIcon,
    show_id: MenuId,
    quit_id: MenuId,
}

impl TrayApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
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
        }
    }
}

impl eframe::App for TrayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Tray menu clicks arrive on a global channel, not as winit events, so poll it on a
        // steady cadence to keep the menu responsive.
        ctx.request_repaint_after(Duration::from_millis(150));
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
            ui.label("Clipboard sync — menu-bar UI scaffold");
            ui.add_space(8.0);
            ui.separator();
            ui.label("Look for the AirPaste icon in the menu bar.");
            ui.label("Tray menu: Show / Quit.");
        });
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
