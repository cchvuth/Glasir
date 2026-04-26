//! Left sidebar: filter box, local / remote branches, and tags.
//! `ref_menus` holds the right-click menus of refs (shared with the graph pills).

pub mod ref_menus;

use crate::git::RefKind;
use crate::repo::{Action, RepoData, branch_checkout};
use crate::theme::icon;
use crate::ui::graph_view::dnd::branch_dnd;
use egui::{RichText, ScrollArea, TextEdit, Ui};
use ref_menus::ref_menu;

fn matches(filter: &str, name: &str) -> bool {
    filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase())
}

fn section(ui: &mut Ui, title: &str, count: usize, add: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(format!("{title}  {count}")).small().strong())
        .id_salt(title)
        .default_open(true)
        .show(ui, add);
}

pub fn sidebar(ui: &mut Ui, d: &RepoData, filter: &mut String, acts: &mut Vec<Action>) {
    ui.add_space(4.0);
    ui.add(TextEdit::singleline(filter).hint_text(format!("{} Filter", icon::MAGNIFYING_GLASS)).desired_width(f32::INFINITY));
    ui.add_space(4.0);
    let f = filter.as_str();
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        let locals: Vec<_> = d.refs.iter().filter(|r| r.kind == RefKind::Local && matches(f, &r.name)).collect();
        section(ui, "LOCAL", locals.len(), |ui| {
            for r in locals {
                let glyph = if r.head { icon::CHECK } else { icon::GIT_BRANCH };
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
                let resp = ui.add(egui::Button::selectable(r.head, rt).frame_when_inactive(false));
                branch_dnd(ui, &resp, &r.name, acts);
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
                egui::CollapsingHeader::new(rem).id_salt(("remote", rem)).default_open(true).show(ui, |ui| {
                    for r in remote_refs.iter().copied().filter(|r| r.remote() == rem) {
                        let resp = ui.add(egui::Button::selectable(false, r.remote_branch()).frame_when_inactive(false));
                        branch_dnd(ui, &resp, &r.name, acts);
                        if resp.clicked() {
                            acts.push(Action::Jump(r.target.clone()));
                        }
                        if resp.double_clicked() {
                            acts.extend(branch_checkout(d, r));
                        }
                        resp.context_menu(|ui| ref_menu(ui, d, r, acts));
                    }
                });
            }
        });
        let mut tags: Vec<_> = d.refs.iter().filter(|r| r.kind == RefKind::Tag && matches(f, &r.name)).collect();
        tags.sort_by_key(|r| std::cmp::Reverse(r.date));
        section(ui, "TAGS", tags.len(), |ui| {
            for r in tags {
                let resp = ui.add(egui::Button::selectable(false, &r.name).frame_when_inactive(false));
                if resp.clicked() {
                    acts.push(Action::Jump(r.target.clone()));
                }
                resp.context_menu(|ui| ref_menu(ui, d, r, acts));
            }
        });
    });
}
