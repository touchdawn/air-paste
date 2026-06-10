//! macOS synthetic paste via CoreGraphics events.
//!
//! Posting key events to other applications requires the process to be trusted for
//! Accessibility (System Settings -> Privacy & Security -> Accessibility). `accessibility_trusted`
//! lets callers detect and surface that before the first synthetic keystroke silently no-ops.
//!
//! The paste is triggered from the Option+V hotkey, so the user is still physically holding
//! Option when we synthesize Cmd+V. Setting the event flags to Command-only is not enough:
//! events posted at `kCGHIDEventTap` are merged with the live hardware modifier state, so the
//! frontmost app would receive Cmd+Option+V — an unbound key equivalent in most apps, which
//! beeps and pastes nothing. Two countermeasures (the same recipe clipboard managers like
//! Maccy ship):
//!   1. wait (bounded) for the physical modifiers to be released, and
//!   2. post via a combined-session-state `CGEventSource` to `kCGAnnotatedSessionEventTap`,
//!      filtering local keyboard events during the post's suppression interval.

use core_foundation_sys::{
    base::CFRelease,
    dictionary::{
        kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks, CFDictionaryCreate,
        CFDictionaryRef,
    },
    number::kCFBooleanTrue,
    string::CFStringRef,
};
use std::{
    ffi::c_void,
    ptr::null,
    time::{Duration, Instant},
};

type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;
type CGEventTapLocation = u32;
type CGEventSourceStateID = i32;
type CGKeyCode = u16;
type CGEventFlags = u64;
type CGEventFilterMask = u32;
type CGEventSuppressionState = u32;

/// kCGAnnotatedSessionEventTap — post into the login session's event stream, past the HID
/// level where physically-held modifiers would be merged into the event.
const KCG_ANNOTATED_SESSION_EVENT_TAP: CGEventTapLocation = 2;
/// kCGEventSourceStateCombinedSessionState.
const KCG_EVENT_SOURCE_STATE_COMBINED_SESSION: CGEventSourceStateID = 0;
/// kCGEventFlagMaskCommand — the Command modifier.
const KCG_EVENT_FLAG_MASK_COMMAND: CGEventFlags = 0x0010_0000;
/// Shift | Control | Option | Command — modifiers the user may still hold from the hotkey chord.
const MODIFIER_FLAGS: CGEventFlags = 0x0002_0000 | 0x0004_0000 | 0x0008_0000 | 0x0010_0000;
/// kCGEventFilterMaskPermitLocalMouseEvents | kCGEventFilterMaskPermitSystemDefinedEvents:
/// everything except local keyboard events, so the held Option cannot interleave with the chord.
const FILTER_PERMIT_MOUSE_AND_SYSTEM: CGEventFilterMask = 0x1 | 0x4;
/// kCGEventSuppressionStateSuppressionInterval.
const KCG_EVENT_SUPPRESSION_STATE_SUPPRESSION_INTERVAL: CGEventSuppressionState = 0;
/// ANSI virtual key code for V.
const KEY_V: CGKeyCode = 9;

/// How long to wait for the hotkey's physical modifiers to be released before pasting anyway.
const MODIFIER_RELEASE_TIMEOUT: Duration = Duration::from_secs(1);
const MODIFIER_RELEASE_POLL: Duration = Duration::from_millis(10);

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        keycode: CGKeyCode,
        keydown: bool,
    ) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: CGEventFlags);
    fn CGEventPost(tap: CGEventTapLocation, event: CGEventRef);
    fn CGEventSourceCreate(state_id: CGEventSourceStateID) -> CGEventSourceRef;
    fn CGEventSourceFlagsState(state_id: CGEventSourceStateID) -> CGEventFlags;
    fn CGEventSourceSetLocalEventsFilterDuringSuppressionState(
        source: CGEventSourceRef,
        filter: CGEventFilterMask,
        state: CGEventSuppressionState,
    );
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
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

    /// Like `accessibility_trusted`, but when untrusted asks macOS to show the system
    /// dialog that registers this app in the Accessibility list and offers to open
    /// System Settings. The grant is keyed to the binary's code signature, so it must be
    /// re-confirmed after every rebuild of an ad-hoc-signed bundle.
    pub fn request_accessibility(&self) -> bool {
        unsafe {
            let keys = [kAXTrustedCheckOptionPrompt as *const c_void];
            let values = [kCFBooleanTrue as *const c_void];
            let options = CFDictionaryCreate(
                null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            let trusted = AXIsProcessTrustedWithOptions(options) != 0;
            if !options.is_null() {
                CFRelease(options as *const c_void);
            }
            trusted
        }
    }

    /// Synthesize Command+V (paste into the focused app).
    pub fn paste(&self) -> anyhow::Result<()> {
        if !self.accessibility_trusted() {
            anyhow::bail!(
                "AirPaste lacks Accessibility permission, so macOS silently drops synthetic \
                 keystrokes. Grant (or re-toggle after an app update) System Settings -> \
                 Privacy & Security -> Accessibility -> AirPaste, then restart the app"
            );
        }
        wait_for_modifier_release();
        post_command_chord(KEY_V)?;
        tracing::debug!("posted synthetic Cmd+V");
        Ok(())
    }
}

/// Block (bounded) until the user releases the physical modifier keys held from the
/// triggering hotkey. Best-effort: on timeout the paste proceeds anyway.
fn wait_for_modifier_release() {
    let start = Instant::now();
    let initial = unsafe { CGEventSourceFlagsState(KCG_EVENT_SOURCE_STATE_COMBINED_SESSION) }
        & MODIFIER_FLAGS;
    let deadline = start + MODIFIER_RELEASE_TIMEOUT;
    let mut timed_out = false;
    while unsafe { CGEventSourceFlagsState(KCG_EVENT_SOURCE_STATE_COMBINED_SESSION) }
        & MODIFIER_FLAGS
        != 0
    {
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        std::thread::sleep(MODIFIER_RELEASE_POLL);
    }
    tracing::debug!(
        initial_flags = format_args!("{initial:#x}"),
        waited_ms = start.elapsed().as_millis() as u64,
        timed_out,
        "modifier release wait finished"
    );
}

/// Post a Command+<key> chord. The Command flag is set explicitly on each event so the
/// chord reads as exactly Cmd+<key> regardless of lingering modifier state.
fn post_command_chord(keycode: CGKeyCode) -> anyhow::Result<()> {
    unsafe {
        let source = CGEventSourceCreate(KCG_EVENT_SOURCE_STATE_COMBINED_SESSION);
        if !source.is_null() {
            CGEventSourceSetLocalEventsFilterDuringSuppressionState(
                source,
                FILTER_PERMIT_MOUSE_AND_SYSTEM,
                KCG_EVENT_SUPPRESSION_STATE_SUPPRESSION_INTERVAL,
            );
        }
        let result = post_chord_with_source(source, keycode);
        if !source.is_null() {
            CFRelease(source as *const c_void);
        }
        result
    }
}

unsafe fn post_chord_with_source(
    source: CGEventSourceRef,
    keycode: CGKeyCode,
) -> anyhow::Result<()> {
    let down = CGEventCreateKeyboardEvent(source, keycode, true);
    if down.is_null() {
        anyhow::bail!("CGEventCreateKeyboardEvent(keydown) returned null");
    }
    CGEventSetFlags(down, KCG_EVENT_FLAG_MASK_COMMAND);
    CGEventPost(KCG_ANNOTATED_SESSION_EVENT_TAP, down);
    CFRelease(down as *const c_void);

    let up = CGEventCreateKeyboardEvent(source, keycode, false);
    if up.is_null() {
        anyhow::bail!("CGEventCreateKeyboardEvent(keyup) returned null");
    }
    CGEventSetFlags(up, KCG_EVENT_FLAG_MASK_COMMAND);
    CGEventPost(KCG_ANNOTATED_SESSION_EVENT_TAP, up);
    CFRelease(up as *const c_void);
    Ok(())
}
