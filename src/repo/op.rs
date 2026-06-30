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
    /// A Settings → Tools command: its label is the user's free text, so the
    /// label-keyed follow-ups (commit box, split, compose) must not fire.
    pub custom: bool,
}

/// What a discard is about to destroy.
#[derive(Clone, Debug)]
pub enum Backup {
    /// These paths (tracked or untracked).
    Paths(Vec<String>),
    /// Like `Paths`, but undo writes untracked files back even where the op
    /// recreated them (a restore from history over an ignored file).
    Overwrite(Vec<String>),
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
            custom: false,
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
                [STASH_LIST | STASH_DROP | STASH_HAS, ..] => stash_cmd(dir, &a),
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

/// Discard one unstaged change (WIP list), backed up so ⌘Z brings the bytes back.
pub fn discard_op(c: &git::Change) -> Op {
    let o = match (c.code, &c.old_path) {
        ('?', _) => Op::new("Discard", &["--literal-pathspecs", "clean", "-fq", "--", &c.path]),
        // Intent-to-add (`add -N`): `checkout` would empty the file instead of removing it.
        ('A' | 'R' | 'C', old) => {
            let o = Op::new("Discard", &["--literal-pathspecs", "rm", "-fq", "--", &c.path]);
            match old.as_deref().filter(|_| c.code == 'R') {
                Some(old) => o.then(&["--literal-pathspecs", "checkout", "--", old]),
                None => o,
            }
        }
        _ => Op::new("Discard", &["--literal-pathspecs", "checkout", "--", &c.path]),
    };
    o.light().backup(Backup::Paths(vec![c.path.clone()])).confirm(format!("Discard changes to {}? (Undo with ⌘Z.)", c.path))
}

/// Unstage one file; a staged rename needs its old path reset too, or its deletion stays staged.
pub fn unstage_op(c: &git::Change) -> Op {
    let mut args = vec!["--literal-pathspecs", "reset", "-q", "--"];
    args.extend(c.old_path.as_deref());
    args.push(&c.path);
    Op::new("Unstage", &args).light()
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

/// `exec` pseudo-commands that name stashes by commit id, since `stash@{n}` shifts whenever
/// a stash is made or dropped (CLI, another tab): `STASH_LIST n ids..` fails unless the
/// first `n` stashes are exactly `ids`, `STASH_DROP id` drops wherever `id` is now, and
/// `STASH_HAS ids..` fails unless every commit still exists.
const STASH_LIST: &str = "!stash-list";
const STASH_DROP: &str = "!stash-drop";
const STASH_HAS: &str = "!stash-has";

fn stash_cmd(dir: &Path, a: &[&str]) -> Res<String> {
    let short = |id: &str| id[..7.min(id.len())].to_owned();
    match a {
        [STASH_LIST, n, ids @ ..] => {
            let n: usize = n.parse().map_err(|_| "bad stash count".to_owned())?;
            let cur = git::stashes(dir)?;
            if cur.iter().take(n).map(|s| s.id.as_str()).eq(ids.iter().copied()) {
                Ok(String::new())
            } else {
                Err("The stash list changed since, so no stash was touched. Check the stashes and try again.".into())
            }
        }
        [STASH_DROP, id] => match git::stashes(dir)?.into_iter().find(|s| s.id == *id) {
            Some(s) => git::run(dir, &["stash", "drop", &s.name]),
            None => Err(format!("Stash {} is no longer in the stash list.", short(id))),
        },
        [STASH_HAS, ids @ ..] => match ids.iter().find(|id| git::run(dir, &["cat-file", "-e", &format!("{id}^{{commit}}")]).is_err()) {
            Some(id) => Err(format!("Stash {} no longer exists (git gc pruned it), so no stash was touched.", short(id))),
            None => Ok(String::new()),
        },
        _ => Err(format!("bad stash command {a:?}")),
    }
}

/// Drop these stashes in one op, by commit id, after checking the list is still the one shown.
/// `git stash store` only pushes on top, so ⌘Z rebuilds the stack down to the
/// deepest dropped stash: it pops the survivors above it, then re-stores all of
/// them oldest first, which brings back the order and the `stash@{n}` numbers.
pub fn drop_stashes(all: &[git::Stash], ids: &[String]) -> Op {
    let dropped: Vec<&git::Stash> = all.iter().filter(|s| ids.contains(&s.id)).collect();
    let (n, deepest) = (dropped.len(), all.iter().rposition(|s| ids.contains(&s.id)).unwrap_or(0));
    let (label, ask) = match dropped[..] {
        [s] => ("Drop stash".to_owned(), format!("Delete {}? (Undo with ⌘Z.)", s.name)),
        _ => (format!("Drop {n} stashes"), format!("Delete {n} stashes? (Undo with ⌘Z.)")),
    };
    let check = |n: usize, ss: &[&git::Stash]| [STASH_LIST.to_owned(), n.to_string()].into_iter().chain(ss.iter().map(|s| s.id.clone())).collect();
    let by_id = |cmd: &str, ss: &[&git::Stash]| ss.iter().map(|s| vec![cmd.to_owned(), s.id.clone()]).collect::<Vec<_>>();
    // The undo below is built from `all`, and redo reruns this op: both need the list unchanged.
    let mut op = Op::new(label, &[]).confirm(ask);
    op.cmds = vec![check(all.len() + 1, &all.iter().collect::<Vec<_>>())];
    op.cmds.extend(by_id(STASH_DROP, &dropped));
    // Check everything before popping: the survivors above the deepest drop and the one
    // below it must be where the drop left them, and every dropped commit must still exist.
    let survivors: Vec<&git::Stash> = all.iter().filter(|s| !ids.contains(&s.id)).collect();
    let above = deepest + 1 - n;
    let mut back = Op::new(format!("Restore {n} stashes"), &[]);
    back.cmds = vec![[STASH_HAS.to_owned()].into_iter().chain(dropped.iter().map(|s| s.id.clone())).collect()];
    back.cmds.push(check(above + 1, &survivors[..(above + 1).min(survivors.len())]));
    back.cmds.extend(by_id(STASH_DROP, &survivors[..above]));
    back.cmds.extend(all[..=deepest].iter().rev().map(|s| ["stash", "store", "-m", &s.message, &s.id].map(String::from).to_vec()));
    op.revert(back)
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
                // Even a partly failed op may have moved refs: always offer undo.
                let snaps = before.and_then(|b| git::snapshot(&dir).ok().map(|a| (b, a)));
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
                if res.is_ok() && !op.custom && op.label.starts_with("Commit") {
                    self.summary.clear();
                    self.body.clear();
                    self.amend = false;
                }
                if let Some((before, after)) = snaps.filter(|(b, a)| b != a) {
                    // Git's own `--autostash` (pull, cherry-pick): `reset --keep` would refuse edits the op merged into.
                    let autostash = op.autostash.clone().or_else(|| {
                        (op.undo == Undo::Keep && op.cmds.iter().flatten().any(|a| a == "--autostash")).then(|| format!("autostash to undo {}", op.label))
                    });
                    self.undo.push(UndoEntry::new(op.label.clone(), UndoKind::Refs { before, after, mode: op.undo, autostash }));
                    self.redo.clear();
                }
                if let (Ok(_), Some(undo)) = (&res, &op.revert) {
                    let redo = Op { confirm: None, revert: None, ..op.clone() };
                    self.undo.push(UndoEntry::new(op.label.clone(), UndoKind::Op { undo: undo.clone(), redo: Box::new(redo) }));
                    self.redo.clear();
                }
                if let Some(saved) = saved.filter(|s| (s.stash.is_some() && !s.tracked.is_empty()) || !s.files.is_empty()) {
                    self.undo.push(UndoEntry::new(op.label.clone(), UndoKind::Discard(saved)));
                    self.redo.clear();
                    // Light ops toast nothing, but a discard deserves a visible way back.
                    if op.light && res.is_ok() {
                        msgs.push(Msg { text: format!("{} ✔ changes backed up", op.label), error: false });
                    }
                }
                // A guided split ends (or never started): see `split.rs`.
                match (op.label.as_str(), res.is_ok()) {
                    _ if op.custom => {}
                    (super::split::FINISH, true) => self.finish_split(),
                    (super::split::FINISH, false) => self.split = None,
                    (super::split::ABORT, true) => self.split_aborted(),
                    (l, false) if l.starts_with("Split ") => self.split = None,
                    // Show the WIP panel with the uncommitted parts (selected on reload).
                    (l, true) if l.starts_with("Split ") => self.sel = None,
                    // A failed compose step leaves the original HEAD's tree staged, which would
                    // revert the new commits: unstage, so only the uncommitted rest remains.
                    (l, false) if l.starts_with("Compose ") => self.run(ctx, Op::new("Unstage all", &["reset", "-q"]).light()),
                    _ => {}
                }
                // Hunk buttons patch the shown diff: mark it stale now, not after the status reload,
                // or a second click re-applies the old patch wherever its context still matches.
                self.refresh_diff(ctx);
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

#[cfg(test)]
mod tests {
    use super::super::testutil::{commit, identity, scratch, settle, sh};
    use super::super::{Action, RepoTab, View};

    /// Unstaging a `git mv` rename unstages both halves: no lone deletion left staged.
    #[test]
    fn unstage_rename() {
        let dir = scratch("unstage-rename");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "old", "a\nb\nc\nd\n");
        sh(&dir, &["mv", "old", "new"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let c = t.data.as_ref().unwrap().status.staged[0].clone();
        assert_eq!((c.path.as_str(), c.old_path.as_deref()), ("new", Some("old")));
        t.apply(&ctx, Action::Git(super::unstage_op(&c)));
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["status", "--porcelain"]), " D old\n?? new\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Deleting non-adjacent stashes, then ⌘Z: the same stashes come back at the same positions.
    #[test]
    fn drop_stashes_and_undo() {
        let dir = scratch("drop-stashes");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base");
        for m in ["one", "two", "three"] {
            std::fs::write(dir.join("a"), m).unwrap();
            sh(&dir, &["stash", "push", "-q", "-m", m]);
        }
        let list = || sh(&dir, &["stash", "list", "--format=%gd %H %gs"]);
        let orig = list();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let all = t.data.as_ref().unwrap().stashes.clone();
        let mut op = super::drop_stashes(&all, &[all[0].id.clone(), all[2].id.clone()]);
        assert_eq!(op.label, "Drop 2 stashes");
        op.confirm = None;
        t.apply(&ctx, Action::Git(op));
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["stash", "list", "--format=%gs"]), "On main: two\n");
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(list(), orig);
        assert!(orig.starts_with("stash@{0} ") && orig.contains("stash@{2} ") && orig.ends_with("On main: one\n"));
        t.apply(&ctx, Action::Redo);
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["stash", "list", "--format=%gs"]), "On main: two\n");
        // A stash made after the drop sits where undo pops: it refuses instead of losing it.
        std::fs::write(dir.join("a"), "four").unwrap();
        sh(&dir, &["stash", "push", "-q", "-m", "four"]);
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["stash", "list", "--format=%gs"]), "On main: four\nOn main: two\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stashes made, popped or pruned behind the app's back: the drop, its undo and its redo
    /// refuse instead of dropping or popping any stash but the ones the user deleted.
    #[test]
    fn drop_stashes_refuses_when_stashes_changed() {
        let dir = scratch("drop-stashes-changed");
        let ctx = egui::Context::default();
        let push = |m: &str| {
            std::fs::write(dir.join("a"), m).unwrap();
            sh(&dir, &["stash", "push", "-q", "-m", m]);
        };
        let list = || sh(&dir, &["stash", "list", "--format=%s"]).replace("On main: ", "").lines().collect::<Vec<_>>().join(" ");
        let setup = |msgs: &[&str]| {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            sh(&dir, &["init", "-q", "-b", "main"]);
            identity(&dir);
            commit(&dir, "a", "base");
            msgs.iter().for_each(|m| push(m));
            let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
            settle(&ctx, &mut t);
            t
        };
        let drop = |t: &mut RepoTab, at: &[usize]| {
            let all = t.data.as_ref().unwrap().stashes.clone();
            let mut op = super::drop_stashes(&all, &at.iter().map(|&i| all[i].id.clone()).collect::<Vec<_>>());
            op.confirm = None;
            op
        };
        let act = |t: &mut RepoTab, a: Action| {
            t.apply(&ctx, a);
            settle(&ctx, t);
        };

        // Redo after a new stash: it keeps the new one.
        let mut t = setup(&["s0", "s1"]);
        let op = drop(&mut t, &[0]);
        act(&mut t, Action::Git(op));
        act(&mut t, Action::Undo);
        push("new");
        assert_eq!(t.redo_label(), Some("Drop stash"));
        act(&mut t, Action::Redo);
        assert_eq!(list(), "new s1 s0");

        // Undo after a survivor in the rebuilt part was popped: s0 below is kept.
        let mut t = setup(&["s0", "s1", "s2", "s3", "s4"]);
        let op = drop(&mut t, &[0, 3]);
        act(&mut t, Action::Git(op));
        assert_eq!(list(), "s3 s2 s0");
        sh(&dir, &["stash", "drop", "-q", "stash@{1}"]);
        act(&mut t, Action::Undo);
        assert_eq!(list(), "s3 s0");

        // Undo after gc pruned the dropped commit: the survivors stay.
        let mut t = setup(&["s0", "s1", "s2"]);
        let op = drop(&mut t, &[2]);
        act(&mut t, Action::Git(op));
        sh(&dir, &["gc", "-q", "--prune=now"]);
        act(&mut t, Action::Undo);
        assert_eq!(list(), "s2 s1");

        // A stash made while the delete waited for confirmation.
        let mut t = setup(&["s0", "s1"]);
        let op = drop(&mut t, &[0]);
        push("ext");
        act(&mut t, Action::Git(op));
        assert_eq!(list(), "ext s1 s0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rename git sees in the worktree (`git add -N new` after `mv old new`).
    #[test]
    fn worktree_rename_stage_all_and_discard() {
        let dir = scratch("wt-rename");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "old.txt", "keep me\n");
        let renamed = || {
            std::fs::rename(dir.join("old.txt"), dir.join("new.txt")).unwrap();
            sh(&dir, &["add", "-N", "new.txt"]);
            let s = crate::git::status(&dir).unwrap();
            assert_eq!(s.unstaged[0].old_path.as_deref(), Some("old.txt"));
            s
        };
        super::exec(&dir, &super::stage_all_op(&renamed()).unwrap()).unwrap();
        let s = crate::git::status(&dir).unwrap();
        assert!(s.unstaged.is_empty(), "{:?}", s.unstaged);
        assert_eq!((s.staged[0].code, s.staged[0].old_path.as_deref()), ('R', Some("old.txt")));
        sh(&dir, &["reset", "-q", "--hard"]);

        super::exec(&dir, &super::discard_op(&renamed().unstaged[0])).unwrap();
        let s = crate::git::status(&dir).unwrap();
        assert!(s.unstaged.is_empty() && s.staged.is_empty(), "{:?}", s.unstaged);
        assert!(!dir.join("new.txt").exists());
        assert_eq!(std::fs::read_to_string(dir.join("old.txt")).unwrap(), "keep me\n");

        // An intent-to-add new file is removed, not truncated to 0 bytes.
        std::fs::write(dir.join("n.txt"), "draft\n").unwrap();
        sh(&dir, &["add", "-N", "n.txt"]);
        super::exec(&dir, &super::discard_op(&crate::git::status(&dir).unwrap().unstaged[0])).unwrap();
        assert!(!dir.join("n.txt").exists() && crate::git::status(&dir).unwrap().unstaged.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
