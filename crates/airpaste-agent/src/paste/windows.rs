use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL, VK_SHIFT,
};

pub struct PasteSimulator;

impl PasteSimulator {
    pub fn new() -> Self {
        Self
    }

    pub fn paste(&self) -> anyhow::Result<()> {
        let inputs = [
            key_input(VK_SHIFT, KEYEVENTF_KEYUP),
            key_input(VK_CONTROL, KEYEVENTF_KEYUP),
            key_input(VK_CONTROL, 0),
            key_input(VK_V, 0),
            key_input(VK_V, KEYEVENTF_KEYUP),
            key_input(VK_CONTROL, KEYEVENTF_KEYUP),
        ];
        let sent = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        if sent != inputs.len() as u32 {
            anyhow::bail!("SendInput sent {sent} of {} events", inputs.len());
        }
        Ok(())
    }
}

const VK_V: u16 = 0x56;

fn key_input(vk: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}
