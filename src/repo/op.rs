//! Git mutations: the `Op` builder, running ops off the UI thread, finishing
//! them (toasts, undo entries, reloads) and common op builders (push/pull/fetch/commit).

use super::action::{Action, Msg, MultiOp};
use super::data::RepoData;
use super::offer::offer;
use super::undo::{UndoEntry, UndoKind, save_worktree, Saved};
use super::task::Task;
use super::RepoTab;
use crate::git::{self, Ref, RefKind, Res, Snapshot};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Whether an op can be undone, and the `git reset` mode that undoes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Undo {
    No,
    /// Commits and soft resets: the undone changes stay staged.
    Soft,
    Mixed,
    /// Everything else (checkout, merge, rebase, branch ops...): `reset --keep`.
    Keep,
}

impl Undo {
    pub(super) fn mode(self) -> &'static str {
        match self {
            Undo::Soft => "--soft",
            Undo::Mixed => "--mixed",
            _ => "--keep",
        }
    }
}

/// One or more git commands run in sequence on a background thread.
#[derive(Clone, Debug)]
pub struct Op {
    pub label: String,
    pub cmds: Vec<Vec<String>>,
    pub input: Option<String>,
    pub confirm: Option<String>,
    pub undo: Undo,
    /// Staging-type op: no toast, refresh status only.
    pub light: bool,
    /// Discards: back up what will be lost first, so the discard can be undone.
    pub backup: Option<Backup>,
    /// Open this path in a new tab when the op succeeds (e.g. a new worktree).
    pub then_open: Option<String>,
    /// Stash uncommitted changes with this message first, reapply them after.
    pub autostash: Option<String>,
    /// Undoes a config-only op (remotes): ⌘Z runs it, redo runs this op again.
    pub revert: Option<Box<Op>>,
}

/// What a discard is about to destroy.
#[derive(Clone, Debug)]
pub enum Backup {
    /// These paths (tracked or untracked).
    Paths(Vec<String>),
    /// Every uncommitted change, including untracked files.
    All,
}

impl Op {
    pub fn new(label: impl Into<String>, args: &[&str]) -> Self {
        Op {
            label: label.into(),
            cmds: vec![args.iter().map(|s| s.to_string()).collect()],
            input: None,
            confirm: None,
            undo: Undo::No,
            light: false,
            backup: None,
            then_open: None,
            autostash: None,
            revert: None,
        }
    }
    pub fn revert(mut self, op: Op) -> Self {
        self.revert = Some(Box::new(op));
        self
    }
    pub fn then_open(mut self, path: impl Into<String>) -> Self {
        self.then_open = Some(path.into());
        self
    }
    /// Make a discard undoable.
    pub fn backup(mut self, b: Backup) -> Self {
        self.backup = Some(b);
        self
    }
    pub fn then(mut self, args: &[&str]) -> Self {
        self.cmds.push(args.iter().map(|s| s.to_string()).collect());
        self
    }
    pub fn confirm(mut self, text: impl Into<String>) -> Self {
        self.confirm = Some(text.into());
        self
    }
    pub fn undo(mut self, u: Undo) -> Self {
        self.undo = u;
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

pub(super) enum Done {
    Op(Op, Res<String>, Option<(Snapshot, Snapshot)>, Option<Saved>),
    Undo(bool, Res<()>),
}

/// Runs an op's commands in order, stopping at the first failure.
pub(super) fn exec(dir: &Path, op: &Op) -> Res<String> {
    let all = || {
        let mut out = Ok(String::new());
        for args in &op.cmds {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            out = match a.as_slice() {
                [super::tools::SHELL, line] => git::run_shell(dir, line),
                _ => git::run_input(dir, &a, op.input.as_deref().map(str::as_bytes)),
            };
            if out.is_err() {
                break;
            }
        }
        out
    };
    match &op.autostash {
        Some(msg) => git::with_autostash(dir, msg, all),
        None => all(),
    }
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
    Op::new(label, &["pull", flag, "--autostash", "--progress"]).undo(Undo::Keep)
}

pub fn fetch_op() -> Op {
    Op::new("Fetch", &["fetch", "--all", "--prune", "--progress"])
}

/// Stage every unstaged and untracked change, but never a conflicted file: `add -A`
/// would mark it resolved, markers and all. None when there's nothing to stage (an
/// empty pathspec list would stage everything). Paths go over stdin: no argv limit.
pub fn stage_all_op(s: &git::Status) -> Option<Op> {
    let paths: String = s.unstaged.iter().flat_map(|c| c.old_path.iter().chain([&c.path])).map(|p| format!("{p}\0")).collect();
    (!paths.is_empty()).then(|| Op::new("Stage all", &["--literal-pathspecs", "add", "-A", "--pathspec-from-file=-", "--pathspec-file-nul"]).input(paths).light())
}

/// Stash everything, untracked files included (toolbar, WIP menu, palette).
pub fn stash_op() -> Op {
    Op::new("Stash", &["stash", "push", "--include-untracked"])
}

/// `git branch -d` (or `-D`) of local branches. ⌘Z recreates them with their
/// `branch.<name>.*` config (upstream, description), which git drops and a ref snapshot lacks.
pub fn delete_branches(d: &RepoData, label: String, names: &[&str], force: bool) -> Op {
    let mut args = vec!["branch", if force { "-D" } else { "-d" }];
    args.extend(names);
    let mut back = Op::new(format!("Restore {}", names.join(", ")), &[]);
    back.cmds.clear();
    for (n, r) in names.iter().filter_map(|n| Some((n, d.local(n)?))) {
        back.cmds.push(vec!["branch".into(), n.to_string(), r.target.clone()]);
        let prefix = format!("branch.{n}.");
        // Only this branch's own keys, not those of a branch `<n>.x`.
        let own = d.config.iter().filter(|(k, _)| k.strip_prefix(&prefix).is_some_and(|v| !v.contains('.')));
        back.cmds.extend(own.map(|(k, v)| vec!["config".into(), "--add".into(), k.clone(), v.clone()]));
    }
    Op::new(label, &args).revert(back)
}

/// Check out a ref: local branch, remote branch (its local twin or a new
/// tracking branch), or a tag (detached). A branch checked out in another
/// worktree opens that worktree instead.
pub fn branch_checkout(d: &RepoData, r: &Ref) -> Option<Action> {
    match r.kind {
        RefKind::Local if r.head => None,
        // Checked out in another worktree: git refuses a second checkout, so open that one.
        RefKind::Local if r.worktree.is_some() => r.worktree.clone().map(Action::OpenTab),
        RefKind::Local => Some(Action::Git(Op::new(format!("Checkout {}", r.name), &["checkout", &r.name, "--"]).undo(Undo::Keep))),
        RefKind::Remote => {
            let local = r.remote_branch();
            Some(Action::Git(if d.local(local).is_some() {
                Op::new(format!("Checkout {local}"), &["checkout", local, "--"]).undo(Undo::Keep)
            } else {
                Op::new(format!("Checkout {}", r.name), &["checkout", "--track", &r.name]).undo(Undo::Keep)
            }))
        }
        RefKind::Tag => Some(Action::Git(Op::new(format!("Checkout {}", r.name), &["checkout", "--detach", &r.full]).undo(Undo::Keep))),
    }
}

impl RepoTab {
    /// Runs `op` (after a confirmation dialog if it asks for one).
    pub fn run(&mut self, ctx: &egui::Context, op: Op) {
        if let [cmd, commit, msg] = op.cmds[0].as_slice()
            && cmd == "__reword__"
        {
            let old = git::run(&self.dir, &["log", "-1", "--format=%B", commit]).unwrap_or_default();
            let body = old.trim_end().split_once('\n').map(|(_, b)| b.trim_start_matches('\n').to_owned()).unwrap_or_default();
            let message = if body.is_empty() { msg.clone() } else { format!("{msg}\n\n{body}") };
            if self.data.as_ref().and_then(|d| d.status.head.as_deref()) == Some(commit.as_str()) {
                let op = Op::new("Reword HEAD", &["commit", "--amend", "--only", "-F", "-"]).input(message).undo(Undo::Soft);
                return self.run(ctx, op);
            }
            return self.apply(ctx, Action::Rewrite { commit: commit.clone(), act: crate::repo::rebase::Act::Reword, message: Some(message) });
        }
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
                let saved = match op.backup.as_ref().map(|b| save_worktree(&dir, b)) {
                    Some(Err(e)) => return Done::Op(op, Err(e), None, None),
                    s => s.and_then(Result::ok),
                };
                let before = (op.undo != Undo::No).then(|| git::snapshot(&dir).ok()).flatten();
                let out = cancel.scope(|| exec(&dir, &op));
                // Even a partly failed op may have moved refs: offer undo, unless it stopped mid
                // merge/rebase/cherry-pick (undo can't check out over that; Abort is the way back).
                let stopped = out.is_err() && git::git_dir(&dir).is_ok_and(|g| !matches!(git::state(&g), git::State::Clean | git::State::Bisecting));
                let snaps = before.filter(|_| !stopped).and_then(|b| git::snapshot(&dir).ok().map(|a| (b, a)));
                Done::Op(op, out, snaps, saved)
            }),
        ));
    }

    /// Finishes the running op once its thread is done.
    pub(super) fn poll_op(&mut self, ctx: &egui::Context, msgs: &mut Vec<Msg>) {
        let Some(done) = self.running.as_mut().and_then(|(_, t)| t.0.try_recv().ok()) else { return };
        self.running = None;
        self.cancel = None;
        match done {
            Done::Op(op, res, snaps, saved) => {
                match &res {
                    Ok(out) if !op.light => {
                        // A custom shell command's output is the point: show its last lines.
                        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
                        let shown = if op.cmds[0][0] == super::tools::SHELL { lines[lines.len().saturating_sub(6)..].join("\n") } else { lines.first().copied().unwrap_or("").to_owned() };
                        let sep = if shown.contains('\n') { "\n" } else { " " };
                        msgs.push(Msg { text: format!("{} ✔{sep}{shown}", op.label), error: false });
                    }
                    Ok(_) => {}
                    Err(e) if e == git::CANCELLED => msgs.push(Msg { text: format!("{} cancelled", op.label), error: false }),
                    Err(e) => match offer(&op, e, self.data.as_ref()) {
                        Some(c) => self.choice = Some(c),
                        None => msgs.push(Msg { text: format!("{} failed:\n{e}", op.label), error: true }),
                    },
                }
                if res.is_ok() && matches!(op.cmds[0][0].as_str(), "fetch" | "pull") {
                    self.fetch_failed = None;
                }
                if let (Ok(_), Some(p)) = (&res, &op.then_open) {
                    self.open_tab = Some(PathBuf::from(p));
                }
                if res.is_ok() && op.label.starts_with("Commit") {
                    self.summary.clear();
                    self.body.clear();
                    self.amend = false;
                }
                if let Some((before, after)) = snaps.filter(|(b, a)| b != a) {
                    self.undo.push(UndoEntry { label: op.label.clone(), kind: UndoKind::Refs { before, after, mode: op.undo, autostash: op.autostash.clone() } });
                    self.redo.clear();
                }
                if let (Ok(_), Some(undo)) = (&res, &op.revert) {
                    let redo = Op { confirm: None, revert: None, ..op.clone() };
                    self.undo.push(UndoEntry { label: op.label.clone(), kind: UndoKind::Op { undo: undo.clone(), redo: Box::new(redo) } });
                    self.redo.clear();
                }
                if let Some(saved) = saved.filter(|s| (s.stash.is_some() && !s.tracked.is_empty()) || !s.files.is_empty()) {
                    self.undo.push(UndoEntry { label: op.label.clone(), kind: UndoKind::Discard(saved) });
                    self.redo.clear();
                    // Light ops toast nothing, but a discard deserves a visible way back.
                    if op.light && res.is_ok() {
                        msgs.push(Msg { text: format!("{} ✔ changes backed up", op.label), error: false });
                    }
                }
                // A guided split ends (or never started): see `split.rs`.
                match (op.label.as_str(), res.is_ok()) {
                    (super::split::FINISH, true) => self.finish_split(),
                    (super::split::FINISH, false) => self.split = None,
                    (l, false) if l.starts_with("Split ") => self.split = None,
                    // Show the WIP panel with the uncommitted parts (selected on reload).
                    (l, true) if l.starts_with("Split ") => self.sel = None,
                    // A failed compose step leaves the original HEAD's tree staged, which would
                    // revert the new commits: unstage, so only the uncommitted rest remains.
                    (l, false) if l.starts_with("Compose ") => self.run(ctx, Op::new("Unstage all", &["reset", "-q"]).light()),
                    _ => {}
                }
                self.reload(ctx, !op.light);
            }
            Done::Undo(was_undo, res) => {
                let (from, to) = if was_undo { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
                match res {
                    Ok(()) => to.extend(from.pop().filter(|e| !matches!(e.kind, UndoKind::Discard(_)))),
                    Err(e) => {
                        // Ref history is unreliable now, but discarded work must stay recoverable.
                        from.retain(|e| matches!(e.kind, UndoKind::Discard(_)));
                        msgs.push(Msg { text: format!("Undo/redo failed:\n{e}"), error: true });
                    }
                }
                self.reload(ctx, true);
            }
        }
        if let Some(r) = &mut self.reflog {
            r.refresh(ctx, &self.dir);
        }
    }

    pub(super) fn multi_op(&mut self, ctx: &egui::Context, kind: MultiOp, ids: Vec<String>) {
        let Some(d) = &self.data else { return };
        if ids.iter().any(|id| d.row_of.get(id).and_then(|&r| d.commit(r)).is_some_and(|c| c.parents.len() > 1)) {
            return self.notes.push(Msg { text: "Merge commits can't be part of a multi-commit action.".into(), error: true });
        }
        let git_dir = d.git_dir.clone();
        let n = ids.len();
        let op = match kind {
            MultiOp::CherryPickEdit => {
                match crate::repo::rebase::Editor::cherry_pick(&self.dir, &ids) {
                    Ok(ed) => (self.diff, self.merge, self.rebase) = (None, None, Some(ed)),
                    Err(e) => self.notes.push(Msg { text: e, error: true }),
                }
                return;
            }
            MultiOp::CherryPick => {
                let mut args = vec!["cherry-pick"];
                args.extend(ids.iter().rev().map(String::as_str)); // oldest first
                Ok(Op::new(format!("Cherry-pick {n} commits"), &args).undo(Undo::Keep))
            }
            MultiOp::Revert => {
                let mut args = vec!["revert", "--no-edit"];
                args.extend(ids.iter().map(String::as_str)); // newest first
                Ok(Op::new(format!("Revert {n} commits"), &args).undo(Undo::Keep))
            }
            MultiOp::Squash => crate::repo::rebase::multi(&self.dir, &git_dir, &ids, crate::repo::rebase::Act::Squash),
            MultiOp::Drop => crate::repo::rebase::multi(&self.dir, &git_dir, &ids, crate::repo::rebase::Act::Drop)
                .map(|o| o.confirm(format!("Drop {n} commits from the current branch? (Undo with ⌘Z.)"))),
        };
        match op {
            Ok(op) => {
                self.multi.clear();
                self.run(ctx, op)
            }
            Err(e) => self.notes.push(Msg { text: e, error: true }),
        }
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
        Op::new(if self.amend { "Commit (amend)" } else { "Commit" }, &args).input(msg).undo(Undo::Soft)
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
        let Some((remote, rb)) = self.status.upstream.as_deref().and_then(|up| self.push_dest(branch, up)) else {
            let remote = &self.remotes.iter().find(|r| r.0 == "origin").or(self.remotes.first())?.0;
            return Some(Op::new("Push", &["push", "--progress", "-u", remote, branch]));
        };
        let up = format!("{remote}/{rb}");
        if !force {
            // A bare push keeps push.default / pushRemote, but `simple` refuses a differently named upstream.
            return Some(if self.status.upstream.as_deref() != Some(up.as_str()) {
                Op::new("Push", &["push", "--progress", "-u", remote, branch])
            } else if rb == branch {
                Op::new("Push", &["push", "--progress"])
            } else {
                Op::new("Push", &["push", "--progress", remote, &format!("{branch}:refs/heads/{rb}")])
            });
        }
        let sha = self.refs.iter().find(|r| r.kind == RefKind::Remote && r.name == up).map_or("", |r| r.target.as_str());
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
