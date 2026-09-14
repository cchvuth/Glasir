//! Inspector for several selected commits: bulk cherry-pick / squash / revert / drop.

use crate::ui::format::keys;
use crate::repo::{Action, MultiOp, RepoTab, Sel};
use crate::theme::{self, icon};
use egui::{RichText, ScrollArea, Ui};

impl RepoTab {
    pub(super) fn multi_panel(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let Some(d) = &self.data else { return };
        let ids = self.multi.clone();
        ui.add_space(6.0);
        ui.label(RichText::new(format!("{} commits selected", ids.len())).heading().strong());
        ui.label(RichText::new(keys("⌘-click to add or remove, ⇧-click for a range.")).small().color(theme::dim(ui)));
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend); // whole buttons move to the next row
            for (kind, label, hint) in [
                (MultiOp::CherryPick, format!("{} Cherry-pick", icon::GIT_COMMIT), "Apply them onto the current branch, oldest first"),
                (MultiOp::CherryPickEdit, format!("{} Cherry-pick…", icon::CHERRIES), "Pick, reword, squash, drop or reorder them, then apply onto the current branch"),
                (MultiOp::Squash, format!("{} Squash", icon::STACK), "Combine consecutive commits of the current branch into one"),
                (MultiOp::Revert, format!("{} Revert", icon::ARROW_COUNTER_CLOCKWISE), "Create commits that undo them"),
                (MultiOp::Drop, format!("{} Drop", icon::TRASH), "Remove them from the current branch"),
            ] {
                if ui.button(label).on_hover_text(hint).clicked() {
                    acts.push(Action::MultiOp(kind, ids.clone()));
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend); // whole buttons move to the next row
            if ui.button(format!("{} Compare oldest ↔ newest", icon::GIT_DIFF)).clicked() {
                let (newest, oldest) = (ids[0].clone(), ids[ids.len() - 1].clone());
                acts.push(Action::Select(Sel::Compare { from: format!("{oldest}^"), to: newest, three_dot: false }));
            }
            if ui.button(format!("{} Copy SHAs", icon::COPY)).clicked() {
                acts.push(Action::Copy(ids.join("\n")));
            }
        });
        ui.add_space(6.0);
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for id in &ids {
                let c = d.row_of.get(id).and_then(|&r| d.commit(r));
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&id[..id.len().min(7)]).monospace().color(theme::dim(ui)));
                    ui.add(egui::Label::new(c.map_or("", |c| c.summary.as_str())).truncate());
                });
            }
        });
    }
}
