//! 发送 tab: a text draft, an explicit file drop zone, and the send statuses. Actual file
//! drops are handled window-wide in `app.rs` (egui reports drops per-window, not per-widget);
//! the zone here is the visual affordance.

use airpaste_agent::SendStatus;
use eframe::egui;

use crate::app::TrayApp;
use crate::theme;
use crate::ui::{hint, primary_button, status_line, Tab};

pub fn show(app: &mut TrayApp, ui: &mut egui::Ui) {
    ui.add(
        egui::TextEdit::multiline(&mut app.send_input)
            .desired_rows(4)
            .desired_width(f32::INFINITY)
            .hint_text("输入或粘贴要发送的文字…"),
    );

    pasted_image_panel(app, ui);

    drop_zone(ui);

    ui.horizontal(|ui| {
        let mod_name = airpaste_agent::HOTKEY_MOD_NAME;
        hint(ui, &format!("{mod_name}+C 发送剪贴板 · {mod_name}+V 接收"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let can_send = app.agent.connected() && !app.send_input.trim().is_empty();
            if primary_button(ui, can_send, "发送").clicked() {
                app.agent.send_text(app.send_input.clone());
                app.send_clear_pending = true;
            }
        });
    });

    if !app.agent.connected() {
        ui.horizontal(|ui| {
            hint(ui, "未连接 — 需先配置并连接服务器");
            if ui.small_button("去设置").clicked() {
                app.tab = Tab::Settings;
            }
        });
    }

    match app.agent.send_text_status() {
        Some(SendStatus::Sending) => {
            hint(ui, "发送中…");
        }
        Some(SendStatus::Sent) => {
            if app.send_clear_pending {
                app.send_clear_pending = false;
                app.send_input.clear();
            }
            status_line(ui, theme::SUCCESS, "✓ 已发送");
        }
        Some(SendStatus::Failed(error)) => {
            app.send_clear_pending = false;
            status_line(
                ui,
                theme::DANGER,
                &format!("✗ 发送失败:{}", crate::ui::preview(&error)),
            );
        }
        None => {}
    }

    if let Some(error) = app.pasted_image_error.lock().unwrap().clone() {
        status_line(
            ui,
            theme::DANGER,
            &format!("✗ 图片发送失败:{}", crate::ui::preview(&error)),
        );
    }

    match app.agent.send_files_status() {
        Some(SendStatus::Sending) => {
            hint(ui, "文件处理中…(大文件需要先计算校验和)");
        }
        Some(SendStatus::Sent) => status_line(
            ui,
            theme::SUCCESS,
            &format!(
                "✓ 文件已发出(对方按 {}+V 接收)",
                airpaste_agent::HOTKEY_MOD_NAME
            ),
        ),
        Some(SendStatus::Failed(error)) => status_line(
            ui,
            theme::DANGER,
            &format!("✗ 文件发送失败:{}", crate::ui::preview(&error)),
        ),
        None => {}
    }
}

/// The staged clipboard image (Cmd+V on this tab): a thumbnail, its dimensions, and an
/// explicit send — pastes are easy to trigger by accident, so nothing ships until confirmed.
fn pasted_image_panel(app: &mut TrayApp, ui: &mut egui::Ui) {
    let mut send_clicked = false;
    let mut cancel_clicked = false;

    if let Some(pending) = &app.pasted_image {
        ui.add_space(6.0);
        ui.add(
            egui::Image::new(&pending.texture)
                .max_size(egui::vec2(ui.available_width(), 140.0))
                .rounding(egui::Rounding::same(6.0)),
        );
        ui.horizontal(|ui| {
            hint(
                ui,
                &format!("剪贴板图片 · {}×{}", pending.width, pending.height),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                send_clicked = primary_button(ui, app.agent.connected(), "发送图片").clicked();
                cancel_clicked = ui.small_button("取消").clicked();
            });
        });
        ui.add_space(2.0);
    }

    if cancel_clicked {
        app.pasted_image = None;
    }
    if send_clicked {
        send_pasted_image(app);
    }
}

/// PNG-encode the staged bitmap and hand it to the file pipeline. Encoding a retina-sized
/// screenshot is slow enough to stutter the UI, so it runs off-thread; progress then surfaces
/// through the regular `send_files_status`, and an encode failure lands in
/// `pasted_image_error`.
fn send_pasted_image(app: &mut TrayApp) {
    let Some(pending) = app.pasted_image.take() else {
        return;
    };
    *app.pasted_image_error.lock().unwrap() = None;
    let agent = app.agent.clone();
    let errors = app.pasted_image_error.clone();
    let (width, height) = (pending.width as u32, pending.height as u32);
    std::thread::spawn(move || {
        match airpaste_agent::stage_pasted_image_png(&pending.rgba, width, height) {
            Ok(path) => agent.send_files(vec![path]),
            Err(error) => *errors.lock().unwrap() = Some(format!("{error:#}")),
        }
    });
}

/// The dashed drop target. Purely visual — drops anywhere on the window are accepted.
fn drop_zone(ui: &mut egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 76.0), egui::Sense::hover());
    let fill = ui.visuals().faint_bg_color;
    let stroke = egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color);
    let text_color = ui.visuals().weak_text_color();
    let painter = ui.painter();
    painter.rect_filled(rect, egui::Rounding::same(8.0), fill);
    let r = rect.shrink(0.5);
    for (a, b) in [
        (r.left_top(), r.right_top()),
        (r.right_top(), r.right_bottom()),
        (r.right_bottom(), r.left_bottom()),
        (r.left_bottom(), r.left_top()),
    ] {
        painter.extend(egui::Shape::dashed_line(&[a, b], stroke, 5.0, 4.0));
    }
    // The paste affordance only exists where the native chord monitor is implemented.
    #[cfg(target_os = "macos")]
    const DROP_HINT: &str = "拖入文件/文件夹发送 · ⌘V 粘贴图片或文件";
    #[cfg(not(target_os = "macos"))]
    const DROP_HINT: &str = "把文件或文件夹拖到这里发送";
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        DROP_HINT,
        egui::FontId::proportional(13.0),
        text_color,
    );
}
