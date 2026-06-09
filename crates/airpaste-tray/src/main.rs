//! Air Paste menu-bar / tray UI (egui + eframe + tray-icon).
//!
//! The egui `App` and the agent-embedding `run()` are shared across platforms (`app`); the
//! per-OS bits — CJK font location and the "tray-only" window policy (macOS accessory / no
//! Dock, Windows hidden from the taskbar) — live in `platform`.

// Windows: hide the console window in release builds so the app shows just its own window (and
// the single taskbar button that goes with it) instead of an extra console window/taskbar button.
// Debug builds keep the console so the embedded agent's stderr logs stay visible. No-op elsewhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod autostart;
mod config;
mod platform;
mod server;

fn main() -> eframe::Result<()> {
    app::run()
}
