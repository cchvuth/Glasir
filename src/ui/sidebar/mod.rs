//! Left sidebar: filter box, local / remote branches, tags, worktrees, submodules and stashes.
//! `ref_menus` holds the right-click menus of refs (shared with the graph pills),
//! `remotes` a remote's menu and the add / edit remote dialog.

pub mod ref_menus;
pub mod remotes;
pub mod stashes;
mod folders;
mod submodules;
mod worktrees;
pub(super) use worktrees::worktree_name;

use crate::git::RefKind;
use crate::repo::{Action, RepoData, Sel, branch_checkout, stack};
use crate::theme::{self, icon};
use crate::ui::graph_view::dnd::branch_dnd;
use egui::{RichText, ScrollArea, TextEdit, Ui};
use ref_menus::ref_menu;
use std::collections::HashSet;

fn matches(filter: &str, name: &str) -> bool {
    filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase())
}

/// Hidden refs are dimmed, soloed ones orange.
fn view_style(ui: &Ui, d: &RepoData, r: &crate::git::Ref, text: RichText) -> RichText {
    if d.view.solo.contains(&r.full) {
        text.color(theme::changed())
    } else if !d.shows(&r.full) {
        text.color(theme::dim(ui).gamma_multiply(0.7))
    } else {
        text
    }
}

/// An eye at the right end of the hovered row: hides `refs` from the graph, or shows them again
/// once all are hidden. HEAD's branch is always in the graph, so it's left out.
pub(super) fn eye(ui: &mut Ui, row: egui::Rect, d: &RepoData, refs: &[&crate::git::Ref], acts: &mut Vec<Action>) {
    let refs: Vec<&str> = refs.iter().filter(|r| !r.head).map(|r| r.full.as_str()).collect();
    let row = egui::Rect::from_x_y_ranges(row.left()..=ui.max_rect().right(), row.y_range());
    if refs.is_empty() || !d.view.solo.is_empty() || !ui.rect_contains_pointer(row) {
        return;
    }
    let hidden = refs.iter().all(|f| !d.shows(f));
    let at = egui::Rect::from_center_size(egui::pos2(row.right() - 14.0, row.center().y), egui::vec2(26.0, 22.0));
    let resp = ui.interact(at, egui::Id::new(("eye", &refs, row.top() as i32)), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.hovered() {
        ui.painter().rect_filled(at, 6.0, theme::hover(ui));
    }
    let col = if resp.hovered() { theme::text(ui) } else { theme::dim(ui) };
    ui.painter().text(at.center(), egui::Align2::CENTER_CENTER, if hidden { icon::EYE_SLASH } else { icon::EYE }, egui::FontId::proportional(14.0), col);
    if resp.on_hover_text(if hidden { "Show in graph" } else { "Hide from graph" }).clicked() {
        acts.push(Action::View(crate::repo::ViewChange::Hide(refs.iter().map(|f| f.to_string()).collect(), !hidden)));
    }
}

/// A thin line between the pinned default/long-lived branches and the rest.
fn pinned_separator(ui: &mut Ui, d: &RepoData, i: usize, names: &[&str]) {
    if i > 0 && d.branch_rank(names[i - 1]) < 2 && d.branch_rank(names[i]) == 2 {
        let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 9.0), egui::Sense::hover());
        let c = ui.visuals().widgets.noninteractive.fg_stroke.color.gamma_multiply(0.12);
        ui.painter().hline(r.x_range().shrink(8.0), r.center().y, egui::Stroke::new(1.0, c));
    }
}

/// A sidebar row. The current one (the picked stash) gets a soft
/// accent tint and keeps the normal text colour: egui's selected style uses
/// white text, which frameless rows show on the light themes' pale background.
pub(super) fn row(ui: &Ui, current: bool, text: RichText) -> egui::Button<'static> {
    if current {
        egui::Button::new(text.color(theme::text(ui))).fill(theme::selection(ui))
    } else {
        egui::Button::new(text).frame_when_inactive(false)
    }
}

/// HEAD's row (its branch, this worktree, a detached HEAD): a solid accent capsule like its pill in
/// the graph. `styled`: text already coloured (a hidden or solo branch), kept on a soft tint instead.
pub(super) fn head_row(ui: &Ui, text: RichText, styled: bool) -> egui::Button<'static> {
    let a = theme::accent();
    if styled {
        return egui::Button::new(text).fill(theme::selection(ui));
    }
    egui::Button::new(text.color(egui::Color32::WHITE)).fill(a).stroke(egui::Stroke::new(1.0, egui::Color32::from_white_alpha(90)))
}

/// Stacked branches right under their parent, with their depth (`repo::stack`).
fn stacked<'a>(d: &'a RepoData, locals: &[&'a crate::git::Ref]) -> Vec<(&'a crate::git::Ref, usize)> {
    fn add<'a>(d: &'a RepoData, r: &'a crate::git::Ref, depth: usize, out: &mut Vec<(&'a crate::git::Ref, usize)>, seen: &mut HashSet<&'a str>) {
        seen.insert(&r.name);
        out.push((r, depth));
        for c in stack::children(d, &r.name).into_iter().filter_map(|c| d.local(c)) {
            if depth < 16 && !seen.contains(c.name.as_str()) {
                add(d, c, depth + 1, out, seen);
            }
        }
    }
    let (mut out, mut seen) = (vec![], HashSet::new());
    for r in locals.iter().filter(|r| stack::parent(d, &r.name).is_none_or(|p| d.local(p).is_none())) {
        add(d, r, 0, &mut out, &mut seen);
    }
    // A loop of stale edges has no root: never lose a branch from the list.
    for r in locals {
        if !seen.contains(r.name.as_str()) {
            out.push((r, 0));
        }
    }
    out
}

/// A local branch row: marker, stack / ahead / behind badges, menus, drag-and-drop. `name` is what
/// it shows (the last segment inside a folder); `depth` its stack depth.
fn local_row(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, name: &str, depth: usize, acts: &mut Vec<Action>) {
    let glyph = if r.head { icon::CHECK } else if r.worktree.is_some() { icon::TREE } else { icon::GIT_BRANCH };
    let indent = if depth > 0 { format!("{}{} ", "    ".repeat(depth - 1), icon::ARROW_ELBOW_DOWN_RIGHT) } else { String::new() };
    let mut text = format!("{indent}{glyph}  {name}");
    if depth == 0 && !stack::children(d, &r.name).is_empty() {
        text += &format!("  {}", icon::STACK);
    }
    if stack::stale(d, &r.name) {
        text += &format!("  {}", icon::ARROWS_CLOCKWISE);
    }
    if r.ahead > 0 {
        text += &format!("  ↑ {}", r.ahead);
    }
    if r.behind > 0 {
        text += &format!("  ↓ {}", r.behind);
    }
    let mut rt = RichText::new(text);
    if r.head {
        rt = rt.strong();
    }
    let styled = d.view.solo.contains(&r.full) || !d.shows(&r.full);
    let rt = view_style(ui, d, r, rt);
    let resp = theme::keyed(ui, ("local", &r.full, depth), |ui| ui.add(if r.head { head_row(ui, rt, styled) } else { row(ui, false, rt) }));
    branch_dnd(ui, &resp, &r.name, &r.full, acts);
    eye(ui, resp.rect, d, &[r], acts);
    let mut tip = if r.gone { format!("{}\nUpstream branch is gone", r.name) } else { r.name.clone() };
    if r.head {
        tip += "\nCurrent branch (HEAD)";
    }
    if let Some(p) = stack::parent(d, &r.name) {
        tip += &format!("\nStacked on {p}");
        if stack::stale(d, &r.name) {
            tip += &format!("; {p} has moved on since: right-click → Restack to bring this branch along");
        }
    }
    let resp = resp.on_hover_text(tip);
    if resp.clicked() {
        acts.push(Action::Jump(r.target.clone()));
    }
    if resp.double_clicked() {
        acts.extend(branch_checkout(d, r));
    }
    theme::context_menu(&resp, |ui| ref_menu(ui, d, r, acts));
}

fn remote_row(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, name: &str, acts: &mut Vec<Action>) {
    let text = view_style(ui, d, r, RichText::new(format!("{}  {name}", icon::CLOUD)));
    let resp = theme::keyed(ui, ("remote", &r.full), |ui| ui.add(egui::Button::selectable(false, text).frame_when_inactive(false))).on_hover_text(&r.name);
    branch_dnd(ui, &resp, &r.name, &r.full, acts);
    eye(ui, resp.rect, d, &[r], acts);
    if resp.clicked() {
        acts.push(Action::Jump(r.target.clone()));
    }
    if resp.double_clicked() {
        acts.extend(branch_checkout(d, r));
    }
    theme::context_menu(&resp, |ui| ref_menu(ui, d, r, acts));
}

fn tag_row(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, name: &str, acts: &mut Vec<Action>) {
    let resp = theme::keyed(ui, ("tag", &r.full), |ui| ui.add(egui::Button::selectable(false, view_style(ui, d, r, RichText::new(format!("{}  {name}", icon::TAG)))).frame_when_inactive(false)));
    eye(ui, resp.rect, d, &[r], acts);
    let resp = resp.on_hover_text(if r.message.is_empty() { r.name.clone() } else { format!("{}\n\n{}", r.name, r.message) });
    if resp.clicked() {
        acts.push(Action::Jump(r.target.clone()));
    }
    theme::context_menu(&resp, |ui| ref_menu(ui, d, r, acts));
}

fn in_view(ui: &mut Ui, step: &mut f32, row: impl FnOnce(&mut Ui)) {
    let (y, clip) = (ui.cursor().top(), ui.clip_rect());
    if *step > 0.0 && (y + *step < clip.top() || y > clip.bottom()) {
        ui.add_space(*step);
        return;
    }
    row(ui);
    if *step == 0.0 {
        *step = ui.cursor().top() - y;
    }
}

fn section(ui: &mut Ui, title: &str, count: usize, add: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(format!("{title}  {count}")).small().strong())
        .id_salt(title)
        .default_open(true)
        .show(ui, add);
}

type TagLists = std::sync::Arc<(std::sync::Weak<Vec<crate::git::Ref>>, String, Vec<usize>, Vec<usize>)>;

pub fn sidebar(ui: &mut Ui, d: &RepoData, dir: &std::path::Path, sel: &Option<Sel>, filter: &mut String, acts: &mut Vec<Action>) {
    // One line per row, cut with "…" (full text on hover): a long name must not
    // wrap or widen the panel.
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
    ui.add_space(4.0);
    theme::bar(ui, |ui| {
        if theme::pill(ui, icon::LIFEBUOY).on_hover_text("Reflog: recover commits lost to a reset or rebase").clicked() {
            acts.push(Action::Reflog);
        }
        if theme::pill(ui, icon::BROOM).on_hover_text("Clean up merged & gone branches").clicked() {
            acts.push(Action::Cleanup);
        }
        theme::rest(ui, |ui| ui.add(TextEdit::singleline(filter).margin(crate::theme::INPUT_MARGIN).hint_text(format!("{}  Filter", icon::MAGNIFYING_GLASS)).desired_width(ui.available_width())));
    });
    ui.add_space(4.0);
    let f = filter.as_str();
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        let mut locals: Vec<_> = d.refs.iter().filter(|r| r.kind == RefKind::Local && matches(f, &r.name)).collect();
        locals.sort_by_key(|r| d.branch_rank(&r.name)); // stable: A–Z within each group
        // While filtering, a stacked branch's parent may be filtered out: a flat list then.
        let roots: Vec<_> = locals.iter().copied().filter(|r| !r.name.contains('/')).collect();
        let rows = if f.is_empty() { stacked(d, &roots) } else { roots.iter().map(|r| (*r, 0)).collect() };
        let names: Vec<&str> = rows.iter().map(|r| r.0.name.as_str()).collect();
        section(ui, "LOCAL", locals.len(), |ui| {
            // Detached: no branch row is current, so HEAD gets its own. Mid-rebase (etc.) that is normal, not a warning.
            if let (None, Some(h), true) = (&d.status.branch, &d.status.head, f.is_empty()) {
                let (sha, clean) = (&h[..h.len().min(7)], d.state == crate::git::State::Clean);
                let text = RichText::new(format!("{}  {} {sha}", icon::CHECK, if clean { "detached" } else { "HEAD" })).strong();
                let tip = if clean { format!("Detached HEAD at {sha}: commits made here are easy to lose") } else { format!("HEAD at {sha}, while an operation is in progress") };
                if ui.add(head_row(ui, text, false)).on_hover_text(tip).clicked() {
                    acts.push(Action::Jump(h.clone()));
                }
            }
            let mut step = 0.0;
            for (i, (r, depth)) in rows.iter().copied().enumerate() {
                pinned_separator(ui, d, i, &names);
                if r.head {
                    local_row(ui, d, r, &r.name, depth, acts);
                } else {
                    in_view(ui, &mut step, |ui| local_row(ui, d, r, &r.name, depth, acts));
                }
            }
            let shown: std::collections::HashSet<&str> = names.iter().copied().collect();
            let nested: Vec<usize> = (0..d.refs.len()).filter(|&i| d.refs[i].kind == RefKind::Local && d.refs[i].name.contains('/') && matches(f, &d.refs[i].name) && !shown.contains(d.refs[i].name.as_str())).collect();
            let seed = d.refs.iter().find(|r| r.head && r.kind == RefKind::Local).map(|r| r.name.as_str());
            folders::show(ui, d, ui.id().with((dir, "local")), &nested, 0, f, seed, acts, |ui, r, name, acts| local_row(ui, d, r, name, 0, acts));
        });
        let mut remotes: Vec<&str> = d.remotes.iter().map(|r| r.0.as_str()).collect();
        remotes.sort();
        let remote_refs: Vec<_> = d.refs.iter().filter(|r| r.kind == RefKind::Remote && matches(f, &r.name)).collect();
        section(ui, "REMOTE", remote_refs.len(), |ui| {
            for rem in remotes {
                let url = d.remotes.iter().find(|r| r.0 == rem).map_or("", |r| r.1.as_str());
                // Headers never truncate their text themselves; hand them a galley cut to their width.
                let title = egui::WidgetText::from(rem).into_galley(ui, None, ui.available_width() - ui.spacing().indent, egui::TextStyle::Button);
                let resp = egui::CollapsingHeader::new(title).id_salt(("remote", rem)).default_open(true).show(ui, |ui| {
                    let mut list: Vec<_> = remote_refs.iter().copied().filter(|r| d.split_remote(&r.name).is_some_and(|s| s.0 == rem)).collect();
                    let n = rem.len() + 1;
                    list.sort_by_key(|r| d.branch_rank(&r.name[n..]));
                    list.retain(|r| !r.name[n..].contains('/'));
                    let names: Vec<&str> = list.iter().map(|r| &r.name[n..]).collect();
                    let mut step = 0.0;
                    for (i, r) in list.iter().copied().enumerate() {
                        pinned_separator(ui, d, i, &names);
                        in_view(ui, &mut step, |ui| remote_row(ui, d, r, &r.name[n..], acts));
                    }
                    let nested: Vec<usize> = (0..d.refs.len()).filter(|&i| d.refs[i].kind == RefKind::Remote && d.split_remote(&d.refs[i].name).is_some_and(|s| s.0 == rem) && d.refs[i].name[n..].contains('/') && matches(f, &d.refs[i].name)).collect();
                    folders::show(ui, d, ui.id().with((dir, "remote", rem)), &nested, n, f, None, acts, |ui, r, name, acts| remote_row(ui, d, r, name, acts));
                });
                let all: Vec<_> = d.refs.iter().filter(|r| r.kind == RefKind::Remote && d.split_remote(&r.name).is_some_and(|s| s.0 == rem)).collect();
                eye(ui, resp.header_response.rect, d, &all, acts);
                resp.header_response.on_hover_text(format!("{rem}\n{}", crate::git::redact(url))).context_menu(|ui| remotes::remote_menu(ui, d, rem, acts));
            }
            if d.remotes.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new("No remote yet. To publish, create an empty repository on GitHub or GitHub Enterprise, add its URL here, then Push.")
                            .small()
                            .color(theme::dim(ui)),
                    )
                    .wrap(),
                );
            }
            if theme::small(ui, true, format!("{}  Add remote…", icon::PLUS)).on_hover_text("Add a remote (a fork, upstream or mirror) and fetch it; right-click a remote for more").clicked() {
                acts.push(Action::RemoteForm(crate::repo::remotes::Form::add(d)));
            }
        });
        let tags_id = ui.id().with((dir, "tag-lists"));
        let key = std::sync::Arc::downgrade(&d.refs);
        let lists = match ui.data(|x| x.get_temp::<TagLists>(tags_id)).filter(|c| c.0.ptr_eq(&key) && c.1 == f) {
            Some(c) => c,
            None => {
                let mut tags: Vec<usize> = (0..d.refs.len()).filter(|&i| d.refs[i].kind == RefKind::Tag && matches(f, &d.refs[i].name)).collect();
                tags.sort_by_key(|&i| std::cmp::Reverse(d.refs[i].date));
                let (roots, mut nested): (Vec<usize>, Vec<usize>) = tags.iter().partition(|&&i| !d.refs[i].name.contains('/'));
                nested.sort_unstable();
                let c: TagLists = std::sync::Arc::new((key, f.to_owned(), roots, nested));
                ui.data_mut(|x| x.insert_temp(tags_id, c.clone()));
                c
            }
        };
        let (roots, nested) = (&lists.2, &lists.3);
        section(ui, "TAGS", roots.len() + nested.len(), |ui| {
            let mut step = 0.0;
            for &i in roots {
                in_view(ui, &mut step, |ui| tag_row(ui, d, &d.refs[i], &d.refs[i].name, acts));
            }
            folders::show(ui, d, ui.id().with((dir, "tags")), nested, 0, f, None, acts, |ui, r, name, acts| tag_row(ui, d, r, name, acts));
        });
        worktrees::list(ui, d, dir, f, acts);
        submodules::list(ui, d, f, acts);
        stashes::list(ui, d, sel, f, acts);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{Ref, Submodule, Worktree};
    use crate::ui::testkit::{Sim, Tab, local_ref, tab};
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn a_reload_keeps_a_menu_on_its_row() {
        let mut sim = Sim::new(300.0, 900.0);
        let mut t = tab(&sim.ctx, "sidebar-menu", &["x"]);
        let mut d = t.data.take().unwrap();
        let worktree = |n: &str| Worktree { path: format!("/wt/{n}"), branch: Some(n.into()), ..Default::default() };
        let submodule = |n: &str| Submodule { path: n.into(), head: "1".repeat(40), recorded: "1".repeat(40), flag: ' ' };
        let (before, after) = (["b1", "b2", "b3"], ["a0", "b1", "b2", "b3"]);
        let mut acts = vec![];
        let (dir, mut filter) = (t.dir.clone(), String::new());
        let mut draw = |ui: &mut Ui, d: &RepoData| sidebar(ui, d, &dir, &None, &mut filter, &mut acts);
        for (row, item) in [("b3", "Checkout b3"), ("w3", "Open in new tab"), ("s3", "Update (recursive)")] {
            d.refs = Arc::new(before.map(local_ref).to_vec());
            d.worktrees = Arc::new(before.map(|n| worktree(&n.replace('b', "w"))).to_vec());
            d.submodules = Arc::new(before.map(|n| submodule(&n.replace('b', "s"))).to_vec());
            sim.open_menu(row, |ui| draw(ui, &d));
            d.refs = Arc::new(after.map(local_ref).to_vec());
            d.worktrees = Arc::new(after.map(|n| worktree(&n.replace('b', "w"))).to_vec());
            d.submodules = Arc::new(after.map(|n| submodule(&n.replace('b', "s"))).to_vec());
            sim.pick(item, |ui| draw(ui, &d));
        }
        let picked: Vec<_> = acts.iter().filter_map(|a| match a {
            Action::Git(op) => Some(op.label.as_str()),
            Action::OpenTab(p) => Some(p.as_str()),
            _ => None,
        }).collect();
        assert_eq!(picked, ["Checkout b3", "/wt/w3", "Update (recursive) s3"]);
    }

    #[test]
    fn a_stacked_branch_with_a_slash_shows_once() {
        let mut sim = Sim::new(300.0, 600.0);
        let t = tab(&sim.ctx, "sidebar-stack-slash", &["x"]);
        let dir = t.dir.clone();
        for (b, msg) in [("base", "y"), ("feature/b", "z")] {
            crate::git::run(&dir, &["checkout", "-q", "-b", b]).unwrap();
            crate::git::run(&dir, &["commit", "-q", "--allow-empty", "-m", msg]).unwrap();
        }
        crate::git::run(&dir, &["checkout", "-q", "@{-2}"]).unwrap();
        let mut t = crate::repo::RepoTab::new(&sim.ctx, dir.clone(), 100, Default::default());
        crate::ui::testkit::load(&mut t, &sim.ctx);
        let d = t.data.take().unwrap();
        for _ in 0..3 {
            sim.frame(vec![], |ui| sidebar(ui, &d, &dir, &None, &mut String::new(), &mut vec![]));
        }
        assert_eq!(stack::parent(&d, "feature/b"), Some("base"));
        assert!(sim.text(|s| s.contains("feature/b")).is_some());
        assert!(sim.text(|s| s.contains("feature") && !s.contains("feature/b")).is_none());
    }

    fn tags_and_remote_branches(sim: &Sim, name: &str) -> (Tab, RepoData) {
        let mut t = tab(&sim.ctx, name, &["x"]);
        let mut d = t.data.take().unwrap();
        let tag = |i: usize| Ref { kind: RefKind::Tag, full: format!("refs/tags/v{i:03}"), date: 1000 - i as i64, ..local_ref(&format!("v{i:03}")) };
        let remote = |b: String| Ref { kind: RefKind::Remote, name: format!("origin/{b}"), full: format!("refs/remotes/origin/{b}"), ..local_ref(&b) };
        d.remotes = Arc::new(vec![("origin".into(), String::new())]);
        d.refs = Arc::new((0..150).map(tag).chain(["main".to_owned()].into_iter().chain((0..150).map(|i| format!("b{i:03}"))).map(remote)).collect());
        (t, d)
    }

    fn scrolled_to(ui: &mut Ui, d: &RepoData, dir: &std::path::Path, off: f32, acts: &mut Vec<Action>) {
        let id = ui.make_persistent_id(egui::IdSalt::new("scroll_area"));
        let mut state = egui::scroll_area::State::load(ui.ctx(), id).unwrap_or_default();
        state.offset.y = off;
        state.store(ui.ctx(), id);
        sidebar(ui, d, dir, &None, &mut String::new(), acts);
    }

    #[test]
    fn tag_and_remote_rows_in_view_sit_where_all_rows_would() {
        let (mut sim, mut tall) = (Sim::new(300.0, 400.0), Sim::new(300.0, 12000.0));
        let (t, d) = tags_and_remote_branches(&sim, "sidebar-rows");
        let rows = (0..150).flat_map(|i| [format!("v{i:03}"), format!("b{i:03}")]);
        let names: Vec<String> = rows.chain(["TAGS  150".into(), "STASHES  0".into()]).collect();
        let y_of = |sim: &mut Sim, off: f32| {
            sim.frame(vec![], |ui| scrolled_to(ui, &d, &t.dir, off, &mut vec![]));
            names.iter().filter_map(|n| Some((n.as_str(), sim.text(|s| s.contains(n.as_str()))?.y + off))).collect::<HashMap<_, _>>()
        };
        let all = y_of(&mut tall, 0.0);
        assert_eq!(all.len(), names.len());
        let everything = tall.ctx.fonts(|f| f.num_galleys_in_cache());
        for (row, above) in [("b000", 100.0), ("b075", 200.0), ("TAGS  150", 100.0), ("v000", 250.0), ("v120", 200.0), ("STASHES  0", 300.0)] {
            let off = all[row] - above;
            let seen = y_of(&mut sim, off);
            let laid_out = sim.ctx.fonts(|f| f.num_galleys_in_cache());
            assert!(seen.contains_key(row) && laid_out * 4 < everything, "{row}: {laid_out} of {everything} texts laid out");
            for (n, y) in seen {
                assert!((y - all[n]).abs() < 1.0, "offset {off}: {n} at {y}, not {}", all[n]);
            }
        }
    }

    #[test]
    fn local_rows_in_view_sit_where_all_rows_would() {
        let (mut sim, mut tall) = (Sim::new(300.0, 400.0), Sim::new(300.0, 12000.0));
        let mut t = tab(&sim.ctx, "sidebar-local-rows", &["x"]);
        let mut d = t.data.take().unwrap();
        let names: Vec<String> = ["develop".to_owned()].into_iter().chain((0..150).map(|i| format!("b{i:03}"))).chain(["TAGS  0".into()]).collect();
        d.refs = Arc::new(names[..151].iter().map(|n| Ref { head: n == "b040", ..local_ref(n) }).collect());
        let y_of = |sim: &mut Sim, off: f32| {
            sim.frame(vec![], |ui| scrolled_to(ui, &d, &t.dir, off, &mut vec![]));
            names.iter().filter_map(|n| Some((n.as_str(), sim.text(|s| s.ends_with(n.as_str()))?.y + off))).collect::<HashMap<_, _>>()
        };
        let all = y_of(&mut tall, 0.0);
        assert_eq!(all.len(), names.len());
        let everything = tall.ctx.fonts(|f| f.num_galleys_in_cache());
        for (row, above) in [("b000", 100.0), ("b041", 100.0), ("b100", 200.0), ("TAGS  0", 300.0)] {
            let off = all[row] - above;
            let seen = y_of(&mut sim, off);
            let laid_out = sim.ctx.fonts(|f| f.num_galleys_in_cache());
            assert!(seen.contains_key(row) && laid_out * 4 < everything, "{row}: {laid_out} of {everything} texts laid out");
            for (n, y) in seen {
                assert!((y - all[n]).abs() < 1.0, "offset {off}: {n} at {y}, not {}", all[n]);
            }
        }
    }

    #[test]
    fn menu_stays_with_its_tag_or_remote_branch_when_the_list_scrolls() {
        let mut tall = Sim::new(300.0, 12000.0);
        let (t, d) = tags_and_remote_branches(&tall, "sidebar-row-menus");
        tall.frame(vec![], |ui| scrolled_to(ui, &d, &t.dir, 0.0, &mut vec![]));
        for (row, item) in [("b075", "Checkout origin/b075"), ("v075", "Checkout v075")] {
            let base = tall.text(|s| s.ends_with(row)).unwrap().y - 200.0;
            let mut sim = Sim::new(300.0, 400.0);
            let draw = |ui: &mut Ui, scrolled: bool| scrolled_to(ui, &d, &t.dir, base + if scrolled { 60.0 } else { 0.0 }, &mut vec![]);
            sim.open_menu(row, |ui| draw(ui, false));
            for _ in 0..2 {
                sim.frame(vec![], |ui| draw(ui, true));
            }
            assert!(egui::Popup::is_any_open(&sim.ctx), "{row}: the menu closed");
            assert!(sim.text(|s| s.starts_with(item)).is_some(), "{row}: the open menu is another row's");
        }
    }
}
