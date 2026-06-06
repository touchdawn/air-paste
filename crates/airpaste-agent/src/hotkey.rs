#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(windows)]
pub use windows::spawn_remote_paste_listener;

#[cfg(target_os = "macos")]
pub use macos::spawn_remote_paste_listener;

#[cfg(windows)]
pub const REMOTE_PASTE_HOTKEY_PASTES_AFTER_APPLY: bool = true;

#[cfg(target_os = "macos")]
pub const REMOTE_PASTE_HOTKEY_PASTES_AFTER_APPLY: bool = false;

#[cfg(not(any(windows, target_os = "macos")))]
pub const REMOTE_PASTE_HOTKEY_PASTES_AFTER_APPLY: bool = false;

#[cfg(not(any(windows, target_os = "macos")))]
pub fn spawn_remote_paste_listener(
    _sender: tokio::sync::mpsc::UnboundedSender<()>,
) -> anyhow::Result<()> {
    anyhow::bail!("remote paste hotkey MVP is currently implemented only on Windows and macOS")
}
