use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL, VK_MENU,
};

pub struct PasteSimulator;

impl PasteSimulator {
    pub fn new() -> Self {
        Self
    }

    pub fn accessibility_trusted(&self) -> bool {
        true
    }

    pub fn paste(&self) -> anyhow::Result<()> {
        self.send_ctrl_chord(VK_V)
    }

    /// Release the modifiers the user is holding from the triggering hotkey, then send a
    /// clean Ctrl+<key> chord so the synthesized keystroke is not mixed with the held Alt.
    fn send_ctrl_chord(&self, vk: u16) -> anyhow::Result<()> {
        let inputs = [
            // Release the Alt held from the Alt+C / Alt+V trigger so it doesn't combine into the
            // synthesized Ctrl+<key> (which would read as Ctrl+Alt+<key>, not a copy/paste).
            key_input(VK_MENU, KEYEVENTF_KEYUP),
            key_input(VK_CONTROL, KEYEVENTF_KEYUP),
            key_input(VK_CONTROL, 0),
            key_input(vk, 0),
            key_input(vk, KEYEVENTF_KEYUP),
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
