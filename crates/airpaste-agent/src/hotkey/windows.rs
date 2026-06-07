use crate::hotkey::HotkeyAction;
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
const HOTKEY_ID_COPY: i32 = 2;
const VK_V: u32 = 0x56;
const VK_C: u32 = 0x43;

pub fn spawn_hotkey_listener(
    sender: tokio::sync::mpsc::UnboundedSender<HotkeyAction>,
    enable_copy: bool,
) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("airpaste-hotkey".to_string())
        .spawn(move || {
            if let Err(error) = run_hotkey_loop(sender, enable_copy) {
                tracing::warn!(%error, "hotkey listener stopped");
            }
        })?;
    Ok(())
}

fn run_hotkey_loop(
    sender: tokio::sync::mpsc::UnboundedSender<HotkeyAction>,
    enable_copy: bool,
) -> anyhow::Result<()> {
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
    let _paste_guard = HotkeyGuard(HOTKEY_ID_REMOTE_PASTE);

    let _copy_guard = if enable_copy {
        let registered = unsafe {
            RegisterHotKey(
                HWND::default(),
                HOTKEY_ID_COPY,
                MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
                VK_C,
            ) != 0
        };
        if !registered {
            anyhow::bail!("RegisterHotKey(Ctrl+Shift+C) failed");
        }
        Some(HotkeyGuard(HOTKEY_ID_COPY))
    } else {
        None
    };

    if enable_copy {
        tracing::info!("registered hotkeys Ctrl+Shift+V and Ctrl+Shift+C");
    } else {
        tracing::info!("registered remote paste hotkey Ctrl+Shift+V");
    }

    loop {
        let mut message = MSG::default();
        let result = unsafe { GetMessageW(&mut message, HWND::default(), 0, 0) };
        if result <= 0 {
            break;
        }
        if message.message == WM_HOTKEY {
            let action = match message.wParam as i32 {
                HOTKEY_ID_REMOTE_PASTE => Some(HotkeyAction::PasteRemote),
                HOTKEY_ID_COPY => Some(HotkeyAction::CopyToAirPaste),
                _ => None,
            };
            if let Some(action) = action {
                let _ = sender.send(action);
            }
        }
    }

    Ok(())
}

struct HotkeyGuard(i32);

impl Drop for HotkeyGuard {
    fn drop(&mut self) {
        unsafe {
            UnregisterHotKey(HWND::default(), self.0);
        }
    }
}
