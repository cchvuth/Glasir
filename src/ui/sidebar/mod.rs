//! Left sidebar: filter box, local / remote branches, tags, worktrees, submodules and stashes.
//! `ref_menus` holds the right-click menus of refs (shared with the graph pills),
//! `remotes` a remote's menu and the add / edit remote dialog.

pub mod ref_menus;
pub mod remotes;
pub mod stashes;
mod folders;
mod submodules;
mod worktrees;

use crate::git::RefKind;
use crate::repo::{Action, RepoData, Sel, branch_checkout, stack};
use crate::theme::{self, icon};
use crate::ui::graph_view::dnd::branch_dnd;
use crate::ui::graph_view::hover::hover_ref;
use egui::{RichText, ScrollArea, TextEdit, Ui};
use ref_menus::ref_menu;

fn matches(filter: &str, name: &str) -> bool {
    filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase())
}

/// Hidden refs are dimmed, soloed ones orange.
fn view_style(ui: &Ui, d: &RepoData, r: &crate::git::Ref, text: RichText) -> RichText {
    if d.view.solo.contains(&r.full) {
        text.color(theme::MOD)
    } else if !d.view.shows(&r.full) {
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
    let hidden = refs.iter().all(|f| d.view.hidden.iter().any(|h| h == f));
    let at = egui::Rect::from_center_size(egui::pos2(row.right() - 14.0, row.center().y), egui::vec2(26.0, 22.0));
    let b = egui::Button::new(if hidden { icon::EYE } else { icon::EYE_SLASH }).frame_when_inactive(false);
    if ui.put(at, b).on_hover_text(if hidden { "Show in graph" } else { "Hide from graph" }).clicked() {
        let mut v = d.view.clone();
        v.hidden.retain(|h| !refs.contains(&h.as_str()));
        if !hidden {
            v.hidden.extend(refs.iter().map(|f| f.to_string()));
        }
        acts.push(Action::View(v));
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
    egui::Button::new(text.color(theme::on_color(a))).fill(a).stroke(egui::Stroke::new(1.0, egui::Color32::from_white_alpha(90)))
}

/// Stacked branches right under their parent, with their depth (`repo::stack`).
fn stacked<'a>(d: &'a RepoData, locals: &[&'a crate::git::Ref]) -> Vec<(&'a crate::git::Ref, usize)> {
    fn add<'a>(d: &'a RepoData, r: &'a crate::git::Ref, depth: usize, out: &mut Vec<(&'a crate::git::Ref, usize)>) {
        out.push((r, depth));
        for c in stack::children(d, &r.name).into_iter().filter_map(|c| d.local(c)) {
            if depth < 16 && !out.iter().any(|o| o.0.name == c.name) {
                add(d, c, depth + 1, out);
            }
        }
    }
    let mut out = vec![];
    for r in locals.iter().filter(|r| stack::parent(d, &r.name).is_none_or(|p| d.local(p).is_none())) {
        add(d, r, 0, &mut out);
    }
    // A loop of stale edges has no root: never lose a branch from the list.
    for r in locals {
        if !out.iter().any(|o| o.0.name == r.name) {
            out.push((r, 0));
        }
    }
    out
}

/// A local branch row: marker, stack / ahead / behind badges, menus, drag-and-drop. `name` is what
/// it shows (the last segment inside a folder); `depth` its stack depth.
fn local_row(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, name: &str, depth: usize, acts: &mut Vec<Action>) {
    let glyph = if r.head { icon::CHECK } else if r.worktree.is_some() { icon::TREE_STRUCTURE } else { icon::GIT_BRANCH };
    let indent = if depth > 0 { format!("{}{} ", "    ".repeat(depth - 1), icon::ARROW_ELBOW_DOWN_RIGHT) } else { String::new() };
    let mut text = format!("{indent}{glyph}  {name}");
    if depth == 0 && !stack::children(d, &r.name).is_empty() {
        text += &format!("  {}", icon::STACK);
    }
    if stack::stale(d, &r.name) {
        text += &format!("  {}", icon::ARROWS_CLOCKWISE);
    }
    if r.ahead > 0 {
        text += &format!("  ↑{}", r.ahead);
    }
    if r.behind > 0 {
        text += &format!("  ↓{}", r.behind);
    }
    let mut rt = RichText::new(text);
    if r.head {
        rt = rt.strong();
    }
    let styled = d.view.solo.contains(&r.full) || !d.view.shows(&r.full);
    let rt = view_style(ui, d, r, rt);
    let resp = ui.add(if r.head { head_row(ui, rt, styled) } else { row(ui, false, rt) });
    branch_dnd(ui, &resp, &r.name, &r.full, acts);
    hover_ref(ui, &resp, &r.target);
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
    resp.context_menu(|ui| ref_menu(ui, d, r, acts));
}

fn remote_row(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, name: &str, acts: &mut Vec<Action>) {
    let text = view_style(ui, d, r, RichText::new(format!("{}  {name}", icon::CLOUD)));
    let resp = ui.add(egui::Button::selectable(false, text).frame_when_inactive(false)).on_hover_text(&r.name);
    branch_dnd(ui, &resp, &r.name, &r.full, acts);
    hover_ref(ui, &resp, &r.target);
    eye(ui, resp.rect, d, &[r], acts);
    if resp.clicked() {
        acts.push(Action::Jump(r.target.clone()));
    }
    if resp.double_clicked() {
        acts.extend(branch_checkout(d, r));
    }
    resp.context_menu(|ui| ref_menu(ui, d, r, acts));
}

fn tag_row(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, name: &str, acts: &mut Vec<Action>) {
    let resp = ui.add(egui::Button::selectable(false, view_style(ui, d, r, RichText::new(format!("{}  {name}", icon::TAG)))).frame_when_inactive(false));
    hover_ref(ui, &resp, &r.target);
    eye(ui, resp.rect, d, &[r], acts);
    let resp = resp.on_hover_text(if r.message.is_empty() { r.name.clone() } else { format!("{}\n\n{}", r.name, r.message) });
    if resp.clicked() {
        acts.push(Action::Jump(r.target.clone()));
    }
    resp.context_menu(|ui| ref_menu(ui, d, r, acts));
}

fn section(ui: &mut Ui, title: &str, count: usize, add: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(format!("{title}  {count}")).small().strong())
        .id_salt(title)
        .default_open(true)
        .show(ui, add);
}

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
            for (i, (r, depth)) in rows.iter().copied().enumerate() {
                pinned_separator(ui, d, i, &names);
                local_row(ui, d, r, &r.name, depth, acts);
            }
            let nested: Vec<usize> = (0..d.refs.len()).filter(|&i| d.refs[i].kind == RefKind::Local && d.refs[i].name.contains('/') && matches(f, &d.refs[i].name)).collect();
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
                    for (i, r) in list.iter().copied().enumerate() {
                        pinned_separator(ui, d, i, &names);
                        remote_row(ui, d, r, &r.name[n..], acts);
                    }
                    let nested: Vec<usize> = (0..d.refs.len()).filter(|&i| d.refs[i].kind == RefKind::Remote && d.split_remote(&d.refs[i].name).is_some_and(|s| s.0 == rem) && d.refs[i].name[n..].contains('/') && matches(f, &d.refs[i].name)).collect();
                    folders::show(ui, d, ui.id().with((dir, "remote", rem)), &nested, n, f, None, acts, |ui, r, name, acts| remote_row(ui, d, r, name, acts));
                });
                let all: Vec<_> = d.refs.iter().filter(|r| r.kind == RefKind::Remote && d.split_remote(&r.name).is_some_and(|s| s.0 == rem)).collect();
                eye(ui, resp.header_response.rect, d, &all, acts);
                resp.header_response.on_hover_text(format!("{rem}\n{url}")).context_menu(|ui| remotes::remote_menu(ui, d, rem, acts));
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
        let mut tags: Vec<usize> = (0..d.refs.len()).filter(|&i| d.refs[i].kind == RefKind::Tag && matches(f, &d.refs[i].name)).collect();
        tags.sort_by_key(|&i| std::cmp::Reverse(d.refs[i].date));
        section(ui, "TAGS", tags.len(), |ui| {
            let (roots, mut nested): (Vec<usize>, Vec<usize>) = tags.iter().partition(|&&i| !d.refs[i].name.contains('/'));
            for i in roots {
                tag_row(ui, d, &d.refs[i], &d.refs[i].name, acts);
            }
            nested.sort_unstable();
            folders::show(ui, d, ui.id().with((dir, "tags")), &nested, 0, f, None, acts, |ui, r, name, acts| tag_row(ui, d, r, name, acts));
        });
        worktrees::list(ui, d, dir, f, acts);
        submodules::list(ui, d, f, acts);
        stashes::list(ui, d, sel, f, acts);
    });
}
