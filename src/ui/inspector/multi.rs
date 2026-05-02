//! Inspector for several selected commits: bulk cherry-pick / squash / revert / drop.

use crate::repo::{Action, MultiOp, RepoTab};
use crate::theme::{self, icon};
use egui::{RichText, ScrollArea, Ui};

impl RepoTab {
    pub(super) fn multi_panel(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let Some(d) = &self.data else { return };
        let ids = self.multi.clone();
        ui.add_space(6.0);
        ui.label(RichText::new(format!("{} commits selected", ids.len())).heading().strong());
        ui.label(RichText::new("⌘-click to add or remove, ⇧-click for a range.").small().color(theme::dim(ui)));
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            for (kind, label, hint) in [
                (MultiOp::CherryPick, format!("{} Cherry-pick", icon::GIT_COMMIT), "Apply them onto the current branch, oldest first"),
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
            if ui.button(format!("{} Copy SHAs", icon::COPY)).clicked() {
                acts.push(Action::Copy(ids.join("\n")));
            }
        });
        ui.separator();
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for id in &ids {
                let c = d.row_of.get(id).and_then(|&r| d.commit(r));
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&id[..id.len().min(7)]).monospace().color(theme::dim(ui)));
                    ui.label(c.map_or("", |c| c.summary.as_str()));
                });
            }
        });
    }
}
