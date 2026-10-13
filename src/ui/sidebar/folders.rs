//! Branch and tag names grouped by their `/` prefixes into folder rows (`feature/payments/v2`
//! sits under feature ▸ payments). The tree is `repo::filetree`; the open folders are kept per
//! repo for the session (not saved); with a filter every folder is open.

use crate::git::Ref;
use crate::repo::{Action, RepoData};
use crate::repo::filetree::{FileTree, Node};
use crate::ui::inspector::ROW_H;
use crate::ui::inspector::tree::{INDENT, dir_row};
use egui::{Id, Ui};
use std::collections::HashSet;
use std::sync::Arc;

/// Ref list identity, filter, tree, the open folders it was walked with and the nodes shown.
type Cached = Arc<(usize, String, Arc<FileTree<usize>>, HashSet<String>, Vec<usize>)>;

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
    let (oid, key) = (id.with("open"), Arc::as_ptr(&d.refs) as usize);
    let mut open: HashSet<String> = ui.data(|x| x.get_temp(oid)).unwrap_or_else(|| ancestors(seed.unwrap_or("")));
    let old = ui.data(|x| x.get_temp::<Cached>(id)).filter(|c| c.0 == key && c.1 == filter);
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
    let (top, clip) = (ui.cursor().top(), ui.clip_rect());
    let first = (((clip.top() - top) / step).floor().max(0.0) as usize).min(vis.len());
    let last = ((((clip.bottom() - top) / step).ceil().max(0.0) as usize) + 1).min(vis.len());
    ui.add_space(first as f32 * step);
    let mut toggled = None;
    for &n in &vis[first..last] {
        match &tree.nodes[n] {
            Node::File { idx, depth } => {
                let r = &d.refs[tree.files[*idx]];
                let mut rect = ui.available_rect_before_wrap();
                rect.min.x += *depth as f32 * INDENT + 2.0;
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    ui.spacing_mut().interact_size.y = ROW_H;
                    leaf(ui, r, r.name.rsplit('/').next().unwrap_or(&r.name), acts);
                });
            }
            Node::Dir { path, name, depth, files, .. } => {
                let isopen = filtering || open.contains(path);
                let resp = dir_row(ui, *depth, name, files.len(), !isopen);
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
    use super::ancestors;

    #[test]
    fn the_current_branchs_folders_start_open() {
        let mut v: Vec<_> = ancestors("feature/payments/v2").into_iter().collect();
        v.sort();
        assert_eq!(v, ["feature", "feature/payments"]);
        assert!(ancestors("main").is_empty());
    }
}
