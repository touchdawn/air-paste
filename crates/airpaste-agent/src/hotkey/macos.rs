use anyhow::Context;
use core_foundation_sys::base::{OSStatus, UInt32};
use std::{ffi::c_void, ptr::null_mut, sync::mpsc, time::Duration};

const HOTKEY_ID_REMOTE_PASTE: u32 = 1;
const HOTKEY_SIGNATURE: u32 = u32::from_be_bytes(*b"Apst");
const KEY_CODE_V: u32 = 9;
const SHIFT_KEY: u32 = 1 << 9;
const CONTROL_KEY: u32 = 1 << 12;
const NO_ERR: OSStatus = 0;
const EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const EVENT_KIND_HOTKEY_PRESSED: u32 = 5;
const REMOTE_PASTE_HOTKEY_LABEL: &str = "Ctrl+Shift+V";

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
    fn RunApplicationEventLoop();
}

pub fn spawn_remote_paste_listener(
    sender: tokio::sync::mpsc::UnboundedSender<()>,
) -> anyhow::Result<()> {
    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("airpaste-hotkey".to_string())
        .spawn(move || {
            if let Err(error) = run_hotkey_loop(sender, ready_tx) {
                tracing::warn!(%error, "remote paste hotkey listener stopped");
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
    sender: tokio::sync::mpsc::UnboundedSender<()>,
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

    let mut hotkey_ref = null_mut();
    let status = unsafe {
        RegisterEventHotKey(
            KEY_CODE_V,
            CONTROL_KEY | SHIFT_KEY,
            EventHotKeyID {
                signature: HOTKEY_SIGNATURE,
                id: HOTKEY_ID_REMOTE_PASTE,
            },
            target,
            0,
            &mut hotkey_ref,
        )
    };
    if status != NO_ERR {
        unsafe {
            RemoveEventHandler(handler_ref);
            drop(Box::from_raw(sender));
        }
        return fail_ready(
            ready,
            format!("RegisterEventHotKey({REMOTE_PASTE_HOTKEY_LABEL}) failed with status {status}"),
        );
    }

    let _guard = HotkeyGuard {
        hotkey_ref,
        handler_ref,
        sender,
    };
    tracing::info!("registered remote paste hotkey {REMOTE_PASTE_HOTKEY_LABEL}");
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
    _event: EventRef,
    user_data: *mut c_void,
) -> OSStatus {
    if !user_data.is_null() {
        let sender = &*(user_data as *const tokio::sync::mpsc::UnboundedSender<()>);
        tracing::info!("received remote paste hotkey {REMOTE_PASTE_HOTKEY_LABEL}");
        let _ = sender.send(());
    }
    NO_ERR
}

struct HotkeyGuard {
    hotkey_ref: EventHotKeyRef,
    handler_ref: EventHandlerRef,
    sender: *mut tokio::sync::mpsc::UnboundedSender<()>,
}

impl Drop for HotkeyGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.hotkey_ref.is_null() {
                UnregisterEventHotKey(self.hotkey_ref);
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
