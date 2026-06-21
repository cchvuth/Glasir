//! Toast notifications (bottom right): success fades after 4 s (8 s with an Undo
//! button), errors after 12 s.

use super::app::App;
use crate::theme::{self, icon};
use egui::{Align2, Frame, Id, Margin, RichText, UiBuilder};
use std::path::PathBuf;

pub(super) struct Toast {
    text: String,
    error: bool,
    until: f64,
    /// (tab dir, undo step id): an Undo button while that step is still the newest.
    undo: Option<(PathBuf, u64)>,
}

impl App {
    pub(super) fn toast(&mut self, ctx: &egui::Context, text: String, error: bool) {
        self.toast_undo(ctx, text, error, None);
    }

    pub(super) fn toast_undo(&mut self, ctx: &egui::Context, text: String, error: bool, undo: Option<(PathBuf, u64)>) {
        let now = ctx.input(|i| i.time);
        let secs = if error { 12.0 } else if undo.is_some() { 8.0 } else { 4.0 };
        self.toasts.push(Toast { text, error, until: now + secs, undo });
    }

    pub(super) fn show_toasts(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        self.toasts.retain(|t| t.until > now);
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        let mut dismiss = None;
        let mut undo = None;
        let tabs = &self.tabs;
        let live = |u: &Option<(PathBuf, u64)>| {
            let (dir, id) = u.as_ref()?;
            let i = tabs.iter().position(|t| t.dir == *dir)?;
            (tabs[i].undo_top() == Some(*id) && tabs[i].busy().is_none()).then_some(i)
        };
        egui::Area::new(Id::new("toasts")).anchor(Align2::RIGHT_BOTTOM, egui::vec2(-18.0, -18.0)).show(ctx, |ui| {
            for (i, t) in self.toasts.iter().enumerate() {
                let color = if t.error { theme::DEL } else { theme::ADD };
                let slot = ui.painter().add(egui::Shape::Noop);
                // The click-to-dismiss sense sits under the content, so the Undo button still gets its clicks.
                let r = ui.scope_builder(UiBuilder::new().id_salt(("toast", i)).sense(egui::Sense::click()), |ui| {
                    Frame::new().inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
                        ui.set_max_width(420.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(if t.error { icon::WARNING_CIRCLE } else { icon::CHECK_CIRCLE }).color(color).size(16.0));
                            ui.label(&t.text);
                            if let Some(tab) = live(&t.undo)
                                && theme::pill(ui, RichText::new(format!("{} Undo", icon::ARROW_COUNTER_CLOCKWISE)).strong()).on_hover_text("Undo this (⌘Z)").clicked()
                            {
                                undo = Some((i, tab));
                            }
                        });
                    })
                });
                let r = r.response;
                let glass = theme::glass_shapes(r.rect, 14.0, ui.visuals().dark_mode, 235);
                ui.painter().set(slot, egui::Shape::Vec(glass));
                if r.clicked() {
                    dismiss = Some(i);
                }
                ui.add_space(6.0);
            }
        });
        if let Some((i, tab)) = undo {
            self.tabs[tab].apply(ctx, crate::repo::Action::Undo);
            dismiss = Some(i);
        }
        if let Some(i) = dismiss {
            self.toasts.remove(i);
        }
    }
}
