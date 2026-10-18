//! The List / Tree switch of the changed-file lists, and the tree walker: folder rows
//! (caret, icon, name, count) between the file rows. The tree itself is `repo::filetree`.

use super::{ROW_H, hovering};
use crate::git::Change;
use crate::repo::filetree::{FileTree, Node};
use crate::theme::{self, icon};
use egui::{Id, Response, RichText, Sense, Ui};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const INDENT: f32 = 14.0;

/// Where a file sits among the rows shown, for ⇧-click ranges.
pub enum Order<'a> {
    List(&'a [Change], usize),
    Tree(&'a FileTree, &'a HashSet<String>),
}

impl Order<'_> {
    /// The files in the order shown (what a collapsed folder hides left out) and `c`'s place in it.
    pub fn place(&self, c: &Change) -> (Cow<'_, [Change]>, usize) {
        match self {
            Order::List(l, i) => (Cow::Borrowed(l), *i),
            Order::Tree(t, closed) => {
                let v = t.visible_files(closed);
                let i = v.iter().position(|x| x.path == c.path).unwrap_or(0);
                (Cow::Owned(v), i)
            }
        }
    }
}

pub enum Item<'a> {
    /// A file row; `depth` is `Some` in tree view.
    File { c: &'a Change, depth: Option<usize>, order: Order<'a> },
    /// A folder row, already drawn: `files` are the changes under it.
    Dir { path: &'a str, files: &'a [Change], resp: &'a Response },
}

/// Extra left offset of a file row at tree `depth` (`None`: flat list).
pub fn indent(depth: Option<usize>) -> f32 {
    depth.map_or(0.0, |d| 14.0 + d as f32 * INDENT)
}

pub fn toggle(ui: &mut Ui, tree: &mut bool) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let mut both = [(false, icon::LIST_BULLETS, "View as a list"), (true, icon::TREE_STRUCTURE, "View as a folder tree")];
        // List | Tree also in the right-to-left header rows it sits in.
        if ui.layout().prefer_right_to_left() {
            both.reverse();
        }
        for (on, glyph, tip) in both {
            let sel = *tree == on;
            let text = RichText::new(glyph).size(13.0).color(if sel { theme::accent() } else { theme::dim(ui) });
            let b = egui::Button::new(text).small().corner_radius(255).frame_when_inactive(sel).fill(theme::accent().gamma_multiply(if sel { 0.2 } else { 0.0 }));
            if ui.add(b).on_hover_text(tip).clicked() {
                *tree = on;
            }
        }
    });
}

/// A folder row. Clicking it (unless `show` hands the click to a button) folds it.
pub fn dir_row(ui: &mut Ui, depth: usize, name: &str, count: usize, closed: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
    if hovering(ui, rect) {
        ui.painter().rect_filled(rect, 8.0, theme::hover(ui));
    }
    let (x, y) = (rect.left() + 6.0 + depth as f32 * INDENT, rect.center().y);
    let p = ui.painter().with_clip_rect(rect);
    p.text(egui::pos2(x, y), egui::Align2::LEFT_CENTER, if closed { icon::CARET_RIGHT } else { icon::CARET_DOWN }, egui::FontId::proportional(11.0), theme::dim(ui));
    p.text(egui::pos2(x + 14.0, y), egui::Align2::LEFT_CENTER, icon::FOLDER_SIMPLE, egui::FontId::proportional(13.0), theme::accent());
    let r = p.text(egui::pos2(x + 32.0, y), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(13.0), theme::text(ui));
    p.text(egui::pos2(r.right() + 6.0, y), egui::Align2::LEFT_CENTER, count, egui::FontId::proportional(11.5), theme::dim(ui));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// One changed-file list as flat rows or, with `tree`, as folders. `row` draws a file (or adds a
/// folder's buttons and menu) and returns true when it took the click (a hover button), so the
/// folder doesn't fold. `closed` is this list's folded folders, kept by the caller; `key` changes
/// whenever the list may have (see `show`).
#[allow(clippy::too_many_arguments)]
pub fn list(ui: &mut Ui, list: &[Change], key: u64, tree: bool, name: &'static str, closed: &mut HashMap<&'static str, HashSet<String>>, all: bool, mut row: impl FnMut(&mut Ui, Item) -> bool) {
    let id = ui.id().with(("filetree", name));
    if tree {
        show(ui, list.iter(), key, id, closed.entry(name).or_default(), all, row);
    } else {
        // Flat rows are `ROW_H` high too: with `all` off only those in view are drawn.
        let step = ROW_H + ui.spacing().item_spacing.y;
        let (first, last) = if all { (0, list.len()) } else { visible(ui, step, list.len()) };
        if first > 0 {
            ui.add_space(first as f32 * step);
        }
        for (i, c) in list.iter().enumerate().take(last).skip(first) {
            theme::keyed(ui, ("file", id, &c.path), |ui| row(ui, Item::File { c, depth: None, order: Order::List(list, i) }));
        }
        if last < list.len() {
            ui.add_space((list.len() - last) as f32 * step);
        }
    }
}

pub fn visible(ui: &Ui, step: f32, n: usize) -> (usize, usize) {
    let (top, clip) = (ui.cursor().top(), ui.clip_rect());
    let first = (((clip.top() - top) / step).floor().max(0.0) as usize).min(n);
    (first, (((clip.bottom() - top) / step).ceil().max(0.0) as usize + 1).clamp(first, n))
}

type Cached = Arc<(u64, Arc<FileTree>, HashSet<String>, Vec<usize>)>;

/// The tree rows. The tree and its shown rows are cached under `id` while `key` (a hash of
/// `changes`) and `closed` stay the same, so a frame
/// only walks what's in view: every row is `ROW_H` high, so with `all` off only those are drawn.
pub fn show<'a>(ui: &mut Ui, changes: impl Iterator<Item = &'a Change>, key: u64, id: Id, closed: &mut HashSet<String>, all: bool, mut row: impl FnMut(&mut Ui, Item) -> bool) {
    let old = ui.data(|d| d.get_temp::<Cached>(id)).filter(|c| c.0 == key);
    let cached = match &old {
        Some(c) if c.2 == *closed => c.clone(),
        _ => {
            let tree = old.map_or_else(|| Arc::new(FileTree::build(changes)), |c| c.1.clone());
            let c: Cached = Arc::new((key, tree.clone(), closed.clone(), tree.visible(closed)));
            ui.data_mut(|d| d.insert_temp(id, c.clone()));
            c
        }
    };
    let (tree, vis) = (&cached.1, &cached.3);
    let gap = ui.spacing().item_spacing.y;
    let step = ROW_H + gap;
    let (first, last) = if all {
        (0, vis.len())
    } else {
        visible(ui, step, vis.len())
    };
    if first > 0 {
        ui.add_space(first as f32 * step);
    }
    let mut toggled = None;
    for &n in &vis[first..last] {
        match &tree.nodes[n] {
            Node::File { idx, depth } => {
                let c = &tree.files[*idx];
                theme::keyed(ui, ("file", id, &c.path), |ui| row(ui, Item::File { c, depth: Some(*depth), order: Order::Tree(tree, closed) }));
            }
            Node::Dir { path, name, depth, files, .. } => {
                let resp = theme::keyed(ui, ("dir", id, path), |ui| dir_row(ui, *depth, name, files.len(), closed.contains(path)));
                let took = row(ui, Item::Dir { path, files: &tree.files[files.clone()], resp: &resp });
                if resp.clicked() && !took {
                    toggled = Some(path.clone());
                }
            }
        }
    }
    if last < vis.len() {
        ui.add_space((vis.len() - last) as f32 * step);
    }
    if let Some(p) = toggled
        && !closed.remove(&p)
    {
        closed.insert(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton, RawInput, pos2};

    /// Clicking a folder row folds it, and again unfolds it; files are only handed to `row` while shown.
    #[test]
    fn clicking_a_folder_folds_it() {
        let ctx = egui::Context::default();
        let cs: Vec<Change> = ["a/x.rs", "a/y.rs"].iter().map(|p| Change { path: (*p).into(), old_path: None, code: 'M' }).collect();
        let (mut closed, mut files_seen) = (HashSet::new(), vec![]);
        let at = pos2(40.0, 12.0);
        let press = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        let frames = [vec![Event::PointerMoved(at)], vec![press(true)], vec![press(false)], vec![], vec![]];
        for (i, events) in frames.into_iter().enumerate() {
            files_seen.clear();
            let raw = RawInput { time: Some(i as f64 * 0.05), events, ..Default::default() };
            let mut out = ctx.run_ui(raw, |ui| {
                show(ui, cs.iter(), 0, Id::new("t"), &mut closed, true, |_, item| {
                    if let Item::File { c, .. } = item {
                        files_seen.push(c.path.clone());
                    }
                    false
                });
            });
            out.textures_delta.clear();
        }
        assert!(closed.contains("a"));
        assert!(files_seen.is_empty());
    }

    /// Only the rows in view are drawn, each where it would be with all drawn, and the list's
    /// height stays the same at every scroll offset (nothing below it jumps).
    #[test]
    fn rows_in_view_sit_where_all_rows_would() {
        let ctx = egui::Context::default();
        let cs: Vec<Change> = (0..220).map(|i| Change { path: format!("f{i:03}"), old_path: None, code: 'M' }).collect();
        let mut closed = HashSet::new();
        let mut heights = vec![];
        for off in [0.0, 20.0, 31.0, 200.0, 4000.0] {
            let mut drawn = vec![];
            let raw = RawInput { screen_rect: Some(egui::Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(300.0, 400.0))), ..Default::default() };
            let mut out = ctx.run_ui(raw, |ui| {
                let o = egui::ScrollArea::vertical().vertical_scroll_offset(off).show(ui, |ui| {
                    let top = ui.min_rect().top();
                    show(ui, cs.iter(), 0, Id::new("v"), &mut closed, false, |ui, item| {
                        if let Item::File { c, .. } = item {
                            drawn.push((c.path[1..].parse::<f32>().unwrap(), ui.cursor().top() - top));
                            ui.allocate_exact_size(egui::vec2(10.0, ROW_H), Sense::hover());
                        }
                        false
                    });
                });
                heights.push(o.content_size.y);
            });
            out.textures_delta.clear();
            let step = ROW_H + ctx.global_style().spacing.item_spacing.y;
            assert!(!drawn.is_empty() && drawn.len() < 40, "{off}: {}", drawn.len());
            for (i, y) in drawn {
                assert!((y - i * step).abs() < 0.01, "offset {off}: row {i} at {y}");
            }
        }
        assert!(heights.windows(2).all(|w| w[0] == w[1]), "{heights:?}");
    }

    #[test]
    fn a_list_closing_above_the_view_draws_no_rows() {
        let mut sim = crate::ui::testkit::Sim::new(300.0, 400.0);
        let cs: Vec<Change> = (0..120).map(|i| Change { path: format!("d/f{i:03}"), old_path: None, code: 'M' }).collect();
        let (mut closed, mut inverted) = (HashSet::new(), false);
        for (off, open) in [(0.0, true), (2000.0, false)] {
            sim.frame(vec![], |ui| {
                egui::ScrollArea::vertical().vertical_scroll_offset(off).show(ui, |ui| {
                    egui::CollapsingHeader::new("Unstaged").open(Some(open)).show(ui, |ui| {
                        inverted |= !ui.clip_rect().is_positive();
                        show(ui, cs.iter(), 0, Id::new("v"), &mut closed, false, |ui, item| {
                            if let Item::File { .. } = item {
                                ui.allocate_exact_size(egui::vec2(10.0, ROW_H), Sense::hover());
                            }
                            false
                        });
                    });
                });
            });
        }
        assert!(inverted, "the closing body's clip rect was not inverted");
    }

    #[test]
    fn menu_stays_with_its_file_when_the_list_scrolls() {
        let mut sim = crate::ui::testkit::Sim::new(300.0, 400.0);
        let cs: Vec<Change> = (0..220).map(|i| Change { path: format!("f{i:03}"), old_path: None, code: 'M' }).collect();
        for as_tree in [false, true] {
            let (mut closed, mut picked) = (HashMap::new(), vec![]);
            sim.menu_survives("f007", "Pick", |ui, scrolled| {
                egui::ScrollArea::vertical().vertical_scroll_offset(if scrolled { 170.0 } else { 0.0 }).show(ui, |ui| {
                    list(ui, &cs, 0, as_tree, "t", &mut closed, false, |ui, item| {
                        if let Item::File { c, depth, .. } = item {
                            super::super::file_row(ui, c, false, false, None, depth).context_menu(|ui| {
                                if ui.button("Pick").clicked() {
                                    picked.push(c.path.clone());
                                }
                            });
                        }
                        false
                    });
                });
            });
            assert_eq!(picked, ["f007"], "as a tree: {as_tree}");
        }
    }
}
