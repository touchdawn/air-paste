//! Air Paste menu-bar / tray UI (egui + eframe + tray-icon). macOS-first.

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
fn main() -> eframe::Result<()> {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("airpaste-tray currently supports macOS only");
}
