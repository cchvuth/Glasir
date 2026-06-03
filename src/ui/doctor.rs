//! The connection doctor window: whether git reaches the remote without
//! prompting, what is wrong and how to fix it, and the settings git uses.

use crate::repo::{Action, RepoTab};
use crate::theme::{self, icon};
use egui::{Frame, Id, Key, RichText};

pub(super) fn doctor_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let Some(doc) = &t.doctor else { return };
    let mut close = false;
    egui::Modal::new(Id::new("doctor")).show(ctx, |ui| {
        ui.set_width(620.0);
        ui.heading(format!("{} Connection to {}", icon::STETHOSCOPE, doc.remote));
        let Some(r) = &doc.report else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Running git ls-remote without prompting…").color(theme::dim(ui)));
            });
            close = ui.button("Close").clicked() || ui.input(|i| i.key_pressed(Key::Escape));
            return;
        };
        ui.label(RichText::new(&r.url).monospace().small().color(theme::dim(ui)));
        ui.add_space(6.0);
        let host = crate::git::host(&r.url).unwrap_or_else(|| "<host>".into());
        match r.problem {
            None => {
                ui.label(RichText::new(format!("{} Connected: git can fetch from {} without asking for anything.", icon::CHECK_CIRCLE, doc.remote)).color(theme::ADD).strong());
            }
            Some((what, hint)) => {
                ui.label(RichText::new(format!("{} {what}", icon::WARNING)).color(theme::DEL).strong());
                ui.label(hint.replace("<host>", &host));
            }
        }
        if !r.output.is_empty() && r.problem.is_some() {
            ui.add_space(4.0);
            Frame::new().fill(theme::faint(ui)).corner_radius(8).inner_margin(8).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(110.0).show(ui, |ui| ui.label(RichText::new(&r.output).monospace().size(11.5)));
            });
        }
        ui.add_space(8.0);
        ui.label(RichText::new("What git uses for this remote").small().strong());
        egui::Grid::new("doctor_settings").num_columns(2).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
            for (k, v) in &r.settings {
                ui.label(RichText::new(k).monospace().small());
                ui.label(RichText::new(v).monospace().small());
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(format!("{} Check again", icon::ARROWS_CLOCKWISE)).clicked() {
                acts.push(Action::Doctor(doc.remote.clone()));
            }
            if ui.button(format!("{} Copy report", icon::COPY)).clicked() {
                let mut s = format!("{}  {}\n{}\n\n{}\n", doc.remote, r.url, r.problem.map_or("Connected", |p| p.0), r.output);
                for (k, v) in &r.settings {
                    s += &format!("{k} = {v}\n");
                }
                acts.push(Action::Copy(s));
            }
            close = ui.button("Close").clicked() || ui.input(|i| i.key_pressed(Key::Escape));
        });
    });
    if close {
        t.doctor = None;
    }
}
