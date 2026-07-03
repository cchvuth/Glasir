//! The STASHES section of the sidebar and the stash menu (also used by the
//! stash markers in the graph).

use super::{matches, section};
use crate::repo::{Action, RepoData, Sel, drop_stashes, op, pop_stash};
use egui::{Id, Ui};

/// The stashes ⌘/⇧-clicked together (egui memory: sidebar-only state).
pub const MULTI: &str = "stash-sel";

/// The section: one row per stash; double-click applies it, ⌘/⇧-click selects several.
pub(super) fn list(ui: &mut Ui, d: &RepoData, sel: &Option<Sel>, f: &str, acts: &mut Vec<Action>) {
    let stashes: Vec<_> = d.stashes.iter().filter(|s| matches(f, &s.message)).collect();
    let key = Id::new(MULTI);
    let mut multi: Vec<String> = ui.data(|m| m.get_temp(key)).unwrap_or_default();
    // Gone stashes drop out; clicking anything else ends the multi-selection.
    multi.retain(|id| stashes.iter().any(|s| &s.id == id));
    if !matches!(sel, Some(Sel::Commit(c)) if multi.contains(c)) {
        multi.clear();
    }
    section(ui, "STASHES", stashes.len(), |ui| {
        for (i, s) in stashes.iter().enumerate() {
            let picked = multi.len() > 1 && multi.contains(&s.id);
            let resp = ui.add(super::row(ui, picked, egui::RichText::new(&s.message))).on_hover_text(&s.name);
            if resp.clicked() {
                let (cmd, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
                let anchor = match sel {
                    Some(Sel::Commit(c)) => stashes.iter().position(|s| &s.id == c),
                    _ => None,
                };
                if shift && let Some(a) = anchor {
                    multi = stashes[a.min(i)..=a.max(i)].iter().map(|s| s.id.clone()).collect();
                } else if cmd {
                    if multi.is_empty() {
                        multi.extend(anchor.map(|a| stashes[a].id.clone()));
                    }
                    match multi.iter().position(|x| x == &s.id) {
                        Some(p) => {
                            multi.remove(p);
                        }
                        None => multi.push(s.id.clone()),
                    }
                } else {
                    multi.clear();
                }
                // The inspector shows the clicked stash (the last remaining one on a ⌘-unselect).
                let shown = if multi.contains(&s.id) || multi.is_empty() { s.id.clone() } else { multi.last().unwrap().clone() };
                acts.push(Action::Select(Sel::Commit(shown)));
            }
            if resp.double_clicked() {
                acts.push(op("Apply stash", &["stash", "apply", &s.id]));
            }
            resp.context_menu(|ui| {
                if picked {
                    let n = multi.len();
                    if ui.button(format!("Delete {n} stashes…")).clicked() {
                        acts.push(Action::Git(drop_stashes(&d.stashes, &multi)));
                    }
                } else {
                    stash_menu(ui, d, s, acts);
                }
            });
        }
    });
    ui.data_mut(|m| m.insert_temp(key, multi));
}

/// Right-click menu for a stash (sidebar and graph markers).
pub fn stash_menu(ui: &mut Ui, d: &RepoData, s: &crate::git::Stash, acts: &mut Vec<Action>) {
    if ui.button("Apply stash").clicked() {
        acts.push(op("Apply stash", &["stash", "apply", &s.id]));
    }
    if ui.button("Pop stash").clicked() {
        acts.push(Action::Git(pop_stash(s)));
    }
    if ui.button("Delete stash").clicked() {
        acts.push(Action::Git(drop_stashes(&d.stashes, std::slice::from_ref(&s.id))));
    }
}
