//! Right-click menus of refs (sidebar rows and graph pills): local branch,
//! remote branch and tag menus plus graph hide/solo toggles.

use crate::ui::format::keys;
use crate::ai::Purpose;
use crate::git::{Page, RefKind};
use crate::repo::{Action, Op, RepoData, Sel, Undo, branch_checkout, delete_branches, op, prompt};
use crate::theme::icon;
use crate::ui::graph_view::menus::{integrate_items, reset_menu};
use egui::Ui;

pub fn ref_menu(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    match r.kind {
        RefKind::Local => local_menu(ui, d, r, acts),
        RefKind::Remote => remote_menu(ui, d, r, acts),
        RefKind::Tag => tag_menu(ui, d, r, acts),
    }
    ui.separator();
    let toggle = |list: fn(&mut crate::repo::View) -> &mut Vec<String>| {
        let mut v = d.view.clone();
        let l = list(&mut v);
        match l.iter().position(|x| *x == r.full) {
            Some(i) => _ = l.remove(i),
            None => l.push(r.full.clone()),
        }
        Action::View(v)
    };
    // HEAD is always in the graph, so hiding the checked-out branch would do nothing.
    if d.view.solo.is_empty() && !r.head {
        let hidden = d.view.hidden.contains(&r.full);
        let label = if hidden { format!("{}  Show in graph", icon::EYE) } else { format!("{}  Hide from graph", icon::EYE_SLASH) };
        if ui.button(label).clicked() {
            acts.push(toggle(|v| &mut v.hidden));
        }
    }
    let solo = d.view.solo.contains(&r.full);
    let label = if solo { format!("{}  Unsolo", icon::CROSSHAIR) } else { format!("{}  Solo (show only soloed refs)", icon::CROSSHAIR) };
    if ui.button(label).clicked() {
        acts.push(toggle(|v| &mut v.solo));
    }
}

fn worktree_compare(ui: &mut Ui, name: &str, acts: &mut Vec<Action>) {
    if ui.button(format!("{}  Compare working tree with {name}", icon::GIT_DIFF)).on_hover_text("What your working tree changes relative to this ref").clicked() {
        acts.push(Action::Select(Sel::Compare { from: name.into(), to: crate::git::WORKTREE.into(), three_dot: false }));
    }
}

fn compare_items(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    let (name, rev, tip) = (r.name.as_str(), d.rev(r), r.target.as_str());
    worktree_compare(ui, name, acts);
    let Some(head) = d.status.branch.clone().filter(|h| h != name) else { return };
    if ui.button(format!("{}  Compare with {head}", icon::GIT_DIFF)).on_hover_text(format!("Changes on {name} since it diverged from {head}")).clicked() {
        acts.push(Action::Select(Sel::Compare { from: head.clone(), to: rev.into(), three_dot: true }));
    }
    if crate::ai::enabled() && ui.button(format!("{}  Explain changes vs {head}", icon::SPARKLE)).clicked() {
        acts.push(Action::Ai(Purpose::ExplainRange { from: head.clone(), to: rev.into() }));
    }
    if let Some(base) = d.local(&head) {
        // By commit id: a local branch name need not exist on the remote.
        let page = Page::Compare { base: &base.target, head: tip };
        crate::ui::links::items(ui, d, &[("compare", page)], &[&base.target, tip], acts);
    }
}

/// `r` is the branch (local or remote-tracking) whose PR / web pages these open.
fn web_items(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    let dest = match r.kind {
        RefKind::Remote => d.split_remote(&r.name),
        _ => d.push_dest(&r.name, &r.upstream),
    };
    let (remote, branch) = dest.map_or(("origin".into(), r.name.clone()), |(a, b)| (a.to_owned(), b.to_owned()));
    ui.separator();
    if crate::ai::enabled() && ui.button(format!("{}  Write pull request…", icon::SPARKLE)).clicked() {
        let def = d.default_branch().to_owned();
        let base = if d.refs.iter().any(|x| x.name == format!("origin/{def}")) { format!("origin/{def}") } else { def };
        acts.push(Action::Ai(Purpose::PullRequest { branch: r.name.clone(), base }));
    }
    let Some(web) = d.web_of(&remote) else { return };
    let enc = crate::git::url_path;
    // A stacked branch's pull request goes into its parent's.
    let into = crate::repo::stack::parent(d, &r.name).filter(|_| r.kind == RefKind::Local).map_or_else(|| d.default_branch().to_owned(), str::to_owned);
    if ui.button("Create pull request…").on_hover_text(format!("Opens the compare page into {into} on GitHub / GitHub Enterprise")).clicked() {
        acts.push(Action::Url(format!("{web}/compare/{}...{}?expand=1", enc(&into), enc(&branch))));
    }
    if ui.button("Open branch on remote").clicked() {
        acts.push(Action::Url(Page::Branch(&branch).url(&web)));
    }
}

fn local_menu(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    let name = r.name.as_str();
    if !r.head {
        if ui.button(format!("Checkout {name}")).clicked() {
            acts.extend(branch_checkout(d, r));
        }
        if !r.upstream.is_empty() && r.behind > 0 && r.ahead == 0 && ui.button(format!("Fast-forward {name} to {}", r.upstream)).clicked() {
            acts.push(Action::Git(Op::new(format!("Fast-forward {name}"), &["fetch", ".", &format!("{}:{name}", r.upstream)]).undo(Undo::Keep)));
        }
        if let Some((remote, rb)) = d.split_remote(&r.upstream).filter(|_| !r.gone)
            && ui.button(format!("Pull {name} (fast-forward, without checkout)")).clicked()
        {
            acts.push(Action::Git(crate::repo::remotes::pull_ff(name, remote, rb)));
        }
        integrate_items(ui, d, name, d.rev(r), acts);
        reset_menu(ui, d, &r.target, name, acts);
        compare_items(ui, d, r, acts);
    }
    if r.head
        && let Some((label, a)) = crate::repo::spinoff::move_unpushed(d)
    {
        ui.separator();
        if ui.button(format!("{}  {label}", icon::GIT_FORK)).on_hover_text(format!("New branch at HEAD for them; {name} goes back to {}", r.upstream)).clicked() {
            acts.push(a);
        }
    }
    ui.separator();
    if let Some((remote, rb)) = d.push_target(name, &r.upstream) {
        let to = if rb == name { remote.to_owned() } else { format!("{remote}/{rb}") };
        if ui.button(format!("Push {name} to {to}")).clicked() {
            // `-u` only toward the upstream's own remote: it must not retarget a triangular setup.
            let spec = format!("{name}:refs/heads/{rb}");
            let keep = d.split_remote(&r.upstream).is_some_and(|u| u.0 != remote);
            acts.push(op(format!("Push {name}"), &if keep { vec!["push", remote, &spec] } else { vec!["push", "-u", remote, &spec] }));
        }
    }
    let members = crate::repo::stack::members(d, name);
    if members.len() > 1 {
        if members.iter().any(|m| crate::repo::stack::stale(d, m))
            && ui
                .button(format!("{}  Restack", icon::ARROWS_CLOCKWISE))
                .on_hover_text(keys("Rebase each branch of this stack whose parent moved on onto it, bottom first. All or nothing; undo with ⌘Z."))
                .clicked()
        {
            acts.push(Action::Git(crate::repo::stack::restack_op(d, name)));
        }
        if !d.remotes.is_empty()
            && ui
                .button(format!("{}  Push stack ({} branches)", icon::STACK, members.len()))
                .on_hover_text(format!("Push {}; a branch rewritten since its last push is force-pushed with a lease (asks first), one behind its remote is skipped", members.join(", ")))
                .clicked()
        {
            acts.extend(crate::repo::stack::push_op(d, name).map(Action::Git));
        }
    }
    if !r.head && r.worktree.is_none() && ui.button("Open in a new worktree…").clicked() {
        let path = d.worktree_path(name);
        acts.push(prompt(format!("Worktree folder for {name}"), path, Op::new("Create worktree", &["worktree", "add", "{}", d.worktree_rev(r)]).then_open("{}")));
    }
    if let Some(p) = &r.worktree
        && ui.button(format!("{}  Open its worktree", icon::TREE_STRUCTURE)).on_hover_text(p.as_str()).clicked()
    {
        acts.push(Action::OpenTab(p.clone()));
    }
    if ui.button("Create branch from here…").clicked() {
        acts.push(prompt(format!("New branch from {name}"), "", Op::new("Create branch", &["checkout", "-b", "{}", d.rev(r)]).undo(Undo::Keep)));
    }
    if ui.button("Rename…").clicked() {
        acts.push(prompt(format!("Rename {name}"), name, Op::new("Rename branch", &["branch", "-m", name, "{}"]).revert(Op::new("Rename branch back", &["branch", "-m", "{}", name]))));
    }
    let default_up = if r.upstream.is_empty() {
        format!("{}/{name}", d.default_remote().unwrap_or("origin"))
    } else {
        r.upstream.clone()
    };
    if ui.button("Set upstream…").clicked() {
        acts.push(prompt(format!("Upstream for {name}"), default_up, Op::new("Set upstream", &["branch", "--set-upstream-to={}", name])));
    }
    if !r.upstream.is_empty() && ui.button(format!("Unset upstream ({})", r.upstream)).clicked() {
        acts.push(op("Unset upstream", &["branch", "--unset-upstream", name]));
    }
    if !r.head {
        if ui.button("Delete").clicked() {
            acts.push(Action::Git(delete_branches(d, format!("Delete {name}"), &[name], false)));
        }
        if ui.button("Force delete").clicked() {
            acts.push(Action::Git(
                delete_branches(d, format!("Delete {name}"), &[name], true).confirm(format!("Delete {name} even if it is not merged? (You can undo this.)")),
            ));
        }
    }
    web_items(ui, d, r, acts);
    ui.separator();
    if ui.button("Copy branch name").clicked() {
        acts.push(Action::Copy(name.into()));
    }
}

fn remote_menu(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    if ui.button(format!("Checkout {}", r.name)).clicked() {
        acts.extend(branch_checkout(d, r));
    }
    integrate_items(ui, d, &r.name, d.rev(r), acts);
    reset_menu(ui, d, &r.target, &r.name, acts);
    compare_items(ui, d, r, acts);
    let local = d.split_remote(&r.name).map_or(r.name.as_str(), |s| s.1).to_owned();
    if d.local(&local).is_none() && ui.button("Check out in a new worktree…").clicked() {
        acts.push(prompt(
            format!("Worktree folder for {local}"),
            d.worktree_path(&local),
            Op::new("Create worktree", &["worktree", "add", "--track", "-b", &local, "{}", &r.full]).then_open("{}"),
        ));
    }
    ui.separator();
    if let Some((remote, branch)) = d.split_remote(&r.name)
        && ui.button(format!("Delete {branch} from {remote}")).clicked()
    {
        acts.push(Action::Git(crate::repo::remotes::delete_branch(remote, branch)));
    }
    web_items(ui, d, r, acts);
    ui.separator();
    if ui.button("Copy branch name").clicked() {
        acts.push(Action::Copy(r.name.clone()));
    }
}

fn tag_menu(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    if ui.button(format!("Checkout {}", r.name)).clicked() {
        acts.extend(branch_checkout(d, r));
    }
    reset_menu(ui, d, &r.target, &r.name, acts);
    worktree_compare(ui, &r.name, acts);
    if let Some(remote) = d.default_remote() {
        if ui.button(format!("Push tag to {remote}")).clicked() {
            acts.push(op(format!("Push tag {}", r.name), &["push", remote, &r.full]));
        }
        if ui.button(format!("Delete tag from {remote}")).clicked() {
            acts.push(Action::Git(
                Op::new(format!("Delete remote tag {}", r.name), &["push", remote, "--delete", &r.full])
                    .confirm(format!("Delete tag {} on {remote}?", r.name)),
            ));
        }
    }
    if r.annotated && ui.button(format!("{}  Verify signature", icon::SEAL_CHECK)).on_hover_text("Check the tag's signature (git verify-tag)").clicked() {
        acts.push(Action::VerifyTag(r.full.clone()));
    }
    if ui.button("Delete tag").clicked() {
        acts.push(Action::Git(Op::new(format!("Delete tag {}", r.name), &["tag", "-d", &r.name]).undo(Undo::Keep)));
    }
    if let Some(remote) = d.default_remote() {
        if ui.button(format!("Delete tag here and from {remote}")).clicked() {
            acts.push(Action::Git(crate::repo::tags::delete_everywhere(r, remote)));
        }
        if ui.button(format!("{}  Push all tags to {remote}", icon::TAG)).clicked() {
            acts.push(Action::Git(crate::repo::tags::push_all(remote)));
        }
    }
    crate::ui::links::items(ui, d, &[("release page", Page::Release(&r.name))], &[], acts);
    ui.separator();
    if ui.button("Copy tag name").clicked() {
        acts.push(Action::Copy(r.name.clone()));
    }
}
