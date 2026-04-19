//! Git mutations: the `Op` builder, running ops off the UI thread, finishing
//! them (toasts, reloads) and common op builders (push/pull/fetch/commit).

use super::action::{Action, Msg};
use super::data::RepoData;
use super::offer::offer;
use super::task::Task;
use super::RepoTab;
use crate::git::{self, Ref, RefKind, Res};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One or more git commands run in sequence on a background thread.
#[derive(Clone, Debug)]
pub struct Op {
    pub label: String,
    pub cmds: Vec<Vec<String>>,
    pub input: Option<String>,
    pub confirm: Option<String>,
    /// Staging-type op: no toast, refresh status only.
    pub light: bool,
}

impl Op {
    pub fn new(label: impl Into<String>, args: &[&str]) -> Self {
        Op {
            label: label.into(),
            cmds: vec![args.iter().map(|s| s.to_string()).collect()],
            input: None,
            confirm: None,
            light: false,
        }
    }
    pub fn then(mut self, args: &[&str]) -> Self {
        self.cmds.push(args.iter().map(|s| s.to_string()).collect());
        self
    }
    pub fn confirm(mut self, text: impl Into<String>) -> Self {
        self.confirm = Some(text.into());
        self
    }
    pub fn light(mut self) -> Self {
        self.light = true;
        self
    }
    pub fn input(mut self, s: String) -> Self {
        self.input = Some(s);
        self
    }
}

/// Runs an op's commands in order, stopping at the first failure.
pub(super) fn exec(dir: &Path, op: &Op) -> Res<String> {
    let mut out = Ok(String::new());
    for args in &op.cmds {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        out = git::run_input(dir, &a, op.input.as_deref().map(str::as_bytes));
        if out.is_err() {
            break;
        }
    }
    out
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub enum PullMode {
    Merge,
    FfOnly,
    Rebase,
}

pub fn pull_op(mode: PullMode) -> Op {
    let (label, flag) = match mode {
        PullMode::Merge => ("Pull", "--no-rebase"),
        PullMode::FfOnly => ("Pull (fast-forward only)", "--ff-only"),
        PullMode::Rebase => ("Pull (rebase)", "--rebase"),
    };
    Op::new(label, &["pull", flag, "--autostash", "--progress"])
}

pub fn fetch_op() -> Op {
    Op::new("Fetch", &["fetch", "--all", "--prune", "--progress"])
}

/// Check out a ref: local branch, remote branch (its local twin or a new
/// tracking branch), or a tag (detached).
pub fn branch_checkout(d: &RepoData, r: &Ref) -> Option<Action> {
    match r.kind {
        RefKind::Local if r.head => None,
        RefKind::Local => Some(Action::Git(Op::new(format!("Checkout {}", r.name), &["checkout", &r.name, "--"]))),
        RefKind::Remote => {
            let local = r.remote_branch();
            Some(Action::Git(if d.local(local).is_some() {
                Op::new(format!("Checkout {local}"), &["checkout", local, "--"])
            } else {
                Op::new(format!("Checkout {}", r.name), &["checkout", "--track", &r.name])
            }))
        }
        RefKind::Tag => Some(Action::Git(Op::new(format!("Checkout {}", r.name), &["checkout", "--detach", &r.full]))),
    }
}

impl RepoTab {
    /// Runs `op` (after a confirmation dialog if it asks for one).
    pub fn run(&mut self, ctx: &egui::Context, op: Op) {
        if op.confirm.is_some() {
            self.confirm = Some(op);
            return;
        }
        if let Some((busy, _)) = &self.running {
            self.notes.push(Msg { text: format!("'{}' skipped: '{busy}' is still running.", op.label), error: true });
            return;
        }
        let dir = self.dir.clone();
        let label = op.label.clone();
        let cancel = git::Cancel::default();
        self.cancel = Some(cancel.clone());
        self.running = Some((
            label,
            Task::spawn(ctx, move || {
                let out = cancel.scope(|| exec(&dir, &op));
                (op, out)
            }),
        ));
    }

    /// Finishes the running op once its thread is done.
    pub(super) fn poll_op(&mut self, ctx: &egui::Context, msgs: &mut Vec<Msg>) {
        let Some((op, res)) = self.running.as_mut().and_then(|(_, t)| t.0.try_recv().ok()) else { return };
        self.running = None;
        self.cancel = None;
        match &res {
            Ok(out) if !op.light => {
                let first = out.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                msgs.push(Msg { text: format!("{} ✔ {}", op.label, first), error: false });
            }
            Ok(_) => {}
            Err(e) if e == git::CANCELLED => msgs.push(Msg { text: format!("{} cancelled", op.label), error: false }),
            Err(e) => match offer(&op, e, self.data.as_ref()) {
                Some(c) => self.choice = Some(c),
                None => msgs.push(Msg { text: format!("{} failed:\n{e}", op.label), error: true }),
            },
        }
        if res.is_ok() && op.label.starts_with("Commit") {
            self.summary.clear();
            self.body.clear();
            self.amend = false;
        }
        self.reload(ctx, !op.light);
    }

    /// Op that commits the message in the editor.
    pub fn commit_op(&self) -> Op {
        let msg = if self.body.trim().is_empty() {
            self.summary.trim().to_owned()
        } else {
            format!("{}\n\n{}", self.summary.trim(), self.body.trim())
        };
        let mut args = vec!["commit", "-F", "-"];
        if self.amend {
            args.push("--amend");
        }
        Op::new(if self.amend { "Commit (amend)" } else { "Commit" }, &args).input(msg)
    }

    pub fn push_op(&self, force: bool) -> Option<Op> {
        self.data.as_ref()?.push_op(force)
    }
}

impl RepoData {
    /// Push the current branch, setting an upstream on first push. A force push
    /// leases the remote branch at the commit shown, so a (background) fetch can't
    /// make it overwrite commits you haven't seen.
    pub fn push_op(&self, force: bool) -> Option<Op> {
        let branch = self.status.branch.as_deref()?;
        let Some(up) = self.status.upstream.as_deref() else {
            let remote = &self.remotes.iter().find(|r| r.0 == "origin").or(self.remotes.first())?.0;
            return Some(Op::new("Push", &["push", "--progress", "-u", remote, branch]));
        };
        if !force {
            return Some(Op::new("Push", &["push", "--progress"]));
        }
        let (remote, rb, sha) = match self.refs.iter().find(|r| r.kind == RefKind::Remote && r.name == up) {
            Some(r) => (r.remote(), r.remote_branch(), r.target.as_str()),
            None => up.split_once('/').map(|(r, b)| (r, b, ""))?,
        };
        let (lease, spec) = (format!("--force-with-lease=refs/heads/{rb}:{sha}"), format!("{branch}:refs/heads/{rb}"));
        let args = ["push", "--progress", &lease, "--force-if-includes", remote, &spec];
        let at = if sha.is_empty() { "doesn't exist".into() } else { format!("is still at {}", &sha[..7.min(sha.len())]) };
        let text = format!(
            "Replace {up} with your {branch}? Commits on {up} that aren't in your branch will be lost. Only goes through if {up} {at}, as shown here.\n$ git {}",
            args.join(" ")
        );
        Some(Op::new("Force push", &args).confirm(text))
    }
}
