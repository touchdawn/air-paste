#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::spawn_remote_paste_listener;

#[cfg(not(windows))]
pub fn spawn_remote_paste_listener(
    _sender: tokio::sync::mpsc::UnboundedSender<()>,
) -> anyhow::Result<()> {
    anyhow::bail!("remote paste hotkey MVP is currently implemented only on Windows")
}
