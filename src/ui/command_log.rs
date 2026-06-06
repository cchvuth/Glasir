//! Bottom panel listing every git command (and custom command) Glasir ran (⇧⌘L).

use crate::git;
use crate::theme::{self, icon};
use egui::{Align, Layout, RichText, Ui};

pub(super) fn show(ui: &mut Ui, open: &mut bool) {
    let n = git::LOG.lock().unwrap().len();
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{} Command log", icon::TERMINAL_WINDOW)).strong());
        ui.label(RichText::new(format!("{n} commands")).small().color(theme::dim(ui)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if theme::pill(ui, icon::X).on_hover_text("Close (⇧⌘L)").clicked() {
                *open = false;
            }
            if theme::pill(ui, "Clear").clicked() {
                git::LOG.lock().unwrap().clear();
            }
        });
    });
    ui.add_space(4.0);
    let h = 22.0;
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, h, n, |ui, range| {
        let rows: Vec<git::Logged> = git::LOG.lock().unwrap().iter().rev().skip(range.start).take(range.len()).cloned().collect();
        for e in rows {
            ui.horizontal(|ui| {
                ui.set_height(h);
                let (glyph, color, status) = match e.code {
                    Some(0) => (icon::CHECK_CIRCLE, theme::ADD, "exit 0".to_owned()),
                    Some(c) => (icon::X_CIRCLE, theme::DEL, format!("exit {c}")),
                    None => (icon::STOP_CIRCLE, theme::MOD, "cancelled".to_owned()),
                };
                ui.label(RichText::new(glyph).color(color)).on_hover_text(&status);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::pill(ui, RichText::new(icon::COPY).small()).on_hover_text("Copy as shell command").clicked() {
                        ui.ctx().copy_text(e.shell());
                    }
                    ui.label(RichText::new(format!("{} ms", e.ms)).small().monospace().color(theme::dim(ui)));
                    let repo = e.cwd.file_name().map_or_else(|| e.cwd.display().to_string(), |f| f.to_string_lossy().into_owned());
                    ui.label(RichText::new(repo).small().color(theme::dim(ui)));
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        let text = RichText::new(format!("{} {}", e.prog, e.args.join(" "))).monospace();
                        let tip = format!("{}
{status} · {} ms{}{}", e.cwd.display(), e.ms, if e.stderr.is_empty() { "" } else { "\n\n" }, e.stderr);
                        ui.add(egui::Label::new(text).truncate()).on_hover_text(tip);
                    });
                });
            });
        }
    });
}
