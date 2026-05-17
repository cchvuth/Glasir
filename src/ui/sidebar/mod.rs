//! Left sidebar: filter box, local / remote branches, tags, worktrees and stashes.
//! `ref_menus` holds the right-click menus of refs (shared with the graph pills).

pub mod ref_menus;
pub mod stashes;
mod worktrees;

use crate::git::RefKind;
use crate::repo::{Action, RepoData, branch_checkout};
use crate::theme::{self, icon};
use crate::ui::graph_view::dnd::branch_dnd;
use crate::ui::graph_view::hover::hover_ref;
use egui::{RichText, ScrollArea, TextEdit, Ui};
use ref_menus::ref_menu;

fn matches(filter: &str, name: &str) -> bool {
    filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase())
}

/// Hidden refs are dimmed with an eye-slash, soloed ones orange.
fn view_style(ui: &Ui, d: &RepoData, r: &crate::git::Ref, text: RichText) -> RichText {
    if d.view.solo.contains(&r.full) {
        text.color(theme::MOD)
    } else if !d.view.shows(&r.full) {
        RichText::new(format!("{}  {}", text.text(), icon::EYE_SLASH)).color(theme::dim(ui))
    } else {
        text
    }
}

/// A thin line between the pinned default/long-lived branches and the rest.
fn pinned_separator(ui: &mut Ui, d: &RepoData, i: usize, names: &[&str]) {
    if i > 0 && d.branch_rank(names[i - 1]) < 2 && d.branch_rank(names[i]) == 2 {
        ui.separator();
    }
}

fn section(ui: &mut Ui, title: &str, count: usize, add: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(format!("{title}  {count}")).small().strong())
        .id_salt(title)
        .default_open(true)
        .show(ui, add);
}

pub fn sidebar(ui: &mut Ui, d: &RepoData, filter: &mut String, acts: &mut Vec<Action>) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add(TextEdit::singleline(filter).hint_text(format!("{} Filter", icon::MAGNIFYING_GLASS)).desired_width(ui.available_width() - 36.0));
        if theme::pill(ui, icon::BROOM).on_hover_text("Clean up merged & gone branches").clicked() {
            acts.push(Action::Cleanup);
        }
    });
    ui.add_space(4.0);
    let f = filter.as_str();
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        let mut locals: Vec<_> = d.refs.iter().filter(|r| r.kind == RefKind::Local && matches(f, &r.name)).collect();
        locals.sort_by_key(|r| d.branch_rank(&r.name)); // stable: A–Z within each group
        section(ui, "LOCAL", locals.len(), |ui| {
            for (i, r) in locals.iter().copied().enumerate() {
                pinned_separator(ui, d, i, &locals.iter().map(|r| r.name.as_str()).collect::<Vec<_>>());
                let glyph = if r.head { icon::CHECK } else if r.worktree.is_some() { icon::TREE_STRUCTURE } else { icon::GIT_BRANCH };
                let mut text = format!("{glyph} {}", r.name);
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
                let resp = ui.add(egui::Button::selectable(r.head, view_style(ui, d, r, rt)).frame_when_inactive(false));
                branch_dnd(ui, &resp, &r.name, acts);
                hover_ref(ui, &resp, &r.target);
                let resp = if r.gone { resp.on_hover_text("Upstream branch is gone") } else { resp };
                if resp.clicked() {
                    acts.push(Action::Jump(r.target.clone()));
                }
                if resp.double_clicked() {
                    acts.extend(branch_checkout(d, r));
                }
                resp.context_menu(|ui| ref_menu(ui, d, r, acts));
            }
        });
        let mut remotes: Vec<&str> = d.remotes.iter().map(|r| r.0.as_str()).collect();
        remotes.sort();
        let remote_refs: Vec<_> = d.refs.iter().filter(|r| r.kind == RefKind::Remote && matches(f, &r.name)).collect();
        section(ui, "REMOTE", remote_refs.len(), |ui| {
            for rem in remotes {
                let url = d.remotes.iter().find(|r| r.0 == rem).map_or("", |r| r.1.as_str());
                let resp = egui::CollapsingHeader::new(rem).id_salt(("remote", rem)).default_open(true).show(ui, |ui| {
                    let mut list: Vec<_> = remote_refs.iter().copied().filter(|r| r.remote() == rem).collect();
                    list.sort_by_key(|r| d.branch_rank(r.remote_branch()));
                    let names: Vec<&str> = list.iter().map(|r| r.remote_branch()).collect();
                    for (i, r) in list.iter().copied().enumerate() {
                        pinned_separator(ui, d, i, &names);
                        let text = view_style(ui, d, r, RichText::new(r.remote_branch()));
                        let resp = ui.add(egui::Button::selectable(false, text).frame_when_inactive(false));
                        branch_dnd(ui, &resp, &r.name, acts);
                        hover_ref(ui, &resp, &r.target);
                        if resp.clicked() {
                            acts.push(Action::Jump(r.target.clone()));
                        }
                        if resp.double_clicked() {
                            acts.extend(branch_checkout(d, r));
                        }
                        resp.context_menu(|ui| ref_menu(ui, d, r, acts));
                    }
                });
                resp.header_response.on_hover_text(url);
            }
        });
        let mut tags: Vec<_> = d.refs.iter().filter(|r| r.kind == RefKind::Tag && matches(f, &r.name)).collect();
        tags.sort_by_key(|r| std::cmp::Reverse(r.date));
        section(ui, "TAGS", tags.len(), |ui| {
            for r in tags {
                let resp = ui.add(egui::Button::selectable(false, view_style(ui, d, r, RichText::new(&r.name))).frame_when_inactive(false));
                hover_ref(ui, &resp, &r.target);
                if resp.clicked() {
                    acts.push(Action::Jump(r.target.clone()));
                }
                resp.context_menu(|ui| ref_menu(ui, d, r, acts));
            }
        });
        worktrees::list(ui, d, f, acts);
        stashes::list(ui, d, f, acts);
    });
}
