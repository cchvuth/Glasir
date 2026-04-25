//! Inspector for a commit: message, author and the changed files.

use super::file_row;
use crate::repo::{Action, DiffSrc, RepoTab};
use crate::theme;
use crate::ui::format::initials;
use egui::{Color32, RichText, ScrollArea, Sense, Ui};

impl RepoTab {
    pub(super) fn commit_panel(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let Some(info) = &self.info else {
            ui.centered_and_justified(|ui| ui.spinner());
            return;
        };
        let info = match info {
            Ok(i) => i,
            Err(e) => {
                ui.colored_label(theme::DEL, e);
                return;
            }
        };
        let open = self.diff.as_ref().map(|d| d.src.clone());
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("commit").color(theme::dim(ui)));
                if ui.link(RichText::new(&info.id[..info.id.len().min(10)]).monospace()).on_hover_text("Copy full SHA").clicked() {
                    acts.push(Action::Copy(info.id.clone()));
                }
                if !info.parents.is_empty() {
                    ui.label(RichText::new("parent").color(theme::dim(ui)));
                    for p in &info.parents {
                        if ui.link(RichText::new(&p[..p.len().min(7)]).monospace()).clicked() {
                            acts.push(Action::Jump(p.clone()));
                        }
                    }
                }
            });
            ui.add_space(6.0);
            let (summary, body) = info.message.split_once('\n').unwrap_or((&info.message, ""));
            ui.label(RichText::new(summary).heading().strong());
            if !body.trim().is_empty() {
                ui.add_space(4.0);
                ui.label(body.trim());
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), Sense::hover());
                ui.painter().circle_filled(rect.center(), 15.0, theme::avatar(&info.email));
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, initials(&info.author), egui::FontId::proportional(12.0), Color32::WHITE);
                ui.vertical(|ui| {
                    ui.label(RichText::new(&info.author).strong()).on_hover_text(&info.email);
                    ui.label(RichText::new(format!("authored {}", info.date)).small().color(theme::dim(ui)));
                    if info.committer != info.author {
                        ui.label(RichText::new(format!("committed by {} {}", info.committer, info.committer_date)).small().color(theme::dim(ui)));
                    }
                });
            });
            ui.add_space(8.0);
            ui.separator();
            let n = info.files.len();
            ui.label(RichText::new(format!("{n} changed file{}", if n == 1 { "" } else { "s" })).small().strong());
            for c in &info.files {
                let src = DiffSrc::Commit { id: info.id.clone(), parent: info.parents.first().cloned(), change: c.clone() };
                let resp = file_row(ui, c, open.as_ref() == Some(&src));
                if resp.clicked() {
                    acts.push(Action::Diff(src));
                }
                resp.context_menu(|ui| {
                    if ui.button("Copy path").clicked() {
                        acts.push(Action::Copy(c.path.clone()));
                    }
                });
            }
        });
    }
}
