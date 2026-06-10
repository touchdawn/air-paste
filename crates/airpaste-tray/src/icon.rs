//! The AirPaste app icon — a white paper-plane on a rounded blue tile — rendered in code so we
//! ship no image asset. `icon_rgba` is the single swap point if a designed PNG is added later
//! (decode it to RGBA via the workspace `image` crate). `paint_logo` draws the same artwork
//! with egui's painter for the in-window header.

use eframe::egui;
use tray_icon::Icon;

use crate::theme;

/// The tray-menu icon.
pub fn app_icon() -> Icon {
    let (rgba, size) = icon_rgba();
    Icon::from_rgba(rgba, size, size).expect("valid icon")
}

/// The window / taskbar icon (same artwork as the tray icon).
pub fn window_icon() -> egui::IconData {
    let (rgba, size) = icon_rgba();
    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}

/// Draw the logo tile at `size` px in the current ui (used by the window header).
pub fn paint_logo(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, egui::Rounding::same(size * 0.2), theme::ACCENT);
    let map = |x: f32, y: f32| rect.min + egui::vec2(x, y) * (size / 64.0);
    for (a, b, c) in [(TIP, WING_UPPER, FOLD), (TIP, FOLD, WING_LOWER)] {
        painter.add(egui::Shape::convex_polygon(
            vec![map(a.0, a.1), map(b.0, b.1), map(c.0, c.1)],
            egui::Color32::WHITE,
            egui::Stroke::NONE,
        ));
    }
}

// Paper-plane dart on a 64px grid: tip top-right, two wings, the fold pulled toward the tip so
// the tail reads as two wings.
const TIP: (f32, f32) = (50.0, 13.0);
const WING_UPPER: (f32, f32) = (13.0, 31.0);
const WING_LOWER: (f32, f32) = (33.0, 51.0);
const FOLD: (f32, f32) = (31.0, 34.0);

fn icon_rgba() -> (Vec<u8>, u32) {
    const S: u32 = 64;
    const WHITE: [u8; 4] = [0xff, 0xff, 0xff, 0xff];
    let accent = theme::ACCENT;
    let blue = [accent.r(), accent.g(), accent.b(), 0xff];
    let mut rgba = vec![0u8; (S * S * 4) as usize];
    for y in 0..S {
        for x in 0..S {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let px = if !in_rounded_rect(fx, fy, S as f32, 13.0) {
                [0, 0, 0, 0]
            } else if in_paper_plane(fx, fy) {
                WHITE
            } else {
                blue
            };
            let i = ((y * S + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&px);
        }
    }
    (rgba, S)
}

/// Point-in-rounded-square test for a `size`×`size` tile (2px transparent margin, corner `r`).
fn in_rounded_rect(x: f32, y: f32, size: f32, r: f32) -> bool {
    let (lo, hi) = (2.0, size - 2.0);
    if x < lo || x > hi || y < lo || y > hi {
        return false;
    }
    let cx = x.clamp(lo + r, hi - r);
    let cy = y.clamp(lo + r, hi - r);
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= r * r
}

fn in_paper_plane(x: f32, y: f32) -> bool {
    in_triangle(x, y, TIP, WING_UPPER, FOLD) || in_triangle(x, y, TIP, FOLD, WING_LOWER)
}

fn in_triangle(x: f32, y: f32, a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let edge = |p: (f32, f32), q: (f32, f32)| (x - q.0) * (p.1 - q.1) - (p.0 - q.0) * (y - q.1);
    let (d1, d2, d3) = (edge(a, b), edge(b, c), edge(c, a));
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}
