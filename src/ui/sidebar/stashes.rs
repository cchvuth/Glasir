//! The STASHES section of the sidebar and the stash menu (also used by the
//! stash markers in the graph).

use crate::ui::format::keys;
use super::{matches, section};
use crate::repo::{Action, RepoData, Sel, apply_stash_op, drop_stashes, pop_stash, prompt, rename_stash, stash_branch};
use egui::{Id, Ui};

/// The stashes ⌘/⇧-clicked together (egui memory: sidebar-only state).
pub const MULTI: &str = "stash-sel";

/// The section: one row per stash; double-click applies it, ⌘/⇧-click selects several.
pub(super) fn list(ui: &mut Ui, d: &RepoData, sel: &Option<Sel>, f: &str, acts: &mut Vec<Action>) {
    let stashes: Vec<_> = d.stashes.iter().filter(|s| matches(f, &s.message)).collect();
    let key = Id::new(MULTI);
    let mut multi: Vec<String> = ui.data(|m| m.get_temp(key)).unwrap_or_default();
    // Gone stashes drop out; clicking anything else ends the multi-selection.
    let live: std::collections::HashSet<&String> = stashes.iter().map(|s| &s.id).collect();
    multi.retain(|id| live.contains(id));
    if !matches!(sel, Some(Sel::Commit(c)) if multi.contains(c)) {
        multi.clear();
    }
    section(ui, "STASHES", stashes.len(), |ui| {
        // Rows are all one height: only the visible ones are built, a spacer stands in for the rest.
        let (clip, top, gap) = (ui.clip_rect(), ui.cursor().top(), ui.spacing().item_spacing.y);
        let mut step = 0.0;
        for (i, s) in stashes.iter().enumerate() {
            if step > 0.0 {
                let y = top + i as f32 * step;
                if y + step < clip.top() || y > clip.bottom() {
                    ui.add_space(step - gap);
                    continue;
                }
            }
            let picked = multi.len() > 1 && multi.contains(&s.id);
            let resp = ui.add(super::row(ui, picked, egui::RichText::new(&s.message))).on_hover_text(&s.name);
            if i == 0 {
                step = resp.rect.height() + gap;
            }
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
                acts.push(Action::Git(apply_stash_op(s)));
            }
            resp.context_menu(|ui| {
                if picked {
                    let n = multi.len();
                    if ui.button(format!("Delete {n} stashes…")).clicked() {
                        acts.push(Action::Git(drop_stashes(&d.stashes, &d.stashes.iter().filter(|s| multi.contains(&s.id)).map(|s| s.name.clone()).collect::<Vec<_>>())));
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
    if ui.button("Apply stash").on_hover_text("Restores staged changes as staged when it can; the stash is kept").clicked() {
        acts.push(Action::Git(apply_stash_op(s)));
    }
    if ui.button("Pop stash").on_hover_text(keys("Apply, then delete the stash (⌘Z puts it back)")).clicked() {
        acts.push(Action::Git(pop_stash(s)));
    }
    if ui.button("Rename stash…").clicked() {
        acts.push(prompt("Rename stash", s.message.as_str(), rename_stash(s)));
    }
    if ui.button("Create branch from stash…").on_hover_text("A new branch at the commit the stash was made on, with the stash popped onto it").clicked() {
        acts.push(prompt("New branch from stash", "", stash_branch(s)));
    }
    if ui.button("Delete stash").clicked() {
        acts.push(Action::Git(drop_stashes(&d.stashes, std::slice::from_ref(&s.name))));
    }
}
