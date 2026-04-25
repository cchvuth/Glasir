//! Right-click menus of refs (sidebar rows and graph pills): local branch,
//! remote branch and tag menus.

use crate::git::RefKind;
use crate::repo::{Action, Op, RepoData, Undo, branch_checkout, op, prompt};
use crate::ui::graph_view::menus::integrate_items;
use egui::Ui;

pub fn ref_menu(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    match r.kind {
        RefKind::Local => local_menu(ui, d, r, acts),
        RefKind::Remote => remote_menu(ui, d, r, acts),
        RefKind::Tag => tag_menu(ui, d, r, acts),
    }
}

/// `r` is the branch (local or remote-tracking) whose PR / web pages these open.
fn web_items(ui: &mut Ui, d: &RepoData, r: &crate::git::Ref, acts: &mut Vec<Action>) {
    let (remote, branch) = match r.kind {
        RefKind::Remote => (r.remote().to_owned(), r.remote_branch().to_owned()),
        _ => (r.upstream.split('/').next().filter(|s| !s.is_empty()).unwrap_or("origin").to_owned(), r.name.clone()),
    };
    let Some(web) = d.web_of(&remote) else { return };
    ui.separator();
    let enc = crate::git::url_path;
    if ui.button("Create pull request…").on_hover_text("Opens the compare page on GitHub / GitHub Enterprise").clicked() {
        acts.push(Action::Url(format!("{web}/compare/{}...{}?expand=1", enc(&d.default_branch()), enc(&branch))));
    }
    if ui.button("Open branch on remote").clicked() {
        acts.push(Action::Url(format!("{web}/tree/{}", enc(&branch))));
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
        if let Some((remote, rb)) = r.upstream.split_once('/').filter(|_| !r.gone)
            && ui.button(format!("Pull {name} (fast-forward, without checkout)")).clicked()
        {
            acts.push(Action::Git(Op::new(format!("Pull {name}"), &["fetch", remote, &format!("{rb}:{name}")]).undo(Undo::Keep)));
        }
        integrate_items(ui, d, name, acts);
    }
    ui.separator();
    if let Some(remote) = d.remotes.first().map(|r| r.0.clone()) {
        let remote = if r.upstream.is_empty() { remote } else { r.upstream.split('/').next().unwrap_or("origin").to_owned() };
        if ui.button(format!("Push {name} to {remote}")).clicked() {
            acts.push(op(format!("Push {name}"), &["push", "-u", &remote, name]));
        }
    }
    if ui.button("Create branch from here…").clicked() {
        acts.push(prompt(format!("New branch from {name}"), "", Op::new("Create branch", &["checkout", "-b", "{}", name]).undo(Undo::Keep)));
    }
    if ui.button("Rename…").clicked() {
        acts.push(prompt(format!("Rename {name}"), name, Op::new("Rename branch", &["branch", "-m", name, "{}"]).undo(Undo::Keep)));
    }
    let default_up = if r.upstream.is_empty() {
        format!("{}/{name}", d.remotes.first().map_or("origin", |r| r.0.as_str()))
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
            acts.push(Action::Git(Op::new(format!("Delete {name}"), &["branch", "-d", name]).undo(Undo::Keep)));
        }
        if ui.button("Force delete").clicked() {
            acts.push(Action::Git(
                Op::new(format!("Delete {name}"), &["branch", "-D", name])
                    .undo(Undo::Keep)
                    .confirm(format!("Delete {name} even if it is not merged? (You can undo this.)")),
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
    integrate_items(ui, d, &r.name, acts);
    ui.separator();
    if ui.button(format!("Delete {} from {}", r.remote_branch(), r.remote())).clicked() {
        acts.push(Action::Git(
            Op::new(format!("Delete {}", r.name), &["push", r.remote(), "--delete", r.remote_branch()])
                .confirm(format!("Delete branch {} on remote {}?", r.remote_branch(), r.remote())),
        ));
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
    if let Some((remote, _)) = d.remotes.first() {
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
    if ui.button("Delete tag").clicked() {
        acts.push(Action::Git(Op::new(format!("Delete tag {}", r.name), &["tag", "-d", &r.name]).undo(Undo::Keep)));
    }
    if ui.button("Copy tag name").clicked() {
        acts.push(Action::Copy(r.name.clone()));
    }
}
