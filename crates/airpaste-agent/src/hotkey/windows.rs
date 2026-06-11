use crate::hotkey::{HotkeyAction, HotkeyChords, HotkeyKey, HotkeySpec};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::{
        Input::KeyboardAndMouse::{
            RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
            MOD_WIN,
        },
        WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY},
    },
};

const HOTKEY_ID_REMOTE_PASTE: i32 = 1;
const HOTKEY_ID_COPY: i32 = 2;
const VK_F1: u32 = 0x70;

fn modifiers(spec: HotkeySpec) -> u32 {
    let mut mods = MOD_NOREPEAT;
    if spec.alt {
        mods |= MOD_ALT;
    }
    if spec.ctrl {
        mods |= MOD_CONTROL;
    }
    if spec.shift {
        mods |= MOD_SHIFT;
    }
    if spec.meta {
        mods |= MOD_WIN;
    }
    mods
}

fn vk_code(key: HotkeyKey) -> u32 {
    match key {
        // Letter/digit virtual-key codes equal their uppercase ASCII values.
        HotkeyKey::Char(c) => c as u32,
        HotkeyKey::F(n) => VK_F1 + (n as u32) - 1,
    }
}

pub fn spawn_hotkey_listener(
    sender: tokio::sync::mpsc::UnboundedSender<HotkeyAction>,
    enable_copy: bool,
    chords: HotkeyChords,
) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("airpaste-hotkey".to_string())
        .spawn(move || {
            if let Err(error) = run_hotkey_loop(sender, enable_copy, chords) {
                tracing::warn!(%error, "hotkey listener stopped");
            }
        })?;
    Ok(())
}

fn run_hotkey_loop(
    sender: tokio::sync::mpsc::UnboundedSender<HotkeyAction>,
    enable_copy: bool,
    chords: HotkeyChords,
) -> anyhow::Result<()> {
    let registered = unsafe {
        RegisterHotKey(
            HWND::default(),
            HOTKEY_ID_REMOTE_PASTE,
            modifiers(chords.paste),
            vk_code(chords.paste.key),
        ) != 0
    };
    if !registered {
        anyhow::bail!("RegisterHotKey({}) failed", chords.paste.label());
    }
    let _paste_guard = HotkeyGuard(HOTKEY_ID_REMOTE_PASTE);

    let _copy_guard = if enable_copy {
        let registered = unsafe {
            RegisterHotKey(
                HWND::default(),
                HOTKEY_ID_COPY,
                modifiers(chords.copy),
                vk_code(chords.copy.key),
            ) != 0
        };
        if !registered {
            anyhow::bail!("RegisterHotKey({}) failed", chords.copy.label());
        }
        Some(HotkeyGuard(HOTKEY_ID_COPY))
    } else {
        None
    };

    if enable_copy {
        tracing::info!(
            "registered hotkeys {} and {}",
            chords.paste.label(),
            chords.copy.label()
        );
    } else {
        tracing::info!("registered remote paste hotkey {}", chords.paste.label());
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
