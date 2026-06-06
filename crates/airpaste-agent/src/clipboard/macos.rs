use anyhow::Context;
use std::path::PathBuf;

pub struct Clipboard;

impl Clipboard {
    pub fn new() -> Self {
        Self
    }

    pub fn get_text(&self) -> anyhow::Result<Option<String>> {
        let mut clipboard =
            arboard::Clipboard::new().context("failed to access macOS pasteboard")?;
        match clipboard.get_text() {
            Ok(text) => Ok(Some(text)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(error) => Err(error).context("failed to read text from macOS pasteboard"),
        }
    }

    pub fn set_text(&self, text: &str) -> anyhow::Result<()> {
        let mut clipboard =
            arboard::Clipboard::new().context("failed to access macOS pasteboard")?;
        clipboard
            .set_text(text)
            .context("failed to write text to macOS pasteboard")
    }

    pub fn get_files(&self) -> anyhow::Result<Option<Vec<PathBuf>>> {
        let mut clipboard =
            arboard::Clipboard::new().context("failed to access macOS pasteboard")?;
        match clipboard.get().file_list() {
            Ok(paths) if paths.is_empty() => Ok(None),
            Ok(paths) => Ok(Some(paths)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(error) => Err(error).context("failed to read file URLs from macOS pasteboard"),
        }
    }

    pub fn set_files(&self, paths: &[PathBuf]) -> anyhow::Result<()> {
        if paths.is_empty() {
            return Ok(());
        }

        let mut clipboard =
            arboard::Clipboard::new().context("failed to access macOS pasteboard")?;
        clipboard
            .set()
            .file_list(paths)
            .context("failed to write file URLs to macOS pasteboard")
    }
}
