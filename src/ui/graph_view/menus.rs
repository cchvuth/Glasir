//! Context menus of the graph rows (commit, WIP, multi-selection) and the
//! merge / rebase entries (`integrate_items`) shared with the sidebar.

use crate::git::{Commit, Ref, RefKind};
use crate::repo::{Action, MultiOp, Op, Prompt, RepoData, Sel, Undo, branch_checkout, prompt, stash_op};
use crate::theme::{self, icon};
use egui::Ui;

pub(super) fn multi_menu(ui: &mut Ui, multi: &[String], acts: &mut Vec<Action>) {
    let n = multi.len();
    for (kind, label) in [
        (MultiOp::CherryPick, format!("Cherry-pick {n} commits")),
        (MultiOp::Squash, format!("Squash {n} commits into one")),
        (MultiOp::Revert, format!("Revert {n} commits")),
    ] {
        if ui.button(label).clicked() {
            acts.push(Action::MultiOp(kind, multi.to_vec()));
        }
    }
    if ui.button(egui::RichText::new(format!("Drop {n} commits")).color(theme::DEL)).clicked() {
        acts.push(Action::MultiOp(MultiOp::Drop, multi.to_vec()));
    }
    ui.separator();
    if ui.button("Compare oldest ↔ newest").clicked() {
        acts.push(Action::Select(Sel::Compare { from: format!("{}^", multi[n - 1]), to: multi[0].clone(), three_dot: false }));
    }
    if ui.button("Copy SHAs").clicked() {
        acts.push(Action::Copy(multi.join("\n")));
    }
}
/// Double-click: check out the best branch at this commit.
pub(super) fn checkout_action(d: &RepoData, c: &Commit) -> Option<Action> {
    let refs = d.refs_at.get(&c.id)?;
    let r = refs.iter().map(|&i| &d.refs[i]).find(|r| r.kind != RefKind::Tag)?;
    branch_checkout(d, r)
}

/// Merge/rebase/cherry-pick style actions targeting `target` (branch name or commit id).
pub fn integrate_items(ui: &mut Ui, d: &RepoData, target: &str, acts: &mut Vec<Action>) {
    let Some(head) = d.status.branch.as_deref() else { return };
    if target == head {
        return;
    }
    if ui.button(format!("Merge {target} into {head}")).clicked() {
        acts.push(Action::Git(Op::new(format!("Merge {target}"), &["merge", "--no-edit", target]).undo(Undo::Keep)));
    }
    if ui.button(format!("Rebase {head} onto {target}")).clicked() {
        acts.push(Action::Git(Op::new(format!("Rebase onto {target}"), &["rebase", target]).undo(Undo::Keep)));
    }
    if ui.button(format!("Interactive rebase {head} onto {target}…")).clicked() {
        acts.push(Action::Rebase { base: target.into(), label: target.into() });
    }
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
        acts.push(Action::Git(Op::new(format!("Checkout {short}"), &["checkout", "--detach", &c.id]).undo(Undo::Keep)));
    }
    ui.separator();
    if ui.button("Create branch here…").clicked() {
        acts.push(prompt(
            format!("New branch at {short}"),
            "",
            Op::new("Create branch", &["checkout", "-b", "{}", &c.id]).undo(Undo::Keep),
        ));
    }
    if ui.button("Create worktree here (new branch)…").clicked() {
        acts.push(prompt(
            format!("New branch and worktree at {short}"),
            "",
            Op::new("Create worktree", &["worktree", "add", "-b", "{}", &d.worktree_path("{}"), &c.id]).then_open(d.worktree_path("{}")),
        ));
    }
    if ui.button("Create tag here…").clicked() {
        acts.push(prompt(format!("New tag at {short}"), "", Op::new("Create tag", &["tag", "{}", &c.id]).undo(Undo::Keep)));
    }
    if ui.button("Create annotated tag here…").clicked() {
        acts.push(Action::Prompt(Prompt {
            title: format!("Annotated tag at {short}"),
            value: String::new(),
            op: Op::new("Create tag", &["tag", "-a", "{}", "-m", "{2}", &c.id]).undo(Undo::Keep),
            second: Some(("Tag message (⌘↩ to create)".into(), String::new())),
        }));
    }
    ui.separator();
    let target = refs.iter().find(|r| r.kind != RefKind::Tag).map_or(c.id.as_str(), |r| r.name.as_str());
    let is_head = d.status.head.as_deref() == Some(&c.id);
    if !is_head {
        integrate_items(ui, d, target, acts);
        let mut pick = vec!["cherry-pick"];
        if c.parents.len() > 1 {
            pick.extend(["-m", "1"]);
        }
        pick.push(&c.id);
        if ui.button("Cherry-pick commit").clicked() {
            acts.push(Action::Git(Op::new(format!("Cherry-pick {short}"), &pick).undo(Undo::Keep)));
        }
    }
    if d.status.branch.is_some() && !is_head && ui.button("Edit history after this commit (interactive rebase)…").clicked() {
        acts.push(Action::Rebase { base: c.id.clone(), label: short.into() });
    }
    if d.status.branch.is_some() && c.parents.len() == 1 {
        ui.menu_button(format!("{} Rewrite this commit", icon::PENCIL_SIMPLE), |ui| {
            let staged = !d.status.staged.is_empty();
            if ui
                .add_enabled(staged, egui::Button::new("Amend staged changes into this commit"))
                .on_disabled_hover_text("Stage some changes first")
                .clicked()
            {
                acts.push(Action::AmendInto(c.id.clone()));
                ui.close();
            }
            if ui.button("Edit message…").clicked() {
                // Placeholder op: RepoTab::run turns the new summary into a reword (keeping the body).
                let op = Op::new(format!("Reword {short}"), &["__reword__", &c.id, "{}"]);
                acts.push(prompt(format!("New message for {short}"), c.summary.clone(), op));
                ui.close();
            }
            if ui.button("Squash into parent").clicked() {
                acts.push(Action::Rewrite { commit: c.id.clone(), act: crate::repo::rebase::Act::Squash, message: None });
                ui.close();
            }
            if ui.button(egui::RichText::new("Drop commit").color(theme::DEL)).clicked() {
                acts.push(Action::Rewrite { commit: c.id.clone(), act: crate::repo::rebase::Act::Drop, message: None });
                ui.close();
            }
        });
    }
    let mut revert = vec!["revert", "--no-edit"];
    if c.parents.len() > 1 {
        revert.extend(["-m", "1"]);
    }
    revert.push(&c.id);
    if ui.button("Revert commit").clicked() {
        acts.push(Action::Git(Op::new(format!("Revert {short}"), &revert).undo(Undo::Keep)));
    }
    if let Some(head) = d.status.branch.as_deref().filter(|_| !is_head) {
        ui.menu_button(format!("Reset {head} to this commit"), |ui| {
            for (mode, undo, hint) in [
                ("--soft", Undo::Soft, "keep all changes staged"),
                ("--mixed", Undo::Mixed, "keep changes in working directory"),
                ("--hard", Undo::Keep, "discard all changes"),
            ] {
                if ui.button(format!("{} — {hint}", &mode[2..])).clicked() {
                    let mut o = Op::new(format!("Reset {}", &mode[2..]), &["reset", mode, &c.id]).undo(undo);
                    if mode == "--hard" {
                        o = o.confirm(format!("Hard reset {head} to {short}? Uncommitted changes will be lost."));
                    }
                    acts.push(Action::Git(o));
                }
            }
        });
    }
    ui.separator();
    if ui.button("Copy commit SHA").clicked() {
        acts.push(Action::Copy(c.id.clone()));
    }
    if ui.button("Copy commit message").clicked() {
        acts.push(Action::Copy(c.summary.clone()));
    }
    if !is_head && ui.button("Compare with HEAD").clicked() {
        acts.push(Action::Select(Sel::Compare { from: c.id.clone(), to: "HEAD".into(), three_dot: false }));
    }
    if let Some(web) = d.web() {
        if ui.button("Open commit on remote").clicked() {
            acts.push(Action::Url(format!("{web}/commit/{}", c.id)));
        }
        if ui.button("Copy link to commit").clicked() {
            acts.push(Action::Copy(format!("{web}/commit/{}", c.id)));
        }
    }
}

pub(super) fn wip_menu(ui: &mut Ui, acts: &mut Vec<Action>) {
    if ui.button("Stash all changes").clicked() {
        acts.push(Action::Git(stash_op()));
    }
    if ui.button("Discard all changes").clicked() {
        acts.push(Action::Git(
            Op::new("Discard all", &["reset", "--hard", "-q"])
                .then(&["clean", "-fdq"])
                .backup(crate::repo::Backup::All)
                .confirm("Discard ALL uncommitted changes, including untracked files? (Undo with ⌘Z.)"),
        ));
    }
}
