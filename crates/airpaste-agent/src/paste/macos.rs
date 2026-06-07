//! macOS synthetic copy/paste via CoreGraphics events.
//!
//! Posting key events to other applications requires the process to be trusted for
//! Accessibility (System Settings -> Privacy & Security -> Accessibility). `accessibility_trusted`
//! lets callers detect and surface that before the first synthetic keystroke silently no-ops.

use core_foundation_sys::base::CFRelease;
use std::{ffi::c_void, ptr::null_mut};

type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;
type CGEventTapLocation = u32;
type CGKeyCode = u16;
type CGEventFlags = u64;

/// kCGHIDEventTap — post as though from the HID system, so the frontmost app receives it.
const KCG_HID_EVENT_TAP: CGEventTapLocation = 0;
/// kCGEventFlagMaskCommand — the Command modifier.
const KCG_EVENT_FLAG_MASK_COMMAND: CGEventFlags = 0x0010_0000;
/// ANSI virtual key codes.
const KEY_C: CGKeyCode = 8;
const KEY_V: CGKeyCode = 9;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        keycode: CGKeyCode,
        keydown: bool,
    ) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: CGEventFlags);
    fn CGEventPost(tap: CGEventTapLocation, event: CGEventRef);
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

pub struct PasteSimulator;

impl PasteSimulator {
    pub fn new() -> Self {
        Self
    }

    /// Whether this process may post synthetic key events to other apps.
    pub fn accessibility_trusted(&self) -> bool {
        unsafe { AXIsProcessTrusted() != 0 }
    }

    /// Synthesize Command+V (paste into the focused app).
    pub fn paste(&self) -> anyhow::Result<()> {
        post_command_chord(KEY_V)
    }

    /// Synthesize Command+C (copy the current selection in the focused app).
    pub fn copy(&self) -> anyhow::Result<()> {
        post_command_chord(KEY_C)
    }
}

/// Post a Command+<key> chord. The Command flag is set explicitly on each event so any
/// modifier keys the user is physically holding (e.g. the Ctrl+Shift of the triggering
/// hotkey) do not leak into the synthesized keystroke.
fn post_command_chord(keycode: CGKeyCode) -> anyhow::Result<()> {
    unsafe {
        let down = CGEventCreateKeyboardEvent(null_mut(), keycode, true);
        if down.is_null() {
            anyhow::bail!("CGEventCreateKeyboardEvent(keydown) returned null");
        }
        CGEventSetFlags(down, KCG_EVENT_FLAG_MASK_COMMAND);
        CGEventPost(KCG_HID_EVENT_TAP, down);
        CFRelease(down as *const c_void);

        let up = CGEventCreateKeyboardEvent(null_mut(), keycode, false);
        if up.is_null() {
            anyhow::bail!("CGEventCreateKeyboardEvent(keyup) returned null");
        }
        CGEventSetFlags(up, KCG_EVENT_FLAG_MASK_COMMAND);
        CGEventPost(KCG_HID_EVENT_TAP, up);
        CFRelease(up as *const c_void);
    }
    Ok(())
}
