//! Per-OS bits of the tray UI: where to find a CJK font, and how to make the window behave as
//! a tray-only app (macOS menu-bar accessory with no Dock icon; Windows hidden from the
//! taskbar). Everything else in the UI is shared (`crate::app`).

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
