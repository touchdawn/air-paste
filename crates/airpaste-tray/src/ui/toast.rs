//! Hotkey-feedback HUD: a small undecorated always-on-top window near the top of the screen,
//! shown briefly after Alt+C / Alt+V so the user sees that the hotkey actually fired (the
//! hotkeys are otherwise silent, which reads as "nothing happened").
//!
//! Rendered as an egui immediate viewport from the main `update()` loop, which keeps ticking
//! every 200ms even while the tray window is hidden (the same cadence that polls the tray
//! menu). The viewport is created inactive and mouse-transparent so it can never steal focus
//! from the app the user is pasting into.

use std::time::{Duration, Instant};

use eframe::egui;

use crate::app::TrayApp;
use crate::theme;

/// How long a toast stays up. Errors linger longer so they can actually be read.
const INFO_DURATION: Duration = Duration::from_millis(1200);
const ERROR_DURATION: Duration = Duration::from_secs(3);

/// A toast currently on screen.
pub(crate) struct ActiveToast {
    toast: airpaste_agent::Toast,
    until: Instant,
}

/// Drain agent toasts (a burst keeps only the newest) and render the active one, if any.
pub(crate) fn update(app: &mut TrayApp, ctx: &egui::Context) {
    if let Some(toast) = app.agent.take_toasts().pop() {
        let duration = if toast.is_error {
            ERROR_DURATION
        } else {
            INFO_DURATION
        };
        app.toast = Some(ActiveToast {
            toast,
            until: Instant::now() + duration,
        });
    }

    let Some(active) = &app.toast else { return };
    if Instant::now() >= active.until {
        app.toast = None;
        return;
    }
    // Tick faster than the idle 200ms cadence so the toast disappears on time.
    ctx.request_repaint_after(Duration::from_millis(100));
    show(ctx, &active.toast);
}

fn show(ctx: &egui::Context, toast: &airpaste_agent::Toast) {
    let is_error = toast.is_error;
    let text = toast.text.clone();
    // Errors carry longer text; give them a wider, two-line box.
    let size = if is_error {
        egui::vec2(420.0, 64.0)
    } else {
        egui::vec2(260.0, 44.0)
    };
    // Top-center of the monitor (best-effort: monitor size may be unknown on some platforms).
    let monitor = ctx
        .input(|i| i.viewport().monitor_size)
        .filter(|s| s.x > 0.0)
        .unwrap_or(egui::vec2(1440.0, 900.0));
    let position = egui::pos2((monitor.x - size.x) / 2.0, 64.0);

    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of("airpaste-toast"),
        egui::ViewportBuilder::default()
            .with_title("AirPaste")
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_active(false)
            .with_mouse_passthrough(true)
            .with_taskbar(false)
            .with_inner_size(size)
            .with_position(position),
        move |ctx, _class| {
            let fill = egui::Color32::from_rgba_unmultiplied(0x1d, 0x1e, 0x21, 235);
            let accent = if is_error {
                theme::DANGER
            } else {
                theme::SUCCESS
            };
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::none()
                        .fill(fill)
                        .rounding(12.0)
                        .stroke(egui::Stroke::new(1.0, accent.gamma_multiply(0.6)))
                        .inner_margin(egui::Margin::symmetric(14.0, 8.0)),
                )
                .show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            egui::RichText::new(&text)
                                .size(14.0)
                                .color(egui::Color32::WHITE),
                        );
                    });
                });
        },
    );
}
