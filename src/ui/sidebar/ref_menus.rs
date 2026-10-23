//! Right-click menus of refs (sidebar rows and graph pills): local branch,
//! remote branch and tag menus plus graph hide/solo toggles.

use crate::ui::format::keys;
use crate::ai::Purpose;
use crate::git::{Page, RefKind};
use crate::repo::{Action, Op, RepoData, Sel, Undo, ViewChange, branch_checkout, delete_branches, op, prompt};
use crate::theme::icon;
use crate::ui::graph_view::menus::{integrate_items, reset_menu};
use egui::Ui;

pub fn ref_menu(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    ui.spacing_mut().button_padding.x = 10.0;
    match r.kind {
        RefKind::Local => local_menu(ui, d, r, acts),
        RefKind::Remote => remote_menu(ui, d, r, false, acts),
        RefKind::Tag => tag_menu(ui, d, r, acts),
    }
    view_items(ui, d, r, acts);
    delete_items(ui, d, r, acts);
}

/// A local branch level with its upstream (one graph pill): the local's menu, then what is the
/// remote's own; both deletes come last.
pub fn synced_menu(ui: &mut Ui, d: &RepoData, local: &crate::git::Ref, up: &crate::git::Ref, acts: &mut Vec<Action>) {
    ui.spacing_mut().button_padding.x = 10.0;
    local_menu(ui, d, local, acts);
    view_items(ui, d, local, acts);
    ui.separator();
    ui.label(egui::RichText::new(format!("{}  {}", icon::CLOUD, up.name)).small().weak());
    remote_menu(ui, d, up, true, acts);
    view_items(ui, d, up, acts);
    delete_items(ui, d, local, acts);
    delete_items(ui, d, up, acts);
}

/// The destructive items, last in a ref's menu behind their own separator.
fn delete_items(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    let name = r.name.as_str();
    match r.kind {
        RefKind::Local if !r.head => {
            ui.separator();
            if ui.button("Delete").clicked() {
                acts.push(Action::Git(delete_branches(d, format!("Delete {name}"), &[name], false)));
            }
            if ui.button("Force delete").clicked() {
                acts.push(Action::Git(
                    delete_branches(d, format!("Delete {name}"), &[name], true).confirm(format!("Delete {name} even if it is not merged? (You can undo this.)")),
                ));
            }
            if let Some((label, op)) = crate::repo::delete_everywhere(d, r)
                && ui.button(label).clicked()
            {
                acts.push(Action::Git(op));
            }
        }
        RefKind::Remote => {
            if let Some((remote, branch)) = d.split_remote(name) {
                ui.separator();
                if ui.button(format!("Delete {branch} from {remote}")).clicked() {
                    acts.push(Action::Git(crate::repo::remotes::delete_branch(remote, branch, &r.target)));
                }
            }
        }
        RefKind::Tag => {
            ui.separator();
            if ui.button("Delete tag").clicked() {
                acts.push(Action::Git(Op::new(format!("Delete tag {name}"), &["tag", "-d", "--end-of-options", name]).undo(Undo::Keep)));
            }
            if let Some(remote) = d.default_remote() {
                if ui.button(format!("Delete tag from {remote}")).clicked() {
                    acts.push(Action::Git(Op::new(format!("Delete remote tag {name}"), &["push", remote, "--delete", &r.full]).confirm(format!("Delete tag {name} on {remote}?"))));
                }
                if ui.button(format!("Delete tag here and from {remote}")).clicked() {
                    acts.push(Action::Git(crate::repo::tags::delete_everywhere(r, remote)));
                }
            }
        }
        _ => {}
    }
}

/// Graph hide / solo toggles.
fn view_items(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    ui.separator();
    // HEAD is always in the graph, so hiding the checked-out branch would do nothing.
    if d.view.solo.is_empty() && !r.head {
        let hidden = d.view.hidden.contains(&r.full);
        let label = if hidden { format!("{}  Show in graph", icon::EYE) } else { format!("{}  Hide from graph", icon::EYE_SLASH) };
        if ui.button(label).clicked() {
            acts.push(Action::View(ViewChange::Hide(vec![r.full.clone()], !hidden)));
        }
    }
    let solo = d.view.solo.contains(&r.full);
    let label = if solo { format!("{}  Unsolo", icon::CROSSHAIR) } else { format!("{}  Solo (show only soloed refs)", icon::CROSSHAIR) };
    if ui.button(label).clicked() {
        acts.push(Action::View(ViewChange::Solo(r.full.clone(), !solo)));
    }
}

fn worktree_compare(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    if ui.button(format!("{}  Compare working tree with {}", icon::GIT_DIFF, r.name)).on_hover_text("What your working tree changes relative to this ref").clicked() {
        acts.push(Action::Select(Sel::Compare { from: d.rev(r).into(), to: crate::git::WORKTREE.into(), three_dot: false }));
    }
}

fn compare_items(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    let (name, rev, tip) = (r.name.as_str(), d.rev(r), r.target.as_str());
    worktree_compare(ui, d, r, acts);
    let Some(head) = d.status.branch.clone().filter(|h| h != name) else { return };
    let from = d.local(&head).map_or(head.as_str(), |b| d.rev(b));
    if ui.button(format!("{}  Compare with {head}", icon::GIT_DIFF)).on_hover_text(format!("Changes on {name} since it diverged from {head}")).clicked() {
        acts.push(Action::Select(Sel::Compare { from: from.into(), to: rev.into(), three_dot: true }));
    }
    if crate::ai::enabled() && ui.button(format!("{}  Explain changes vs {head}", icon::SPARKLE)).clicked() {
        acts.push(Action::Ai(Purpose::ExplainRange { from: from.into(), to: rev.into(), three_dot: true }));
    }
    if let Some(base) = d.local(&head) {
        // By commit id: a local branch name need not exist on the remote.
        let page = Page::Compare { base: &base.target, head: tip };
        crate::ui::links::items(ui, d, &[("compare", page)], &[&base.target, tip], acts);
    }
}

/// The remote and its branch that `name` (a local or remote-tracking branch) stands for:
/// where a local branch pushes to; origin and its own name when that is unknown.
fn remote_branch(d: &RepoData, name: &str) -> (String, String) {
    let r = d.refs.iter().find(|r| (r.name == name || r.full == name) && matches!(r.kind, RefKind::Local | RefKind::Remote));
    let name = r.map_or(crate::git::short_ref(name), |r| r.name.as_str());
    let dest = match r {
        Some(r) if r.kind == RefKind::Local => d.push_target(name, &r.upstream),
        _ => d.split_remote(name),
    };
    dest.map_or(("origin".into(), name.to_owned()), |(a, b)| (a.to_owned(), b.to_owned()))
}

/// The compare page that opens a pull request of `head` into `base` (each a local or
/// remote-tracking branch), on the remote `head` is on.
pub(crate) fn pull_request_url(d: &RepoData, head: &str, base: &str) -> Option<String> {
    let (remote, branch) = remote_branch(d, head);
    let base = remote_branch(d, base).1;
    Some(format!("{}/compare/{}...{}?expand=1", d.web_of(&remote)?, crate::git::url_path(&base), crate::git::url_path(&branch)))
}

pub(crate) fn pr_base(d: &RepoData, name: &str) -> String {
    let on = remote_branch(d, name).0;
    if let Some(p) = d.local(name).and_then(|_| crate::repo::stack::parent(d, name))
        && d.refs.iter().any(|x| x.kind == RefKind::Remote && x.name == format!("{on}/{}", remote_branch(d, p).1))
    {
        return p.to_owned();
    }
    let def = d.default_branch();
    if d.refs.iter().any(|x| x.name == format!("origin/{def}")) { format!("origin/{def}") } else { def.to_owned() }
}

/// `r` is the branch (local or remote-tracking) whose PR / web pages these open.
fn web_items(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    let (remote, branch) = remote_branch(d, &r.name);
    ui.separator();
    if crate::ai::enabled() && ui.button(format!("{}  Write pull request…", icon::SPARKLE)).clicked() {
        let base = pr_base(d, &r.name);
        acts.push(Action::Ai(Purpose::PullRequest { branch: d.rev(r).into(), base: d.local(&base).map_or(base.clone(), |l| d.rev(l).to_owned()) }));
    }
    let Some(web) = d.web_of(&remote) else { return };
    let base = pr_base(d, &r.name);
    let into = remote_branch(d, &base).1;
    if ui.button("Create pull request…").on_hover_text(format!("Opens the compare page into {into} on GitHub / GitHub Enterprise")).clicked()
        && let Some(url) = pull_request_url(d, &r.name, &base)
    {
        acts.push(Action::Url(url));
    }
    if ui.button("Open branch on remote").clicked() {
        acts.push(Action::Url(Page::Branch(&branch).url(&web)));
    }
}

fn local_menu(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    let name = r.name.as_str();
    // Switch and sync.
    if !r.head {
        if ui.button(format!("Checkout {name}")).clicked() {
            acts.extend(branch_checkout(d, r));
        }
        if !r.upstream.is_empty() && r.behind > 0 && r.ahead == 0 && ui.button(format!("Fast-forward {name} to {}", r.upstream)).clicked() {
            acts.push(Action::Git(Op::new(format!("Fast-forward {name}"), &["fetch", ".", &format!("{}:{}", r.upstream, r.full)]).undo(Undo::Keep)));
        }
        if let Some((remote, rb)) = d.split_remote(&r.upstream).filter(|_| !r.gone)
            && ui.button(format!("Pull {name} (fast-forward, without checkout)")).clicked()
        {
            acts.push(Action::Git(crate::repo::remotes::pull_ff(name, remote, rb)));
        }
    }
    if let Some((remote, rb)) = d.push_target(name, &r.upstream) {
        let to = if rb == name { remote.to_owned() } else { format!("{remote}/{rb}") };
        if ui.button(format!("Push {name} to {to}")).clicked() {
            // `-u` only toward the upstream's own remote: it must not retarget a triangular setup.
            let spec = format!("{}:refs/heads/{rb}", r.full);
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
    // Into the checked-out branch.
    if !r.head {
        ui.separator();
        integrate_items(ui, d, name, d.rev(r), acts);
        reset_menu(ui, d, &r.target, name, acts);
        compare_items(ui, d, r, acts);
    }
    // The branch itself.
    ui.separator();
    if ui.button("Create branch from here…").clicked() {
        acts.push(prompt(format!("New branch from {name}"), "", Op::new("Create branch", &["checkout", "-b", "{}", d.rev(r)]).undo(Undo::Keep)));
    }
    if ui.button("Rename…").clicked() {
        acts.push(prompt(format!("Rename {name}"), name, Op::new("Rename branch", &["branch", "-m", "--", name, "{}"]).revert(Op::new("Rename branch back", &["branch", "-m", "--", "{}", name]))));
    }
    let default_up = if r.upstream.is_empty() {
        format!("{}/{name}", d.default_remote().unwrap_or("origin"))
    } else {
        r.upstream.clone()
    };
    if ui.button("Set upstream…").clicked() {
        acts.push(prompt(format!("Upstream for {name}"), default_up, Op::new("Set upstream", &["branch", "--set-upstream-to", "{}", "--end-of-options", name])));
    }
    if !r.upstream.is_empty() && ui.button(format!("Unset upstream ({})", r.upstream)).clicked() {
        acts.push(op("Unset upstream", &["branch", "--unset-upstream", "--end-of-options", name]));
    }
    if r.head
        && let Some((label, a)) = crate::repo::spinoff::move_unpushed(d)
        && ui.button(format!("{}  {label}", icon::GIT_FORK)).on_hover_text(format!("New branch at HEAD for them; {name} goes back to {}", r.upstream)).clicked()
    {
        acts.push(a);
    }
    worktree_items(ui, d, r, acts);
    web_items(ui, d, r, acts);
    ui.separator();
    if ui.button("Copy branch name").clicked() {
        acts.push(Action::Copy(name.into()));
    }
}

/// Open the branch in a worktree, or remove the one it is checked out in (keeping the branch).
fn worktree_items(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    let name = r.name.as_str();
    let its = r.worktree.as_ref().and_then(|p| d.worktrees.iter().skip(1).find(|w| &w.path == p && w.locked.is_none()));
    let this = r.head.then(|| d.here.filter(|&i| i > 0).and_then(|i| d.worktrees.get(i)).filter(|w| w.locked.is_none())).flatten().zip(d.worktrees.first());
    let open = !r.head && r.worktree.is_none();
    if !open && r.worktree.is_none() && its.is_none() && this.is_none() {
        return;
    }
    ui.separator();
    if open && ui.button("Open in a new worktree…").clicked() {
        let path = d.worktree_path(name);
        acts.push(prompt(format!("Worktree folder for {name}"), path, Op::new("Create worktree", &["worktree", "add", "--end-of-options", "{}", d.worktree_rev(r)]).then_open("{}")));
    }
    if let Some(p) = &r.worktree
        && ui.button(format!("{}  Open its worktree", icon::TREE)).on_hover_text(p.as_str()).clicked()
    {
        acts.push(Action::Switch(p.clone()));
    }
    if let Some(w) = its
        && ui.button(format!("Remove its worktree (keep {name})")).on_hover_text(w.path.as_str()).clicked()
    {
        let (args, warn) = crate::repo::worktree_remove_args(w);
        acts.push(Action::Git(crate::repo::remove_worktree_op(w, &args, &format!("{warn}\nThe branch {name} stays."))));
    }
    if let Some((w, main)) = this
        && ui.button(format!("Remove this worktree (keep {name})")).on_hover_text(w.path.as_str()).clicked()
    {
        let (args, warn) = crate::repo::worktree_remove_args(w);
        let from_main: Vec<&str> = ["-C", main.path.as_str()].into_iter().chain(args).collect();
        let op = crate::repo::remove_worktree_op(w, &from_main, &format!("{warn}\nThe branch {name} stays; this tab moves to the primary worktree."));
        acts.push(Action::Git(op.then_open(main.path.clone())));
    }
}

/// `paired`: right after its local branch's menu (same commit), so only what is the remote's own.
fn remote_menu(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, paired: bool, acts: &mut Vec<Action>) {
    if paired {
        if ui.button("Copy branch name").clicked() {
            acts.push(Action::Copy(r.name.clone()));
        }
        return;
    }
    if ui.button(format!("Checkout {}", r.name)).clicked() {
        acts.extend(branch_checkout(d, r));
    }
    let local = d.split_remote(&r.name).map_or(r.name.as_str(), |s| s.1).to_owned();
    if d.local(&local).is_none() && ui.button("Check out in a new worktree…").clicked() {
        acts.push(prompt(
            format!("Worktree folder for {local}"),
            d.worktree_path(&local),
            Op::new("Create worktree", &["worktree", "add", "--track", "-b", &local, "{}", &r.full]).then_open("{}"),
        ));
    }
    ui.separator();
    integrate_items(ui, d, &r.name, d.rev(r), acts);
    reset_menu(ui, d, &r.target, &r.name, acts);
    compare_items(ui, d, r, acts);
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
    if let Some(remote) = d.default_remote() {
        if ui.button(format!("Push tag to {remote}")).clicked() {
            acts.push(op(format!("Push tag {}", r.name), &["push", remote, &r.full]));
        }
        if ui.button(format!("{}  Push all tags to {remote}", icon::TAG)).clicked() {
            acts.push(Action::Git(crate::repo::tags::push_all(remote)));
        }
    }
    ui.separator();
    reset_menu(ui, d, &r.target, &r.name, acts);
    worktree_compare(ui, d, r, acts);
    if r.annotated && ui.button(format!("{}  Verify signature", icon::SEAL_CHECK)).on_hover_text("Check the tag's signature (git verify-tag)").clicked() {
        acts.push(Action::VerifyTag(r.full.clone()));
    }
    crate::ui::links::items(ui, d, &[("release page", Page::Release(&r.name))], &[], acts);
    ui.separator();
    if ui.button("Copy tag name").clicked() {
        acts.push(Action::Copy(r.name.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::testkit::{Sim, load, tab};
    use egui::PointerButton;

    #[test]
    fn two_hides_before_the_reload_hide_both() {
        let mut sim = Sim::new(500.0, 900.0);
        let mut t = tab(&sim.ctx, "hide-twice", &["one"]);
        let dir = t.dir.clone();
        for b in ["x", "y"] {
            crate::git::run(&dir, &["branch", b]).unwrap();
        }
        t.reload(&sim.ctx, true);
        load(&mut t, &sim.ctx);
        let mut acts = vec![];
        let d = t.data.as_ref().unwrap();
        for b in ["x", "y"] {
            let mut draw = |ui: &mut Ui| view_items(ui, d, d.local(b).unwrap(), &mut acts);
            sim.frame(vec![], &mut draw);
            let at = sim.text(|s| s.ends_with("Hide from graph")).expect("the hide item");
            sim.click(at, PointerButton::Primary, draw);
        }
        for a in acts {
            t.apply(&sim.ctx, a);
        }
        assert_eq!(t.view.hidden, ["refs/heads/x", "refs/heads/y"]);
    }

    #[test]
    fn pull_request_base_is_the_stack_parent() {
        let sim = Sim::new(500.0, 900.0);
        let mut t = tab(&sim.ctx, "pr-base", &["one"]);
        let dir = t.dir.clone();
        let git = |args: &[&str]| crate::git::run(&dir, args).unwrap();
        for b in ["a", "b"] {
            git(&["checkout", "-q", "-b", b]);
            std::fs::write(t.dir.join(b), b).unwrap();
            git(&["add", b]);
            git(&["commit", "-q", "-m", b]);
        }
        t.reload(&sim.ctx, true);
        load(&mut t, &sim.ctx);
        let d = t.data.as_ref().unwrap();
        assert_eq!((pr_base(d, "b"), pr_base(d, "a")), (d.default_branch().to_owned(), d.default_branch().to_owned()));
        git(&["update-ref", "refs/remotes/origin/a", "a"]);
        git(&["tag", "a", "HEAD~2"]);
        t.reload(&sim.ctx, true);
        load(&mut t, &sim.ctx);
        let d = t.data.as_ref().unwrap();
        assert_eq!(pr_base(d, "b"), "a");
        assert_eq!(d.rev(d.local("a").unwrap()), "refs/heads/a");
    }

    #[test]
    fn a_branch_named_like_a_ref_path_is_addressed_in_full() {
        let sim = Sim::new(500.0, 900.0);
        let mut t = tab(&sim.ctx, "rev-ref-path", &["one", "two"]);
        let dir = t.dir.clone();
        let run = |args: &[&str]| crate::git::run(&dir, args).unwrap();
        run(&["branch", "tags/v1", "HEAD~1"]);
        run(&["tag", "v1", "HEAD"]);
        t.reload(&sim.ctx, true);
        load(&mut t, &sim.ctx);
        let d = t.data.as_ref().unwrap();
        assert_eq!(d.rev(d.local("tags/v1").unwrap()), "refs/heads/tags/v1");
    }

    #[test]
    fn push_works_for_a_branch_that_shares_its_name_with_a_tag() {
        let mut sim = Sim::new(500.0, 900.0);
        let mut t = tab(&sim.ctx, "push-tagged", &["one"]);
        let (dir, remote) = (t.dir.clone(), std::env::temp_dir().join(format!("glasir-ui-push-tagged-remote-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&remote);
        std::fs::create_dir_all(&remote).unwrap();
        let git = |at: &std::path::Path, args: &[&str]| crate::git::run(at, args).unwrap();
        git(&remote, &["init", "-q", "--bare"]);
        git(&dir, &["checkout", "-q", "-b", "release-1.0"]);
        git(&dir, &["tag", "release-1.0"]);
        git(&dir, &["remote", "add", "origin", &remote.to_string_lossy()]);
        t.reload(&sim.ctx, true);
        load(&mut t, &sim.ctx);
        let (d, mut acts) = (t.data.as_ref().unwrap(), vec![]);
        let mut draw = |ui: &mut Ui| ref_menu(ui, d, d.local("release-1.0").unwrap(), &mut acts);
        sim.frame(vec![], &mut draw);
        let at = sim.text(|s| s.starts_with("Push release-1.0 to")).expect("the push item");
        sim.click(at, PointerButton::Primary, draw);
        assert_eq!(acts.len(), 1);
        t.apply(&sim.ctx, acts.remove(0));
        load(&mut t, &sim.ctx);
        assert_eq!(git(&remote, &["rev-parse", "refs/heads/release-1.0"]), git(&dir, &["rev-parse", "HEAD"]));
        let _ = std::fs::remove_dir_all(&remote);
    }

    #[test]
    fn fast_forward_moves_the_branch_whatever_its_name() {
        let mut sim = Sim::new(500.0, 900.0);
        let mut t = tab(&sim.ctx, "ff-name", &["one"]);
        let dir = t.dir.clone();
        let git = |args: &[&str]| crate::git::run(&dir, args).unwrap();
        let main = git(&["branch", "--show-current"]).trim().to_owned();
        git(&["branch", "tags/v1"]);
        std::fs::write(dir.join("f"), "two").unwrap();
        git(&["commit", "-q", "-am", "two"]);
        git(&["branch", "-q", "-u", &main, "tags/v1"]);
        t.reload(&sim.ctx, true);
        load(&mut t, &sim.ctx);
        let (d, mut acts) = (t.data.as_ref().unwrap(), vec![]);
        let mut draw = |ui: &mut Ui| ref_menu(ui, d, d.local("tags/v1").unwrap(), &mut acts);
        sim.frame(vec![], &mut draw);
        let at = sim.text(|s| s.starts_with("Fast-forward tags/v1")).expect("the fast-forward item");
        sim.click(at, PointerButton::Primary, draw);
        t.apply(&sim.ctx, acts.remove(0));
        load(&mut t, &sim.ctx);
        assert_eq!(git(&["rev-parse", "refs/heads/tags/v1"]), git(&["rev-parse", "HEAD"]));
        assert_eq!(git(&["tag"]), "");
    }

    #[test]
    fn a_pull_request_opens_where_its_branch_is_pushed() {
        let ctx = egui::Context::default();
        let mut t = tab(&ctx, "pr-url", &["one"]);
        let dir = t.dir.clone();
        let git = |args: &[&str]| crate::git::run(&dir, args).unwrap();
        git(&["remote", "add", "origin", "https://github.com/o/r.git"]);
        git(&["remote", "add", "upstream", "https://github.com/u/r.git"]);
        for r in ["origin/main", "origin/bugfix-123", "upstream/feature"] {
            git(&["update-ref", &format!("refs/remotes/{r}"), "HEAD"]);
        }
        git(&["branch", "-q", "fix"]);
        git(&["branch", "-q", "--set-upstream-to=origin/bugfix-123", "fix"]);
        git(&["branch", "-q", "--track", "forked", "origin/main"]);
        git(&["config", "branch.forked.pushRemote", "upstream"]);
        t.reload(&ctx, true);
        load(&mut t, &ctx);
        let d = t.data.as_ref().unwrap();
        assert_eq!(pull_request_url(d, "fix", "origin/main").as_deref(), Some("https://github.com/o/r/compare/main...bugfix-123?expand=1"));
        assert_eq!(pull_request_url(d, "upstream/feature", "origin/main").as_deref(), Some("https://github.com/u/r/compare/main...feature?expand=1"));
        assert_eq!(pull_request_url(d, "refs/heads/fix", "origin/main").as_deref(), Some("https://github.com/o/r/compare/main...bugfix-123?expand=1"));
        assert_eq!(pull_request_url(d, "forked", "origin/main").as_deref(), Some("https://github.com/u/r/compare/main...forked?expand=1"));
    }

    #[test]
    fn compares_and_pull_requests_mean_the_branch_not_a_same_named_tag() {
        let mut sim = Sim::new(500.0, 1400.0);
        let mut t = tab(&sim.ctx, "compare-tagged", &["one"]);
        let dir = t.dir.clone();
        let git = |args: &[&str]| crate::git::run(&dir, args).unwrap().trim().to_owned();
        let other = git(&["branch", "--show-current"]);
        git(&["tag", "release-1.0"]);
        git(&["checkout", "-q", "-b", "release-1.0"]);
        git(&["commit", "-q", "--allow-empty", "-m", "two"]);
        let branch = git(&["rev-parse", "refs/heads/release-1.0"]);
        let mut revs = vec![];
        for (head, menu, items) in [(&other, "release-1.0", &["Compare working tree with", "Write pull request"][..]), (&"release-1.0".to_owned(), other.as_str(), &["Compare with", "Explain changes vs"][..])] {
            git(&["checkout", "-q", head]);
            t.reload(&sim.ctx, true);
            load(&mut t, &sim.ctx);
            let d = t.data.as_ref().unwrap();
            for item in items {
                let mut acts = vec![];
                let mut draw = |ui: &mut Ui| ref_menu(ui, d, d.local(menu).unwrap(), &mut acts);
                sim.frame(vec![], &mut draw);
                let at = sim.text(|s| s.contains(item)).unwrap_or_else(|| panic!("{item} is not in the menu"));
                sim.click(at, PointerButton::Primary, draw);
                revs.push(match acts.pop() {
                    Some(Action::Select(Sel::Compare { from, .. })) => from,
                    Some(Action::Ai(Purpose::ExplainRange { from, .. })) => from,
                    Some(Action::Ai(Purpose::PullRequest { branch, .. })) => branch,
                    _ => panic!("{item}: no compare or AI action"),
                });
            }
        }
        for rev in revs {
            assert_eq!(git(&["rev-parse", &rev]), branch, "{rev} is the tag");
        }
    }
}
