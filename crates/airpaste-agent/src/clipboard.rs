#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(windows)]
pub use windows::Clipboard;

#[cfg(target_os = "macos")]
pub use macos::Clipboard;

/// An RGBA bitmap read from the system clipboard (row-major, 4 bytes per pixel).
pub struct ClipboardImage {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

#[cfg(not(any(windows, target_os = "macos")))]
pub struct Clipboard;

#[cfg(not(any(windows, target_os = "macos")))]
impl Clipboard {
    pub fn new() -> Self {
        Self
    }

    pub fn get_text(&self) -> anyhow::Result<Option<String>> {
        anyhow::bail!("clipboard MVP is currently implemented only on Windows")
    }

    pub fn set_text(&self, _text: &str) -> anyhow::Result<()> {
        anyhow::bail!("clipboard MVP is currently implemented only on Windows")
    }

    pub fn get_files(&self) -> anyhow::Result<Option<Vec<std::path::PathBuf>>> {
        anyhow::bail!("file clipboard MVP is currently implemented only on Windows")
    }

    pub fn set_files(&self, _paths: &[std::path::PathBuf]) -> anyhow::Result<()> {
        anyhow::bail!("file clipboard MVP is currently implemented only on Windows")
    }

    pub fn get_image(&self) -> anyhow::Result<Option<ClipboardImage>> {
        anyhow::bail!("image clipboard is currently implemented only on macOS")
    }
}
