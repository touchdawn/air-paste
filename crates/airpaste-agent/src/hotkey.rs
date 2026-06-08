#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(windows)]
pub use windows::spawn_hotkey_listener;

#[cfg(target_os = "macos")]
pub use macos::spawn_hotkey_listener;

/// A global hotkey the agent listens for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyAction {
    /// Alt+V (Option+V on macOS) — paste remote content (files, or in isolated mode the inbox text).
    PasteRemote,
    /// Alt+C (Option+C on macOS) — capture the current selection into the AirPaste channel (isolated).
    CopyToAirPaste,
}

#[cfg(windows)]
pub const REMOTE_PASTE_HOTKEY_PASTES_AFTER_APPLY: bool = true;

#[cfg(target_os = "macos")]
pub const REMOTE_PASTE_HOTKEY_PASTES_AFTER_APPLY: bool = false;

#[cfg(not(any(windows, target_os = "macos")))]
pub const REMOTE_PASTE_HOTKEY_PASTES_AFTER_APPLY: bool = false;

#[cfg(not(any(windows, target_os = "macos")))]
pub fn spawn_hotkey_listener(
    _sender: tokio::sync::mpsc::UnboundedSender<HotkeyAction>,
    _enable_copy: bool,
) -> anyhow::Result<()> {
    anyhow::bail!("global hotkeys MVP is currently implemented only on Windows and macOS")
}
