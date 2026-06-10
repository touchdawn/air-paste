//! Per-OS bits of the tray UI: where to find a CJK font, how to make the window behave as
//! a tray-only app (macOS menu-bar accessory with no Dock icon; Windows hidden from the
//! taskbar), and the native paste-chord monitor. Everything else in the UI is shared
//! (`crate::app`).

use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the native key monitor when the user presses the paste chord (Cmd+V); drained once
/// per frame by `TrayApp::update`.
static PASTE_REQUESTED: AtomicBool = AtomicBool::new(false);

/// True exactly once per observed paste chord. egui cannot report this itself: egui-winit
/// swallows the V key event for paste chords and only synthesizes `Event::Paste` when the
/// clipboard holds text, so image-only and file pastes are otherwise invisible.
pub fn take_paste_request() -> bool {
    PASTE_REQUESTED.swap(false, Ordering::Relaxed)
}

/// Watch for Cmd+V via a local `NSEvent` monitor (events delivered to this app only). The
/// event is passed through untouched, so egui's own text-paste path keeps working. Must be
/// called on the main thread, after the event loop exists.
#[cfg(target_os = "macos")]
pub fn install_paste_monitor() {
    use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};
    use std::ptr::NonNull;

    let block = block2::RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        let key_event = unsafe { event.as_ref() };
        let flags = key_event.modifierFlags();
        let plain_command = flags.contains(NSEventModifierFlags::Command)
            && !flags.contains(NSEventModifierFlags::Option)
            && !flags.contains(NSEventModifierFlags::Control);
        if plain_command && !key_event.isARepeat() {
            let is_v = key_event
                .charactersIgnoringModifiers()
                .is_some_and(|chars| chars.to_string().eq_ignore_ascii_case("v"));
            if is_v {
                PASTE_REQUESTED.store(true, Ordering::Relaxed);
            }
        }
        event.as_ptr()
    });
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block)
    };
    // The monitor lives for the rest of the process; dropping the token would uninstall it.
    std::mem::forget(monitor);
}

/// Windows: not implemented yet — clipboard image reading (CF_DIB) is also stubbed out, so
/// there is nothing for the chord to trigger. Left for the Windows-side work.
#[cfg(not(target_os = "macos"))]
pub fn install_paste_monitor() {}

/// CJK-capable fonts to try, in order, for the current OS. egui's bundled fonts have no CJK
/// glyphs, so we load one of these as the primary proportional font.
pub const CJK_FONT_CANDIDATES: &[&str] = CANDIDATES;

#[cfg(target_os = "macos")]
const CANDIDATES: &[&str] = &[
    // Arial Unicode is a single-face .ttf (loads cleanly); the .ttc collections are fallbacks
    // (egui loads face 0).
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/STHeiti Light.ttc",
];

#[cfg(target_os = "windows")]
const CANDIDATES: &[&str] = &[
    // Microsoft YaHei (微软雅黑) ships on every modern Windows; the rest are fallbacks.
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\msyhl.ttc",
    r"C:\Windows\Fonts\simhei.ttf",
    r"C:\Windows\Fonts\simsun.ttc",
];

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const CANDIDATES: &[&str] = &[];

/// Apply this platform's window policy.
///
/// - macOS: run as an `Accessory` activation-policy app — no Dock icon, no app menu bar — via
///   eframe's event-loop-builder hook (winit's macOS extension). The menu-bar icon is the
///   persistent presence; that's the native menu-bar-app convention.
/// - Windows: keep the *normal taskbar button*. The previous tray-only variant hid the window
///   from the taskbar (`with_taskbar(false)`), which made it easy to lose behind other windows
///   with no taskbar / Alt-Tab entry to bring it back. The tray icon still exists as an extra.
pub fn apply_tray_window_policy(options: &mut eframe::NativeOptions) {
    #[cfg(target_os = "macos")]
    {
        options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            builder.with_activation_policy(ActivationPolicy::Accessory);
        }));
    }
    // Windows and other platforms: no special policy — the window keeps its normal taskbar button
    // so it is always findable via the taskbar and Alt-Tab.
    let _ = options;
}
