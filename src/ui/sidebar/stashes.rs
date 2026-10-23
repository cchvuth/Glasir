//! The STASHES section of the sidebar and the stash menu (also used by the
//! stash markers in the graph).

use crate::ui::format::keys;
use super::{matches, section};
use crate::repo::{Action, RepoData, Sel, apply_stash_op, drop_stashes, pop_stash, prompt, rename_stash, stash_branch};
use crate::theme;
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
                    ui.add_space(step);
                    continue;
                }
            }
            let picked = multi.len() > 1 && multi.contains(&s.id);
            let resp = theme::keyed(ui, ("stash", &s.id), |ui| ui.add(super::row(ui, picked, egui::RichText::new(&s.message)))).on_hover_text(&s.name);
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
    if ui.button("Create branch from stash…").on_hover_text("A new branch at the commit the stash was made on, with the stash popped onto it").clicked() {
        acts.push(prompt("New branch from stash", "", stash_branch(s)));
    }
    ui.separator();
    if ui.button("Rename stash…").clicked() {
        acts.push(prompt("Rename stash", s.message.as_str(), rename_stash(s)));
    }
    ui.separator();
    if ui.button("Delete stash").clicked() {
        acts.push(Action::Git(drop_stashes(&d.stashes, std::slice::from_ref(&s.name))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Stash;
    use crate::ui::testkit::{Sim, Tab, tab};
    use egui::ScrollArea;
    use std::sync::Arc;

    fn stashes(sim: &Sim, name: &str) -> (Tab, RepoData) {
        let mut t = tab(&sim.ctx, name, &["x"]);
        let mut d = t.data.take().unwrap();
        d.stashes = Arc::new((0..40).map(|i| Stash { id: format!("{i:040}"), base: String::new(), name: format!("stash@{{{i}}}"), message: format!("stash {i}") }).collect());
        (t, d)
    }

    #[test]
    fn menu_stays_with_its_stash_when_the_list_scrolls() {
        let mut sim = Sim::new(300.0, 400.0);
        let (_t, d) = stashes(&sim, "stash-menu");
        let mut acts = vec![];
        sim.menu_survives("stash 7", "Apply stash", |ui, scrolled| {
            ScrollArea::vertical().vertical_scroll_offset(if scrolled { 170.0 } else { 0.0 }).show(ui, |ui| list(ui, &d, &None, "", &mut acts));
        });
        let ops: Vec<_> = acts.iter().filter_map(|a| if let Action::Git(op) = a { op.cmds.first().and_then(|c| c.get(1)) } else { None }).collect();
        assert_eq!(ops, [&d.stashes[7].id]);
    }

    #[test]
    fn rows_in_view_sit_where_all_rows_would() {
        let mut sim = Sim::new(300.0, 400.0);
        let (_t, d) = stashes(&sim, "stash-rows");
        let (mut y0, mut step, mut heights) = (0.0, 0.0, vec![]);
        for off in [0.0, 100.0, 400.0, 900.0] {
            let mut height = 0.0;
            sim.frame(vec![], |ui| height = ScrollArea::vertical().vertical_scroll_offset(off).show(ui, |ui| list(ui, &d, &None, "", &mut vec![])).content_size.y);
            heights.push(height);
            let drawn: Vec<_> = (0..40).filter_map(|i| Some((i, sim.text(|t| t == format!("stash {i}"))?.y + off))).collect();
            if off == 0.0 {
                (y0, step) = (drawn[0].1, drawn[1].1 - drawn[0].1);
            }
            assert!(!drawn.is_empty() && drawn.len() < 20, "{off}: {} rows drawn", drawn.len());
            for (i, y) in drawn {
                assert!((y - (y0 + i as f32 * step)).abs() < 1.0, "offset {off}: stash {i} at {y}");
            }
        }
        assert!(heights.windows(2).all(|w| w[0] == w[1]), "{heights:?}");
    }
}
