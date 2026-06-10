#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(windows)]
pub use windows::PasteSimulator;

#[cfg(target_os = "macos")]
pub use macos::PasteSimulator;

#[cfg(not(any(windows, target_os = "macos")))]
pub struct PasteSimulator;

#[cfg(not(any(windows, target_os = "macos")))]
impl PasteSimulator {
    pub fn new() -> Self {
        Self
    }

    pub fn request_accessibility(&self) -> bool {
        true
    }

    pub fn paste(&self) -> anyhow::Result<()> {
        anyhow::bail!("paste simulation MVP is currently implemented only on Windows and macOS")
    }
}
