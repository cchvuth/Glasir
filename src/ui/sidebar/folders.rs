//! Branch and tag names grouped by their `/` prefixes into folder rows (`feature/payments/v2`
//! sits under feature ▸ payments). The tree is `repo::filetree`; the open folders are kept per
//! repo for the session (not saved); with a filter every folder is open.

use crate::git::Ref;
use crate::repo::{Action, RepoData};
use crate::repo::filetree::{FileTree, Node};
use crate::theme;
use crate::ui::inspector::ROW_H;
use crate::ui::inspector::tree::{INDENT, dir_row, visible};
use egui::{Id, Ui};
use std::collections::HashSet;
use std::sync::{Arc, Weak};

/// Ref list identity, filter, tree, the open folders it was walked with and the nodes shown.
type Cached = Arc<(Weak<Vec<Ref>>, String, Arc<FileTree<usize>>, HashSet<String>, Vec<usize>)>;

/// Folders to start open: the ancestors of `name` (the current branch).
fn ancestors(name: &str) -> HashSet<String> {
    name.match_indices('/').map(|(i, _)| name[..i].to_owned()).collect()
}

/// Draws the refs `idx` (indices into `d.refs`; the first `strip` bytes of a name, a remote's
/// `origin/`, are not grouped) as folders. `leaf` draws one ref given its last name segment.
/// `seed`: the name whose folders start open. The tree and the shown rows are cached under `id`
/// while the ref list, filter and open folders stay the same; rows out of view are skipped
/// (all are `ROW_H` high), so thousands of branches stay cheap.
#[allow(clippy::too_many_arguments)]
pub fn show(ui: &mut Ui, d: &RepoData, id: Id, idx: &[usize], strip: usize, filter: &str, seed: Option<&str>, acts: &mut Vec<Action>, mut leaf: impl FnMut(&mut Ui, &Ref, &str, &mut Vec<Action>)) {
    let (oid, key) = (id.with("open"), Arc::downgrade(&d.refs));
    let mut open: HashSet<String> = ui.data(|x| x.get_temp(oid)).unwrap_or_else(|| ancestors(seed.unwrap_or("")));
    let old = ui.data(|x| x.get_temp::<Cached>(id)).filter(|c| c.0.ptr_eq(&key) && c.1 == filter);
    let filtering = !filter.is_empty();
    let cached = match &old {
        Some(c) if c.3 == open => c.clone(),
        _ => {
            let tree = old.map_or_else(|| Arc::new(FileTree::build_by(idx.iter().map(|&i| (d.refs[i].name[strip..].to_owned(), i)))), |c| c.2.clone());
            // `visible` takes the closed folders: every folder not open (none while filtering).
            let closed: HashSet<String> = if filtering {
                HashSet::new()
            } else {
                tree.nodes.iter().filter_map(|n| if let Node::Dir { path, .. } = n { Some(path.clone()).filter(|p| !open.contains(p)) } else { None }).collect()
            };
            let c: Cached = Arc::new((key, filter.to_owned(), tree.clone(), open.clone(), tree.visible(&closed)));
            ui.data_mut(|x| x.insert_temp(id, c.clone()));
            c
        }
    };
    let (tree, vis) = (&cached.2, &cached.4);
    let step = ROW_H + ui.spacing().item_spacing.y;
    let (first, last) = visible(ui, step, vis.len());
    ui.add_space(first as f32 * step);
    let mut toggled = None;
    for &n in &vis[first..last] {
        match &tree.nodes[n] {
            Node::File { idx, depth } => {
                let r = &d.refs[tree.files[*idx]];
                let mut rect = ui.available_rect_before_wrap();
                rect.min.x += *depth as f32 * INDENT + 2.0;
                ui.scope_builder(egui::UiBuilder::new().id(id.with(&r.full)).max_rect(rect), |ui| {
                    ui.spacing_mut().interact_size.y = ROW_H;
                    ui.spacing_mut().button_padding.y = 0.0;
                    leaf(ui, r, r.name.rsplit('/').next().unwrap_or(&r.name), acts);
                });
            }
            Node::Dir { path, name, depth, files, .. } => {
                let isopen = filtering || open.contains(path);
                let resp = theme::keyed(ui, ("dir", id, path), |ui| dir_row(ui, *depth, name, files.len(), !isopen));
                let refs: Vec<&Ref> = tree.files[files.clone()].iter().map(|&i| &d.refs[i]).collect();
                super::eye(ui, resp.rect, d, &refs, acts);
                if resp.clicked() && !filtering {
                    toggled = Some(path.clone());
                }
            }
        }
    }
    ui.add_space((vis.len() - last) as f32 * step);
    if let Some(p) = toggled {
        if !open.remove(&p) {
            open.insert(p);
        }
    }
    ui.data_mut(|x| x.insert_temp(oid, open));
}

#[cfg(test)]
mod tests {
    use super::super::local_row;
    use super::*;
    use crate::ui::testkit::{Sim, Tab, local_ref, tab};
    use egui::ScrollArea;

    #[test]
    fn the_current_branchs_folders_start_open() {
        let mut v: Vec<_> = ancestors("feature/payments/v2").into_iter().collect();
        v.sort();
        assert_eq!(v, ["feature", "feature/payments"]);
        assert!(ancestors("main").is_empty());
    }

    fn branches(sim: &Sim, name: &str, list: &[String]) -> (Tab, RepoData) {
        let mut t = tab(&sim.ctx, name, &["x"]);
        let mut d = t.data.take().unwrap();
        d.refs = Arc::new(list.iter().map(|n| local_ref(n)).collect());
        (t, d)
    }

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("feature/b{i:03}")).collect()
    }

    #[test]
    fn menu_stays_with_its_branch_when_the_list_scrolls() {
        let mut sim = Sim::new(300.0, 400.0);
        let (_t, d) = branches(&sim, "folder-menu", &names(80));
        let (idx, mut acts) = ((0..80).collect::<Vec<_>>(), vec![]);
        sim.menu_survives("b009", "Checkout ", |ui, scrolled| {
            ScrollArea::vertical().vertical_scroll_offset(if scrolled { 250.0 } else { 0.0 }).show(ui, |ui| show(ui, &d, Id::new("tree"), &idx, 0, "b", None, &mut acts, |ui, r, name, acts| local_row(ui, &d, r, name, 0, acts)));
        });
        let ops: Vec<_> = acts.iter().filter_map(|a| if let Action::Git(op) = a { Some(op.label.as_str()) } else { None }).collect();
        assert_eq!(ops, ["Checkout feature/b009"]);
    }

    #[test]
    fn rows_in_view_sit_where_all_rows_would() {
        let mut sim = Sim::new(300.0, 400.0);
        let (_t, d) = branches(&sim, "folder-rows", &names(120));
        let (idx, step) = ((0..120).collect::<Vec<_>>(), ROW_H + sim.ctx.global_style().spacing.item_spacing.y);
        let (mut y0, mut heights) = (0.0, vec![]);
        for off in [0.0, 100.0, 400.0, 900.0, 2000.0] {
            let mut height = 0.0;
            sim.frame(vec![], |ui| {
                height = ScrollArea::vertical().vertical_scroll_offset(off).show(ui, |ui| show(ui, &d, Id::new("tree"), &idx, 0, "b", None, &mut vec![], |ui, r, name, acts| local_row(ui, &d, r, name, 0, acts))).content_size.y;
            });
            heights.push(height);
            let drawn: Vec<_> = (0..120).filter_map(|i| Some((i, sim.text(|t| t.ends_with(&format!("b{i:03}")))?.y + off))).collect();
            if off == 0.0 {
                y0 = drawn[0].1;
                assert!((drawn[1].1 - drawn[0].1 - step).abs() < 0.01, "leaves are {} apart, not {step}", drawn[1].1 - drawn[0].1);
            }
            assert!(!drawn.is_empty() && drawn.len() < 40, "{off}: {} rows drawn", drawn.len());
            for (i, y) in drawn {
                assert!((y - (y0 + i as f32 * step)).abs() < 1.0, "offset {off}: b{i:03} at {y}");
            }
        }
        assert!(heights.windows(2).all(|w| w[0] == w[1]), "{heights:?}");
    }

    #[test]
    fn a_list_closing_above_the_view_draws_no_rows() {
        let mut sim = Sim::new(300.0, 400.0);
        let (_t, d) = branches(&sim, "folder-closing", &names(120));
        let (idx, mut inverted) = ((0..120).collect::<Vec<_>>(), false);
        for (off, open) in [(0.0, true), (2000.0, false)] {
            sim.frame(vec![], |ui| {
                ScrollArea::vertical().vertical_scroll_offset(off).show(ui, |ui| {
                    egui::CollapsingHeader::new("TAGS").open(Some(open)).show(ui, |ui| {
                        inverted |= !ui.clip_rect().is_positive();
                        show(ui, &d, Id::new("tree"), &idx, 0, "b", None, &mut vec![], |ui, r, name, acts| local_row(ui, &d, r, name, 0, acts))
                    });
                });
            });
        }
        assert!(inverted, "the closing body's clip rect was not inverted");
    }

    #[test]
    fn a_newer_ref_list_never_passes_for_the_cached_one() {
        let mut sim = Sim::new(300.0, 400.0);
        let (_t, mut d) = branches(&sim, "folder-cache", &names(50));
        let mut shown = |d: &RepoData| {
            let idx: Vec<_> = (0..d.refs.len()).collect();
            sim.frame(vec![], |ui| {
                ScrollArea::vertical().show(ui, |ui| show(ui, d, Id::new("tree"), &idx, 0, "b", None, &mut vec![], |ui, r, name, acts| local_row(ui, d, r, name, 0, acts)));
            });
            (0..d.refs.len()).filter(|&i| sim.text(|t| t.ends_with(&d.refs[i].name[8..])).is_some()).count()
        };
        assert!(shown(&d) > 10);
        let (stale, mut spare) = (Arc::as_ptr(&d.refs), vec![]);
        d.refs = Arc::new(vec![]);
        d.refs = loop {
            let newer = Arc::new(["feature/bb0", "feature/bb1"].map(local_ref).to_vec());
            if Arc::as_ptr(&newer) == stale || spare.len() == 500 {
                break newer;
            }
            spare.push(newer);
        };
        assert_eq!(shown(&d), 2);
    }
}
