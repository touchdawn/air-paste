use crate::hotkey::{HotkeyAction, HotkeyChords, HotkeyKey, HotkeySpec};
use anyhow::Context;
use core_foundation_sys::base::{OSStatus, UInt32};
use std::{ffi::c_void, mem::size_of, ptr::null_mut, sync::mpsc, time::Duration};

const HOTKEY_ID_REMOTE_PASTE: u32 = 1;
const HOTKEY_ID_COPY: u32 = 2;
const HOTKEY_SIGNATURE: u32 = u32::from_be_bytes(*b"Apst");
// Carbon modifier masks (Events.h): cmdKey, shiftKey, optionKey, controlKey.
const CMD_KEY: u32 = 1 << 8;
const SHIFT_KEY: u32 = 1 << 9;
const OPTION_KEY: u32 = 1 << 11;
const CONTROL_KEY: u32 = 1 << 12;
const NO_ERR: OSStatus = 0;
const EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const EVENT_KIND_HOTKEY_PRESSED: u32 = 5;
const EVENT_PARAM_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
const TYPE_EVENT_HOTKEY_ID: u32 = u32::from_be_bytes(*b"hkid");

fn carbon_modifiers(spec: HotkeySpec) -> u32 {
    let mut mods = 0;
    if spec.alt {
        mods |= OPTION_KEY;
    }
    if spec.ctrl {
        mods |= CONTROL_KEY;
    }
    if spec.shift {
        mods |= SHIFT_KEY;
    }
    if spec.meta {
        mods |= CMD_KEY;
    }
    mods
}

/// ANSI-layout virtual key codes (Carbon Events.h kVK_ANSI_*).
fn carbon_key_code(key: HotkeyKey) -> u32 {
    match key {
        HotkeyKey::Char(c) => match c {
            b'A' => 0,
            b'S' => 1,
            b'D' => 2,
            b'F' => 3,
            b'H' => 4,
            b'G' => 5,
            b'Z' => 6,
            b'X' => 7,
            b'C' => 8,
            b'V' => 9,
            b'B' => 11,
            b'Q' => 12,
            b'W' => 13,
            b'E' => 14,
            b'R' => 15,
            b'Y' => 16,
            b'T' => 17,
            b'1' => 18,
            b'2' => 19,
            b'3' => 20,
            b'4' => 21,
            b'6' => 22,
            b'5' => 23,
            b'9' => 25,
            b'7' => 26,
            b'8' => 28,
            b'0' => 29,
            b'O' => 31,
            b'U' => 32,
            b'I' => 34,
            b'P' => 35,
            b'L' => 37,
            b'J' => 38,
            b'K' => 40,
            b'N' => 45,
            b'M' => 46,
            // HotkeySpec::parse only emits ascii alphanumerics, so this is unreachable.
            other => unreachable!("unmapped hotkey character {other}"),
        },
        HotkeyKey::F(n) => match n {
            1 => 122,
            2 => 120,
            3 => 99,
            4 => 118,
            5 => 96,
            6 => 97,
            7 => 98,
            8 => 100,
            9 => 101,
            10 => 109,
            11 => 103,
            12 => 111,
            other => unreachable!("unmapped function key F{other}"),
        },
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EventTypeSpec {
    event_class: UInt32,
    event_kind: UInt32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyID {
    signature: UInt32,
    id: UInt32,
}

type EventHotKeyRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventRef = *mut c_void;
type EventTargetRef = *mut c_void;
type EventHandlerUPP = Option<
    unsafe extern "C" fn(
        next_handler: EventHandlerCallRef,
        event: EventRef,
        user_data: *mut c_void,
    ) -> OSStatus,
>;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerUPP,
        event_type_count: UInt32,
        event_types: *const EventTypeSpec,
        user_data: *mut c_void,
        handler_ref: *mut EventHandlerRef,
    ) -> OSStatus;
    fn RemoveEventHandler(handler_ref: EventHandlerRef) -> OSStatus;
    fn RegisterEventHotKey(
        key_code: UInt32,
        modifiers: UInt32,
        hotkey_id: EventHotKeyID,
        target: EventTargetRef,
        options: UInt32,
        hotkey_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn UnregisterEventHotKey(hotkey_ref: EventHotKeyRef) -> OSStatus;
    fn GetEventParameter(
        event: EventRef,
        name: UInt32,
        desired_type: UInt32,
        out_actual_type: *mut UInt32,
        buffer_size: usize,
        out_actual_size: *mut usize,
        out_data: *mut c_void,
    ) -> OSStatus;
    fn RunApplicationEventLoop();
}

pub fn spawn_hotkey_listener(
    sender: tokio::sync::mpsc::UnboundedSender<HotkeyAction>,
    enable_copy: bool,
    chords: HotkeyChords,
) -> anyhow::Result<()> {
    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("airpaste-hotkey".to_string())
        .spawn(move || {
            if let Err(error) = run_hotkey_loop(sender, enable_copy, chords, ready_tx) {
                tracing::warn!(%error, "hotkey listener stopped");
            }
        })
        .context("failed to spawn macOS hotkey listener")?;

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => anyhow::bail!(error),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            anyhow::bail!("macOS hotkey listener did not report readiness")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            anyhow::bail!("macOS hotkey listener stopped before reporting readiness")
        }
    }
}

fn run_hotkey_loop(
    sender: tokio::sync::mpsc::UnboundedSender<HotkeyAction>,
    enable_copy: bool,
    chords: HotkeyChords,
    ready: mpsc::Sender<Result<(), String>>,
) -> anyhow::Result<()> {
    let sender = Box::into_raw(Box::new(sender));
    let target = unsafe { GetApplicationEventTarget() };
    if target.is_null() {
        unsafe {
            drop(Box::from_raw(sender));
        }
        return fail_ready(ready, "GetApplicationEventTarget returned null");
    }

    let event_type = EventTypeSpec {
        event_class: EVENT_CLASS_KEYBOARD,
        event_kind: EVENT_KIND_HOTKEY_PRESSED,
    };
    let mut handler_ref = null_mut();
    let status = unsafe {
        InstallEventHandler(
            target,
            Some(handle_hotkey_event),
            1,
            &event_type,
            sender.cast(),
            &mut handler_ref,
        )
    };
    if status != NO_ERR {
        unsafe {
            drop(Box::from_raw(sender));
        }
        return fail_ready(
            ready,
            format!("InstallEventHandler failed with status {status}"),
        );
    }

    let mut paste_ref = null_mut();
    let status = unsafe {
        RegisterEventHotKey(
            carbon_key_code(chords.paste.key),
            carbon_modifiers(chords.paste),
            EventHotKeyID {
                signature: HOTKEY_SIGNATURE,
                id: HOTKEY_ID_REMOTE_PASTE,
            },
            target,
            0,
            &mut paste_ref,
        )
    };
    if status != NO_ERR {
        unsafe {
            RemoveEventHandler(handler_ref);
            drop(Box::from_raw(sender));
        }
        return fail_ready(
            ready,
            format!(
                "RegisterEventHotKey({}) failed with status {status}",
                chords.paste.label()
            ),
        );
    }

    let mut copy_ref = null_mut();
    if enable_copy {
        let status = unsafe {
            RegisterEventHotKey(
                carbon_key_code(chords.copy.key),
                carbon_modifiers(chords.copy),
                EventHotKeyID {
                    signature: HOTKEY_SIGNATURE,
                    id: HOTKEY_ID_COPY,
                },
                target,
                0,
                &mut copy_ref,
            )
        };
        if status != NO_ERR {
            unsafe {
                UnregisterEventHotKey(paste_ref);
                RemoveEventHandler(handler_ref);
                drop(Box::from_raw(sender));
            }
            return fail_ready(
                ready,
                format!(
                    "RegisterEventHotKey({}) failed with status {status}",
                    chords.copy.label()
                ),
            );
        }
    }

    let _guard = HotkeyGuard {
        paste_ref,
        copy_ref,
        handler_ref,
        sender,
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
    let _ = ready.send(Ok(()));

    unsafe {
        RunApplicationEventLoop();
    }

    Ok(())
}

fn fail_ready<T>(
    ready: mpsc::Sender<Result<(), String>>,
    message: impl Into<String>,
) -> anyhow::Result<T> {
    let message = message.into();
    let _ = ready.send(Err(message.clone()));
    anyhow::bail!(message)
}

unsafe extern "C" fn handle_hotkey_event(
    _next_handler: EventHandlerCallRef,
    event: EventRef,
    user_data: *mut c_void,
) -> OSStatus {
    if user_data.is_null() {
        return NO_ERR;
    }
    let sender = &*(user_data as *const tokio::sync::mpsc::UnboundedSender<HotkeyAction>);

    let mut hotkey_id = EventHotKeyID {
        signature: 0,
        id: 0,
    };
    let mut actual_type: UInt32 = 0;
    let mut actual_size: usize = 0;
    let status = GetEventParameter(
        event,
        EVENT_PARAM_DIRECT_OBJECT,
        TYPE_EVENT_HOTKEY_ID,
        &mut actual_type,
        size_of::<EventHotKeyID>(),
        &mut actual_size,
        (&mut hotkey_id as *mut EventHotKeyID).cast(),
    );
    if status != NO_ERR {
        return NO_ERR;
    }

    let (action, label) = match hotkey_id.id {
        HOTKEY_ID_REMOTE_PASTE => (HotkeyAction::PasteRemote, "remote-paste"),
        HOTKEY_ID_COPY => (HotkeyAction::CopyToAirPaste, "copy-to-airpaste"),
        _ => return NO_ERR,
    };
    tracing::info!("received {label} hotkey");
    let _ = sender.send(action);
    NO_ERR
}

struct HotkeyGuard {
    paste_ref: EventHotKeyRef,
    copy_ref: EventHotKeyRef,
    handler_ref: EventHandlerRef,
    sender: *mut tokio::sync::mpsc::UnboundedSender<HotkeyAction>,
}

impl Drop for HotkeyGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.paste_ref.is_null() {
                UnregisterEventHotKey(self.paste_ref);
            }
            if !self.copy_ref.is_null() {
                UnregisterEventHotKey(self.copy_ref);
            }
            if !self.handler_ref.is_null() {
                RemoveEventHandler(self.handler_ref);
            }
            if !self.sender.is_null() {
                drop(Box::from_raw(self.sender));
            }
        }
    }
}
