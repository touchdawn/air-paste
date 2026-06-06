use windows_sys::Win32::{
    Foundation::HWND,
    UI::{
        Input::KeyboardAndMouse::{
            RegisterHotKey, UnregisterHotKey, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
        },
        WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY},
    },
};

const HOTKEY_ID_REMOTE_PASTE: i32 = 1;
const VK_V: u32 = 0x56;

pub fn spawn_remote_paste_listener(
    sender: tokio::sync::mpsc::UnboundedSender<()>,
) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("airpaste-hotkey".to_string())
        .spawn(move || {
            if let Err(error) = run_hotkey_loop(sender) {
                tracing::warn!(%error, "remote paste hotkey listener stopped");
            }
        })?;
    Ok(())
}

fn run_hotkey_loop(sender: tokio::sync::mpsc::UnboundedSender<()>) -> anyhow::Result<()> {
    let registered = unsafe {
        RegisterHotKey(
            HWND::default(),
            HOTKEY_ID_REMOTE_PASTE,
            MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
            VK_V,
        ) != 0
    };
    if !registered {
        anyhow::bail!("RegisterHotKey(Ctrl+Shift+V) failed");
    }

    let _guard = HotkeyGuard;
    tracing::info!("registered remote paste hotkey Ctrl+Shift+V");

    loop {
        let mut message = MSG::default();
        let result = unsafe { GetMessageW(&mut message, HWND::default(), 0, 0) };
        if result <= 0 {
            break;
        }
        if message.message == WM_HOTKEY && message.wParam == HOTKEY_ID_REMOTE_PASTE as usize {
            let _ = sender.send(());
        }
    }

    Ok(())
}

struct HotkeyGuard;

impl Drop for HotkeyGuard {
    fn drop(&mut self) {
        unsafe {
            UnregisterHotKey(HWND::default(), HOTKEY_ID_REMOTE_PASTE);
        }
    }
}
