//! Toast notifications (bottom right): success fades after 4 s, errors after 12 s.

use super::app::App;
use crate::theme::{self, icon};
use egui::{Align2, Frame, Id, Margin, RichText};

pub(super) struct Toast {
    text: String,
    error: bool,
    until: f64,
}

impl App {
    pub(super) fn toast(&mut self, ctx: &egui::Context, text: String, error: bool) {
        let now = ctx.input(|i| i.time);
        self.toasts.push(Toast { text, error, until: now + if error { 12.0 } else { 4.0 } });
    }

    pub(super) fn show_toasts(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        self.toasts.retain(|t| t.until > now);
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        let mut dismiss = None;
        egui::Area::new(Id::new("toasts")).anchor(Align2::RIGHT_BOTTOM, egui::vec2(-18.0, -18.0)).show(ctx, |ui| {
            for (i, t) in self.toasts.iter().enumerate() {
                let color = if t.error { theme::DEL } else { theme::ADD };
                let slot = ui.painter().add(egui::Shape::Noop);
                let r = Frame::new()
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_max_width(420.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(if t.error { icon::WARNING_CIRCLE } else { icon::CHECK_CIRCLE }).color(color).size(16.0));
                            ui.label(&t.text);
                        });
                    })
                    .response;
                let glass = theme::glass_shapes(r.rect, 14.0, ui.visuals().dark_mode, 235);
                ui.painter().set(slot, egui::Shape::Vec(glass));
                if ui.interact(r.rect, Id::new(("toast", i)), egui::Sense::click()).clicked() {
                    dismiss = Some(i);
                }
                ui.add_space(6.0);
            }
        });
        if let Some(i) = dismiss {
            self.toasts.remove(i);
        }
    }
}
