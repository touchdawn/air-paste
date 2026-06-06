#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::Clipboard;

#[cfg(not(windows))]
pub struct Clipboard;

#[cfg(not(windows))]
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
}
