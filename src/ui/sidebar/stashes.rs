//! The STASHES section of the sidebar and the stash menu (also used by the
//! stash markers in the graph).

use super::{matches, section};
use crate::repo::{Action, Op, RepoData, Sel, op};
use egui::Ui;

/// The section: one row per stash; double-click applies it.
pub(super) fn list(ui: &mut Ui, d: &RepoData, f: &str, acts: &mut Vec<Action>) {
    let stashes: Vec<_> = d.stashes.iter().filter(|s| matches(f, &s.message)).collect();
    section(ui, "STASHES", stashes.len(), |ui| {
        for s in stashes {
            let resp = ui.add(egui::Button::selectable(false, &s.message).frame_when_inactive(false)).on_hover_text(&s.name);
            if resp.clicked() {
                acts.push(Action::Select(Sel::Commit(s.id.clone())));
            }
            if resp.double_clicked() {
                acts.push(op("Apply stash", &["stash", "apply", &s.name]));
            }
            resp.context_menu(|ui| stash_menu(ui, s, acts));
        }
    });
}

/// Right-click menu for a stash (sidebar and graph markers).
pub fn stash_menu(ui: &mut Ui, s: &crate::git::Stash, acts: &mut Vec<Action>) {
    if ui.button("Apply stash").clicked() {
        acts.push(op("Apply stash", &["stash", "apply", &s.name]));
    }
    if ui.button("Pop stash").clicked() {
        acts.push(op("Pop stash", &["stash", "pop", &s.name]));
    }
    if ui.button("Delete stash").clicked() {
        acts.push(Action::Git(Op::new("Drop stash", &["stash", "drop", &s.name]).confirm(format!("Delete {}?", s.name))));
    }
}
