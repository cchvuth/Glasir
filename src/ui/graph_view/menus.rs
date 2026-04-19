//! Context menus of the graph rows (commit, WIP).

use crate::git::{Commit, Ref, RefKind};
use crate::repo::{Action, Op, RepoData, branch_checkout};
use egui::Ui;

/// Double-click: check out the best branch at this commit.
pub(super) fn checkout_action(d: &RepoData, c: &Commit) -> Option<Action> {
    let refs = d.refs_at.get(&c.id)?;
    let r = refs.iter().map(|&i| &d.refs[i]).find(|r| r.kind != RefKind::Tag)?;
    branch_checkout(d, r)
}

pub fn commit_menu(ui: &mut Ui, d: &RepoData, c: &Commit, acts: &mut Vec<Action>) {
    let short = &c.id[..c.id.len().min(7)];
    let refs: Vec<&Ref> = d.refs_at.get(&c.id).map(|v| v.iter().map(|&i| &d.refs[i]).collect()).unwrap_or_default();
    for r in refs.iter().filter(|r| r.kind != RefKind::Tag && !r.head) {
        if ui.button(format!("Checkout {}", r.name)).clicked()
            && let Some(a) = branch_checkout(d, r)
        {
            acts.push(a);
        }
    }
    if ui.button("Checkout this commit (detached)").clicked() {
        acts.push(Action::Git(Op::new(format!("Checkout {short}"), &["checkout", "--detach", &c.id])));
    }
    ui.separator();
    if ui.button("Copy commit SHA").clicked() {
        acts.push(Action::Copy(c.id.clone()));
    }
    if ui.button("Copy commit message").clicked() {
        acts.push(Action::Copy(c.summary.clone()));
    }
}

pub(super) fn wip_menu(ui: &mut Ui, acts: &mut Vec<Action>) {
    if ui.button("Discard all changes").clicked() {
        acts.push(Action::Git(
            Op::new("Discard all", &["reset", "--hard", "-q"])
                .then(&["clean", "-fdq"])
                .confirm("Discard ALL uncommitted changes, including untracked files?"),
        ));
    }
}
