//! Air Paste menu-bar / tray UI (egui + eframe + tray-icon).
//!
//! The egui `App` and the agent-embedding `run()` are shared across platforms (`app`); the
//! per-OS bits — CJK font location and the "tray-only" window policy (macOS accessory / no
//! Dock, Windows hidden from the taskbar) — live in `platform`.

mod app;
mod platform;

fn main() -> eframe::Result<()> {
    app::run()
}
