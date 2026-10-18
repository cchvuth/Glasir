//! Context menus of the graph rows (commit, WIP, multi-selection) and the
//! merge / rebase entries (`integrate_items`) shared with the sidebar.

use crate::ui::format::keys;
use crate::git::{Commit, Ref, RefKind, State};
use crate::repo::{Action, MultiOp, Op, Prompt, RepoData, Search, Sel, Undo, branch_checkout, prompt, stash_prompt};
use crate::repo::merge_opts::{Merge, merge_op, pick_op};
use crate::theme::{self, icon};
use egui::Ui;

/// `d`: the full history; `shown`: the graph on screen (the search hits while filtering, which also
/// holds hits older than `d`'s loaded page).
pub(super) fn multi_menu(ui: &mut Ui, d: &RepoData, shown: &RepoData, multi: &[String], acts: &mut Vec<Action>) {
    let n = multi.len();
    for (kind, label) in [
        (MultiOp::CherryPick, format!("Cherry-pick {n} commits")),
        (MultiOp::CherryPickEdit, format!("{}  Cherry-pick interactively…", icon::CHERRIES)),
        (MultiOp::Squash, format!("Squash {n} commits into one")),
        (MultiOp::Revert, format!("Revert {n} commits")),
    ] {
        if ui.button(label).clicked() {
            acts.push(Action::MultiOp(kind, multi.to_vec()));
        }
    }
    if ui.button(egui::RichText::new(format!("Drop {n} commits")).color(theme::del())).clicked() {
        acts.push(Action::MultiOp(MultiOp::Drop, multi.to_vec()));
    }
    recompose_items(ui, d, &format!("Recompose {n} commits…"), &multi[n - 1], multi, acts);
    patch_items(ui, multi, acts);
    ui.separator();
    if ui.button("Compare oldest ↔ newest").clicked() {
        acts.push(Action::Select(Sel::Compare { from: format!("{}^", multi[n - 1]), to: multi[0].clone(), three_dot: false }));
    }
    if ui.button("Copy SHAs").clicked() {
        acts.push(Action::Copy(multi.join("\n")));
    }
    // GitHub's base...head leaves the base out: compare from the oldest one's parent (none for a root).
    if let Some(base) = d.first_parent(&multi[n - 1]).or_else(|| shown.first_parent(&multi[n - 1])) {
        let page = crate::git::Page::Compare { base, head: &multi[0] };
        crate::ui::links::items(ui, d, &[("compare", page)], &[&multi[n - 1], &multi[0]], acts);
    }
    crate::ui::bisect::multi_item(ui, d, multi, acts);
}
/// The commits from `oldest` up to HEAD (exactly `picked`, if any) back into the Commit
/// Composer, with or without the uncommitted changes.
fn recompose_items(ui: &mut Ui, d: &RepoData, label: &str, oldest: &str, picked: &[String], acts: &mut Vec<Action>) {
    if !crate::ai::enabled() {
        return;
    }
    let dirty = !d.status.staged.is_empty() || !d.status.unstaged.is_empty();
    for wip in [false, true].into_iter().take(1 + dirty as usize) {
        let text = if wip { format!("{}  {label} (with uncommitted changes)", icon::SPARKLE) } else { format!("{}  {label}", icon::SPARKLE) };
        let hover = keys(&format!("Let {} re-plan these unpushed commits' changes as new focused commits (you review the plan first; undo with ⌘Z)", crate::ai::name()));
        if ui.button(text).on_hover_text(hover).clicked() {
            acts.push(Action::Recompose(crate::repo::compose::Recompose { oldest: oldest.into(), picked: picked.to_vec(), wip }));
            ui.close();
        }
    }
}

fn patch_items(ui: &mut Ui, ids: &[String], acts: &mut Vec<Action>) {
    ui.separator();
    if ui.button(format!("{}  Copy as patch", icon::COPY)).clicked() {
        acts.push(Action::Patch { ids: ids.to_vec(), save: false });
    }
    if ui.button(format!("{}  Save as patch…", icon::FLOPPY_DISK)).clicked() {
        acts.push(Action::Patch { ids: ids.to_vec(), save: true });
    }
}

/// "Move tag here": every tag not already on this commit, newest first.
fn move_tag_menu(ui: &mut Ui, d: &RepoData, id: &str, acts: &mut Vec<Action>) {
    let movable = |r: &&Ref| r.kind == RefKind::Tag && r.target != id;
    if !d.refs.iter().any(|r| movable(&r)) {
        return;
    }
    ui.menu_button(format!("{}  Move tag here", icon::TAG), |ui| {
        let mut tags: Vec<&Ref> = d.refs.iter().filter(movable).collect();
        tags.sort_by_key(|r| std::cmp::Reverse(r.date));
        egui::ScrollArea::vertical().max_height(320.0).show_rows(ui, ui.spacing().interact_size.y, tags.len(), |ui, rows| {
            for &r in &tags[rows] {
                let b = ui.button(&r.name);
                let b = if r.message.is_empty() { b } else { b.on_hover_text(&r.message) };
                if b.clicked() {
                    acts.push(crate::repo::tags::move_tag(&d.git_dir, r, id).map_or_else(Action::Error, Action::Git));
                }
            }
        });
    });
}

/// Double-click: check out the best branch at this commit.
pub(super) fn checkout_action(d: &RepoData, c: &Commit) -> Option<Action> {
    let refs = d.refs_at.get(&c.id)?;
    let r = refs.iter().map(|&i| &d.refs[i]).find(|r| r.kind != RefKind::Tag)?;
    branch_checkout(d, r)
}

/// Merge/rebase/cherry-pick style actions targeting `target` (branch name or commit id) at
/// revision `rev` (`RepoData::rev`, or the commit id).
pub fn integrate_items(ui: &mut Ui, d: &RepoData, target: &str, rev: &str, acts: &mut Vec<Action>) {
    let Some(head) = d.status.branch.as_deref() else { return };
    if target == head {
        return;
    }
    if ui.button(format!("Merge {target} into {head}")).clicked() {
        acts.push(Action::Git(merge_op(target, rev, None)));
    }
    ui.menu_button(format!("Merge {target} into {head} with options"), |ui| {
        for how in Merge::ALL {
            let hint = match how {
                Merge::Squash => "one staged change, commit it yourself (needs no uncommitted changes)",
                Merge::NoFf => "always create a merge commit",
                Merge::FfOnly => "only move the branch forward, never create a commit",
            };
            if ui.button(format!("Merge ({})", how.name())).on_hover_text(hint).clicked() {
                acts.push(Action::Git(merge_op(target, rev, Some(how))));
                ui.close();
            }
        }
    });
    if ui.button(format!("Rebase {head} onto {target}")).clicked() {
        let op = Op::new(format!("Rebase onto {target}"), &["rebase", "--end-of-options", rev]).undo(Undo::Keep);
        acts.push(Action::RunRebase { op, onto: rev.into(), branch: None, first: None });
    }
    if ui.button(format!("Interactive rebase {head} onto {target}…")).clicked() {
        acts.push(Action::Rebase { base: rev.into(), label: target.into() });
    }
}

/// Is commit `id` on HEAD's own history? A commit older than the loaded page is asked of git.
pub(super) fn on_branch(ui: &Ui, d: &RepoData, search: Option<&Search>, id: &str) -> bool {
    let Some(head) = d.status.head.as_deref() else { return false };
    match d.row_of.get(head).zip(d.row_of.get(id)) {
        Some((&h, &r)) => d.reachable(h)[r],
        None => search.and_then(|s| s.is_ancestor(ui.ctx(), id, head)).unwrap_or(false),
    }
}

pub fn commit_menu(ui: &mut Ui, d: &RepoData, c: &Commit, on_branch: bool, acts: &mut Vec<Action>) {
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
            Op::new("Create worktree", &["worktree", "add", "-b", "{}", &d.worktree_path(Prompt::VALUE), &c.id]).then_open(d.worktree_path(Prompt::VALUE)),
        ));
    }
    if ui.button("Create tag here…").clicked() {
        acts.push(prompt(format!("New tag at {short}"), "", Op::new("Create tag", &["tag", "--no-sign", "--", "{}", &c.id]).undo(Undo::Keep)));
    }
    if ui.button("Create annotated tag here…").clicked() {
        acts.push(Action::Prompt(Prompt {
            title: format!("Annotated tag at {short}"),
            value: String::new(),
            op: Op::new("Create tag", &["tag", "-a", "--cleanup=whitespace", "-m", "{2}", "--", "{}", &c.id]).undo(Undo::Keep),
            second: Some(("Tag message (⌘↩ to create)".into(), String::new())),
            sign: Some(d.cfg_bool("tag.gpgsign")),
        }));
    }
    move_tag_menu(ui, d, &c.id, acts);
    ui.separator();
    let (target, rev) = refs.iter().find(|r| r.kind != RefKind::Tag).map_or((c.id.as_str(), c.id.as_str()), |r| (r.name.as_str(), d.rev(r)));
    let is_head = d.status.head.as_deref() == Some(&c.id);
    if !is_head {
        integrate_items(ui, d, target, rev, acts);
        let merge = c.parents.len() > 1;
        if ui.button("Cherry-pick commit").clicked() {
            acts.push(Action::Git(pick_op(format!("Cherry-pick {short}"), &[&c.id], merge, false)));
        }
        if ui
            .button("Cherry-pick without committing")
            .on_hover_text("Stage its change and commit it yourself (needs no uncommitted changes)")
            .clicked()
        {
            acts.push(Action::Git(pick_op(format!("Cherry-pick {short} (no commit)"), &[&c.id], merge, true)));
        }
    }
    // Only HEAD's own history: for another branch's commit it would be a rebase onto it.
    if d.status.branch.is_some() && !is_head && on_branch && ui.button("Edit history after this commit (interactive rebase)…").clicked() {
        acts.push(Action::Rebase { base: c.id.clone(), label: short.into() });
    }
    if d.status.branch.is_some() && c.parents.len() == 1 {
        ui.menu_button(format!("{}  Rewrite this commit", icon::PENCIL_SIMPLE), |ui| {
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
            if ui.button(format!("{}  Split this commit…", icon::SCISSORS)).on_hover_text("Uncommit it, then commit its parts one by one").clicked() {
                acts.push(Action::Split(c.id.clone()));
                ui.close();
            }
            if on_branch {
                recompose_items(ui, d, "Recompose from here…", &c.id, &[], acts);
            }
            if ui.button("Squash into parent").clicked() {
                acts.push(Action::Rewrite { commit: c.id.clone(), act: crate::repo::rebase::Act::Squash, message: None });
                ui.close();
            }
            if ui.button(egui::RichText::new("Drop commit").color(theme::del())).clicked() {
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
        let mut op = Op::new(format!("Revert {short}"), &revert).undo(Undo::Keep);
        if let Some(first) = c.parents.first().filter(|_| c.parents.len() > 1) {
            op.label = format!("Revert merge {short}");
            op = op.confirm(format!(
                "{short} is a merge commit. Revert it against parent 1 ({})? That undoes what the merge brought in from its other side(s); the first parent's side stays. (Undo with ⌘Z.)",
                &first[..first.len().min(7)]
            ));
        }
        acts.push(Action::Git(op));
    }
    reset_menu(ui, d, &c.id, "this commit", acts);
    crate::ui::bisect::commit_items(ui, d, c, acts);
    ui.separator();
    if ui.button("Copy commit SHA").clicked() {
        acts.push(Action::Copy(c.id.clone()));
    }
    if ui.button("Copy commit message").clicked() {
        acts.push(Action::Copy(crate::git::run(&d.git_dir, &["log", "-1", "--format=%B", &c.id]).map_or_else(|_| c.summary.clone(), |m| m.trim_end().to_owned())));
    }
    if !is_head && ui.button("Compare with HEAD").clicked() {
        acts.push(Action::Select(Sel::Compare { from: c.id.clone(), to: "HEAD".into(), three_dot: false }));
    }
    if ui.button("Compare working tree with this commit").on_hover_text("Your uncommitted and committed changes since this commit").clicked() {
        acts.push(Action::Select(Sel::Compare { from: c.id.clone(), to: crate::git::WORKTREE.into(), three_dot: false }));
    }
    crate::ui::links::items(ui, d, &[("commit", crate::git::Page::Commit(&c.id))], &[&c.id], acts);
    // format-patch skips merge commits.
    if c.parents.len() < 2 {
        patch_items(ui, std::slice::from_ref(&c.id), acts);
    }
    let cmds = crate::repo::tools::commands(Some("sha"));
    if !cmds.is_empty() {
        ui.separator();
        // {branch}: the branch at this commit, else the current one.
        let branch = refs.iter().find(|r| r.kind == RefKind::Local).map(|r| r.name.clone()).or_else(|| d.status.branch.clone()).unwrap_or_default();
        let vars = crate::repo::tools::Vars { branch, sha: c.id.clone(), path: String::new() };
        crate::ui::inspector::custom_items(ui, cmds, &vars, acts);
    }
}

/// "Reset <current branch> to <what>" (commit, branch and tag menus); `id` is the commit.
pub fn reset_menu(ui: &mut Ui, d: &RepoData, id: &str, what: &str, acts: &mut Vec<Action>) {
    let Some(head) = d.status.branch.as_deref().filter(|_| d.status.head.as_deref().is_some_and(|h| h != id)) else { return };
    let label = format!("{}  Reset {head} to {what}", icon::ARROW_U_UP_LEFT);
    if let Some(busy) = reset_busy(d) {
        ui.add_enabled(false, egui::Button::new(label)).on_disabled_hover_text(busy);
        return;
    }
    ui.menu_button(label, |ui| reset_items(ui, d, id, acts));
}

/// Why resetting is off now (an operation in progress), if it is.
pub fn reset_busy(d: &RepoData) -> Option<String> {
    let busy = match d.state {
        State::Clean => return None,
        State::Merging => "merge",
        State::Rebasing => "rebase",
        State::CherryPicking => "cherry-pick",
        State::Reverting => "revert",
        State::Bisecting => "bisect",
        State::Applying => "patch (git am)",
    };
    Some(format!("Finish or abort the {busy} in progress first"))
}

/// The soft / mixed / keep / hard choices of resetting the current branch to `id`.
pub fn reset_items(ui: &mut Ui, d: &RepoData, id: &str, acts: &mut Vec<Action>) {
    for (mode, hint) in [
        ("--soft", "keep all changes staged"),
        ("--mixed", "keep changes, unstaged"),
        ("--keep", "update files, keep uncommitted changes"),
        ("--hard", "discard all changes (⌘Z restores them)"),
    ] {
        let text = egui::RichText::new(keys(&format!("{} — {hint}", &mode[2..])));
        if ui.button(if mode == "--hard" { text.color(theme::del()) } else { text }).clicked() {
            acts.push(Action::Git(crate::repo::reset_op(d, id, mode)));
        }
    }
}

pub(super) fn wip_menu(ui: &mut Ui, state: State, acts: &mut Vec<Action>) {
    if ui.button("Stash all changes").clicked() {
        acts.push(stash_prompt());
    }
    if ui.button("Discard all changes").clicked() {
        acts.push(Action::Git(discard_all_op(state)));
    }
}

fn discard_all_op(state: State) -> Op {
    let ends = match state {
        State::Merging => " This also ends the merge in progress.",
        State::CherryPicking => " This also ends the cherry-pick in progress.",
        State::Reverting => " This also ends the revert in progress.",
        _ => "",
    };
    Op::new("Discard all", &["reset", "--hard", "-q"])
        .then(&["clean", "-fdq"])
        .backup(crate::repo::Backup::Tree { revs: vec![], untracked: true })
        .confirm(format!("Discard ALL uncommitted changes, including untracked files?{ends} (Undo with ⌘Z.)"))
}

#[cfg(test)]
mod tests {
    use crate::ui::testkit::{Sim, load, tab};
    use egui::{PointerButton, Ui};

    #[test]
    fn discard_all_says_it_ends_a_merge() {
        let text = |s| super::discard_all_op(s).confirm.unwrap_or_default();
        assert!(text(crate::git::State::Merging).contains("ends the merge"));
        assert!(!text(crate::git::State::Clean).contains("ends"));
    }

    fn click(sim: &mut Sim, item: &str, mut draw: impl FnMut(&mut Ui)) {
        sim.frame(vec![], &mut draw);
        sim.frame(vec![], &mut draw);
        let at = sim.text(|s| s.ends_with(item)).unwrap_or_else(|| panic!("{item} is not drawn"));
        sim.click(at, PointerButton::Primary, draw);
    }

    #[test]
    fn rebasing_pushed_commits_onto_a_branch_asks_first() {
        let mut sim = Sim::new(800.0, 600.0);
        let mut t = tab(&sim.ctx, "rebase-onto-pushed", &["base"]);
        let dir = t.dir.clone();
        let git = |args: &[&str]| crate::git::run(&dir, args).unwrap();
        git(&["branch", "-M", "main"]);
        git(&["branch", "old"]);
        git(&["checkout", "-q", "-b", "feature"]);
        for f in ["f1", "f2"] {
            std::fs::write(dir.join(f), f).unwrap();
            git(&["add", f]);
            git(&["commit", "-q", "-m", f]);
        }
        git(&["update-ref", "refs/remotes/origin/feature", "HEAD"]);
        git(&["checkout", "-q", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "m1"]);
        git(&["checkout", "-q", "feature"]);
        t.reload(&sim.ctx, true);
        load(&mut t, &sim.ctx);
        let mut acts = vec![];
        let d = t.data.as_ref().unwrap();
        click(&mut sim, "Rebase feature onto main", |ui| super::integrate_items(ui, d, "main", "main", &mut acts));
        let palette = crate::ui::palette::items(Some(&t), &[], crate::repo::PullMode::Merge, false);
        let pick = |label: &str| palette.iter().find_map(|i| match &i.cmd {
            crate::ui::palette::Cmd::Tab(a) if i.label == label => Some(a.clone()),
            _ => None,
        });
        acts.extend(pick("Rebase feature onto main"));
        t.drop_menu = Some(("refs/heads/feature".into(), "refs/heads/main".into(), egui::pos2(20.0, 20.0)));
        click(&mut sim, "Rebase feature onto main", |ui| crate::ui::dialogs::drop_menu(&mut t, ui.ctx(), &mut acts));
        assert_eq!(acts.len(), 3);
        for a in acts {
            t.apply(&sim.ctx, a);
            load(&mut t, &sim.ctx);
            let ask = t.confirm.take().and_then(|o| o.confirm);
            assert!(ask.as_deref().is_some_and(|c| c.contains("2 commits already on a remote branch")), "{ask:?}");
        }
        let tip = git(&["rev-parse", "HEAD"]);
        t.apply(&sim.ctx, pick("Rebase feature onto old").unwrap());
        load(&mut t, &sim.ctx);
        assert!(t.confirm.is_none());
        assert_eq!(git(&["rev-parse", "HEAD"]), tip);
    }

    #[test]
    fn move_tag_lays_out_only_the_tags_in_view() {
        use crate::git::{Ref, RefKind};
        use crate::repo::Action;
        let mut sim = Sim::new(800.0, 600.0);
        let mut t = tab(&sim.ctx, "move-tag-rows", &["x"]);
        let mut d = t.data.take().unwrap();
        let tag = |i: i64| Ref { kind: RefKind::Tag, full: format!("refs/tags/t{i:03}"), target: "old".into(), date: i, ..crate::ui::testkit::local_ref(&format!("t{i:03}")) };
        d.refs = std::sync::Arc::new((0..300).map(tag).collect());
        let mut acts = vec![];
        click(&mut sim, "Move tag here", |ui| super::move_tag_menu(ui, &d, "new", &mut acts));
        assert!(sim.text(|s| s == "t299").is_some(), "the newest tag comes first");
        let laid_out = sim.ctx.fonts(|f| f.num_galleys_in_cache());
        assert!(laid_out < 60, "{laid_out} texts laid out for 300 tags");
        click(&mut sim, "t298", |ui| super::move_tag_menu(ui, &d, "new", &mut acts));
        assert!(matches!(&acts[..], [Action::Git(op)] if op.label == "Move tag t298"));
    }
}
