//! The WORKTREES section of the sidebar: one row per worktree with its menu.

use super::{matches, section};
use crate::repo::{Action, Op, Prompt, RepoData, op, prompt};
use crate::theme::{self, icon};
use egui::{RichText, Ui};

/// The section: worktree rows, then "New worktree…" / "Prune stale".
pub(super) fn list(ui: &mut Ui, d: &RepoData, dir: &std::path::Path, f: &str, acts: &mut Vec<Action>) {
    let wts: Vec<_> = d.worktrees.iter().filter(|w| !w.bare && (matches(f, &w.path) || w.branch.as_deref().is_some_and(|b| matches(f, b)))).collect();
    section(ui, "WORKTREES", wts.len(), |ui| {
        // By path: detached worktrees at the same commit look alike. HEAD + branch only as a fallback.
        let here = (d.worktrees.iter().find(|w| std::path::Path::new(&w.path) == dir))
            .or_else(|| d.worktrees.iter().find(|w| d.status.head.as_deref() == Some(w.head.as_str()) && w.branch == d.status.branch))
            .map(|w| w.path.clone());
        for w in wts {
            worktree_row(ui, d, w, here.as_deref() == Some(w.path.as_str()), acts);
        }
        ui.horizontal_wrapped(|ui| {
            if theme::small(ui, true, format!("{}  New worktree…", icon::PLUS)).on_hover_text("New branch from HEAD in its own folder").clicked() {
                acts.push(prompt(
                    "New branch and worktree from HEAD",
                    "",
                    Op::new("Create worktree", &["worktree", "add", "-b", "{}", &d.worktree_path(Prompt::VALUE), "HEAD"]).then_open(d.worktree_path(Prompt::VALUE)),
                ));
            }
            if d.worktrees.iter().any(|w| w.prunable) && theme::small(ui, true, "Prune stale").clicked() {
                let stale: Vec<&str> = d.worktrees.iter().filter(|w| w.prunable).map(|w| w.path.as_str()).collect();
                let warn = format!("Forget {} worktree(s) whose folder is missing?\n{}\nIf one is only on an unplugged drive, pruning drops its HEAD, index and any rebase in progress.", stale.len(), stale.join("\n"));
                acts.push(Action::Git(Op::new("Prune worktrees", &["worktree", "prune"]).confirm(warn)));
            }
        });
    });
}

fn worktree_row(ui: &mut Ui, d: &RepoData, w: &crate::git::Worktree, current: bool, acts: &mut Vec<Action>) {
    let name = w.branch.clone().unwrap_or_else(|| format!("detached @ {}", &w.head[..w.head.len().min(7)]));
    let mut label = format!("{}  {name}", if current { icon::CHECK } else { icon::TREE_STRUCTURE });
    if w.changes > 0 {
        label += &format!("  {}{}", icon::PENCIL_SIMPLE, w.changes);
    }
    if w.locked.is_some() {
        label += &format!("  {}", icon::LOCK_SIMPLE);
    }
    let mut text = RichText::new(label);
    if w.prunable {
        text = text.strikethrough().color(theme::dim(ui));
    }
    let tip = format!(
        "{}{}{}",
        w.path,
        w.locked.as_ref().map_or(String::new(), |r| format!("\nLocked{}", if r.is_empty() { String::new() } else { format!(": {r}") })),
        if w.prunable { "\nMissing on disk (prune to clean up)" } else { "" }
    );
    let resp = theme::keyed(ui, ("worktree", &w.path), |ui| ui.add(if current { super::head_row(ui, text, false) } else { super::row(ui, false, text) })).on_hover_text(tip);
    if (resp.double_clicked() || resp.clicked()) && !current && !w.prunable {
        acts.push(Action::Switch(w.path.clone()));
    }
    resp.context_menu(|ui| {
        if !current && !w.prunable && ui.button("Open in new tab").clicked() {
            acts.push(Action::OpenTab(w.path.clone()));
        }
        if !w.prunable && ui.button("Show in file manager").clicked() {
            crate::repo::reveal(std::path::Path::new(&w.path));
        }
        match &w.locked {
            Some(_) => {
                if ui.button("Unlock").clicked() {
                    acts.push(op("Unlock worktree", &["worktree", "unlock", &w.path]));
                }
            }
            None => {
                if ui.button("Lock…").on_hover_text("Protect from pruning (e.g. on a removable drive)").clicked() {
                    acts.push(prompt("Lock reason (optional)", "in use", Op::new("Lock worktree", &["worktree", "lock", "--reason", "{}", &w.path])));
                }
            }
        }
        let is_main = d.worktrees.first().is_some_and(|m| m.path == w.path);
        if !current && !is_main && w.locked.is_none() {
            ui.separator();
            let (force, warn) = if w.changes > 0 {
                (true, format!("{} has {} uncommitted change(s) that will be LOST.", w.path, w.changes))
            } else {
                (false, format!("Remove the worktree folder {}?", w.path))
            };
            let mut args = vec!["worktree", "remove"];
            if force {
                args.push("--force");
            }
            args.push(&w.path);
            if ui.button("Remove worktree").clicked() {
                acts.push(Action::Git(crate::repo::remove_worktree_op(w, &args, &warn)));
            }
            if let Some(b) = &w.branch
                && ui.button(format!("Remove worktree and delete {b}")).clicked()
            {
                let op = crate::repo::remove_worktree_and_branch(d, &args, b);
                acts.push(Action::Git(op.confirm(format!("{warn}\nBranch {b} is deleted too (undoable)."))));
            }
        }
        ui.separator();
        if ui.button("Copy path").clicked() {
            acts.push(Action::Copy(w.path.clone()));
        }
    });
}
