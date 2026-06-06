#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::PasteSimulator;

#[cfg(not(windows))]
pub struct PasteSimulator;

#[cfg(not(windows))]
impl PasteSimulator {
    pub fn new() -> Self {
        Self
    }

    pub fn paste(&self) -> anyhow::Result<()> {
        anyhow::bail!("paste simulation MVP is currently implemented only on Windows")
    }
}
