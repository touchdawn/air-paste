//! Air Paste menu-bar / tray UI (egui + eframe + tray-icon).
//!
//! The egui `App` and the agent-embedding `run()` are shared across platforms (`app`); the
//! per-OS bits — CJK font location and the "tray-only" window policy (macOS accessory / no
//! Dock, Windows hidden from the taskbar) — live in `platform`.

// Windows: hide the console window in release builds so the app is truly tray-only (a console
// subsystem exe otherwise gets its own taskbar button, defeating `with_taskbar(false)`). Debug
// builds keep the console so the embedded agent's stderr logs stay visible. No-op elsewhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod platform;

fn main() -> eframe::Result<()> {
    app::run()
}
