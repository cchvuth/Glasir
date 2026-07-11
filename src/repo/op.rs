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
    pub input: Option<Vec<u8>>,
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
    /// Run (even when cancelled) if a command fails: rolls a multi-step op back, stopping at a failing step.
    pub on_fail: Vec<Vec<String>>,
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
            on_fail: vec![],
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
    pub fn input(mut self, s: impl Into<Vec<u8>>) -> Self {
        self.input = Some(s.into());
        self
    }
}

pub(super) enum Done {
    Op(Op, Res<String>, Option<(Snapshot, Snapshot)>, Option<Saved>),
    Undo(bool, Res<()>),
}

/// Runs an op's commands in order, stopping at the first failure.
pub(super) fn exec(dir: &Path, op: &Op) -> Res<String> {
    let one = |args: &Vec<String>| {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        match a.as_slice() {
            [super::tools::SHELL, line] => git::run_shell(dir, line),
            [STASH_LIST | STASH_DROP | STASH_HAS | STASH_POP, ..] => stash_cmd(dir, &a),
            [BRANCH_DELETE | BRANCH_RESTORE, ..] => branch_cmd(dir, &a),
            [super::remotes::PR_GUARD, b, fetched, seen] => super::remotes::pr_guard(dir, b, fetched, seen),
            [super::diff::DIFF_IS, ..] => super::diff::diff_is(dir, &a),
            [super::rebase::HEAD_IS, want] => match super::rebase::head_state(dir) == *want {
                true => Ok(String::new()),
                false => Err("The branch changed since this was prepared, so nothing was rewritten. Try again.".into()),
            },
            [super::rebase::ABORT_ANY] => match git::state(&git::git_dir(dir)?) {
                git::State::Rebasing => git::run(dir, &["rebase", "--abort"]),
                _ => Ok(String::new()),
            },
            _ => git::run_input(dir, &a, op.input.as_deref()),
        }
    };
    let all = || {
        let mut out = Ok(String::new());
        for args in &op.cmds {
            out = one(args);
            if out.is_err() {
                if !op.on_fail.is_empty() {
                    // A later step after a failed one could hit the wrong ref (a still-detached rebase HEAD).
                    let back = git::Cancel::default().scope(|| op.on_fail.iter().try_for_each(|a| one(a).map(drop)));
                    out = out.map_err(|e| match back {
                        Err(b) => format!("{e}\nRolling back failed too, so the repository may be left part-way:\n{b}"),
                        Ok(()) if e == git::CANCELLED => e,
                        Ok(()) => format!("{e}\nRolled back to where it started."),
                    });
                }
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
        // Bare `--rebase` would flatten local merge commits, even over `pull.rebase=merges`.
        PullMode::Rebase => ("Pull (rebase)", "--rebase=merges"),
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
    // A rename's old path too, so undo deletes it again.
    let paths = c.old_path.iter().filter(|_| c.code == 'R').chain([&c.path]).cloned().collect();
    o.light().backup(Backup::Paths(paths)).confirm(format!("Discard changes to {}? (Undo with ⌘Z.)", c.path))
}

/// Unstage one file; a staged rename needs its old path reset too, or its deletion stays
/// staged. A copy's source is a separate change: resetting it would drop its staged content.
pub fn unstage_op(c: &git::Change) -> Op {
    let mut args = vec!["--literal-pathspecs", "reset", "-q", "--"];
    args.extend(c.old_path.as_deref().filter(|_| c.code == 'R'));
    args.push(&c.path);
    Op::new("Unstage", &args).light()
}

/// The in-progress banner's Skip / Abort of a merge, rebase, cherry-pick, revert or am.
/// Only `rebase --skip/--abort` run `reset --hard`, wiping unrelated uncommitted edits too.
pub fn sequencer_op(cmd: &str, abort: bool) -> Op {
    let (label, flag) = match (abort, cmd) {
        (true, _) => (format!("Abort {cmd}"), "--abort"),
        (false, "am") => ("Skip this patch".into(), "--skip"),
        _ => ("Skip this commit".into(), "--skip"),
    };
    let op = Op::new(label, &[cmd, flag]).undo(Undo::Keep);
    match (cmd, abort) {
        ("rebase", false) => op.backup(Backup::All).confirm("Skip discards all uncommitted changes, not just this commit's (⌘Z restores them)."),
        ("rebase", true) => op.backup(Backup::All).confirm("Abort discards all uncommitted changes (⌘Z restores them)."),
        _ => op,
    }
}

/// Stash everything, untracked files included (toolbar, WIP menu, palette).
pub fn stash_op() -> Op {
    Op::new("Stash", &["stash", "push", "--include-untracked"])
}

/// `git branch -d` (or `-D`) of local branches. ⌘Z recreates them with their
/// `branch.<name>.*` config (upstream, description), which git drops and a ref snapshot lacks.
/// Refuses if a branch moved since `d` was loaded: a redo, or a delete confirmed on a stale
/// view, must not delete commits nobody saw.
pub fn delete_branches(d: &RepoData, label: String, names: &[&str], force: bool) -> Op {
    let flag = if force { "-D" } else { "-d" };
    let (mut op, mut back) = (Op::new(label, &[]), Op::new(format!("Restore {}", names.join(", ")), &[]));
    (op.cmds, back.cmds) = (vec![], vec![]);
    for n in names {
        // A branch gone since it was shown (oid "") refuses the delete.
        op.cmds.push([BRANCH_DELETE, n, d.local(n).map_or("", |r| &r.target), flag].map(String::from).to_vec());
        let Some(r) = d.local(n) else { continue };
        let prefix = format!("branch.{n}.");
        // Only this branch's own keys, not those of a branch `<n>.x`.
        let own = d.config.iter().filter(|(k, _)| k.strip_prefix(&prefix).is_some_and(|v| !v.contains('.')));
        back.cmds.push([BRANCH_RESTORE, n, &r.target].map(String::from).into_iter().chain(own.flat_map(|(k, v)| [k.clone(), v.clone()])).collect());
    }
    op.revert(back)
}

/// `exec` pseudo-commands of a branch delete, one per branch so a partly failed delete
/// knows what it deleted: `BRANCH_DELETE name oid flag` deletes the branch only if it is
/// still at `oid` (undo would bring back that commit); `BRANCH_RESTORE name oid key value..`
/// recreates it with its config.
const BRANCH_DELETE: &str = "!branch-delete";
const BRANCH_RESTORE: &str = "!branch-restore";

fn branch_cmd(dir: &Path, a: &[&str]) -> Res<String> {
    match a {
        [BRANCH_DELETE, n, oid, flag] => {
            let now = git::run(dir, &["rev-parse", "--verify", "-q", &format!("refs/heads/{n}")]).unwrap_or_default();
            if now.trim() != *oid {
                return Err(format!("{n} changed since it was shown, so it wasn't deleted. Check it and try again."));
            }
            git::run(dir, &["branch", flag, n])
        }
        [BRANCH_RESTORE, n, oid, kv @ ..] => {
            let out = git::run(dir, &["branch", n, oid])?;
            for p in kv.chunks(2) {
                if let [k, v] = p {
                    git::run(dir, &["config", "--add", k, v])?;
                }
            }
            Ok(out)
        }
        _ => Err(format!("bad branch command {a:?}")),
    }
}

/// After a failed branch delete: undo and redo cover just the branches it did delete
/// before the failure (none: no undo), which are gone with their reflogs.
fn keep_deleted(dir: &Path, op: &mut Op) {
    let Some(back) = op.revert.as_mut().filter(|b| b.cmds.first().is_some_and(|c| c[0] == BRANCH_RESTORE)) else { return };
    back.cmds.retain(|c| git::run(dir, &["show-ref", "--verify", "-q", &format!("refs/heads/{}", c[1])]).is_err());
    let gone: Vec<String> = back.cmds.iter().map(|c| c[1].clone()).collect();
    if gone.is_empty() {
        op.revert = None;
    } else {
        op.cmds.retain(|c| gone.contains(&c[1]));
    }
}

/// Remove a worktree (`remove`: the `worktree remove` command), then force-delete its branch
/// `b`; ⌘Z restores the branch with its config (not the folder).
pub fn remove_worktree_and_branch(d: &RepoData, remove: &[&str], b: &str) -> Op {
    let mut op = delete_branches(d, "Remove worktree".into(), &[b], true);
    op.cmds.insert(0, remove.iter().map(|s| s.to_string()).collect());
    let Some(r) = d.local(b) else { return op };
    // Check the tip before removing the worktree, not only before the delete.
    op.cmds.insert(0, vec!["update-ref".into(), "--stdin".into()]);
    op.input(format!("verify refs/heads/{b} {}\n", r.target))
}

/// `exec` pseudo-commands that name stashes by commit id, since `stash@{n}` shifts whenever
/// a stash is made or dropped (CLI, another tab): `STASH_LIST n ids..` fails unless the
/// first `n` stashes are exactly `ids`, `STASH_DROP id` / `STASH_POP id` drop / pop wherever `id` is now, and
/// `STASH_HAS ids..` fails unless every commit still exists.
const STASH_LIST: &str = "!stash-list";
const STASH_DROP: &str = "!stash-drop";
const STASH_HAS: &str = "!stash-has";
const STASH_POP: &str = "!stash-pop";

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
        [cmd @ (STASH_DROP | STASH_POP), id] => match git::stashes(dir)?.into_iter().find(|s| s.id == *id) {
            Some(s) => git::run(dir, &["stash", if *cmd == STASH_POP { "pop" } else { "drop" }, &s.name]),
            None => Err(format!("Stash {} is no longer in the stash list.", short(id))),
        },
        [STASH_HAS, ids @ ..] => match ids.iter().find(|id| git::run(dir, &["cat-file", "-e", &format!("{id}^{{commit}}")]).is_err()) {
            Some(id) => Err(format!("Stash {} no longer exists (git gc pruned it), so no stash was touched.", short(id))),
            None => Ok(String::new()),
        },
        _ => Err(format!("bad stash command {a:?}")),
    }
}

/// Pop this stash wherever it is in the list now.
pub fn pop_stash(s: &git::Stash) -> Op {
    let mut op = Op::new("Pop stash", &[]);
    op.cmds = vec![vec![STASH_POP.into(), s.id.clone()]];
    op
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
            let local = d.split_remote(&r.name).map_or(r.name.as_str(), |s| s.1);
            Some(Action::Git(if d.local(local).is_some() {
                Op::new(format!("Checkout {local}"), &["checkout", local, "--"]).undo(Undo::Keep)
            } else {
                Op::new(format!("Checkout {}", r.name), &["checkout", "--track", &r.full]).undo(Undo::Keep)
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
                let mut op = op;
                if out.is_err() {
                    keep_deleted(&dir, &mut op);
                }
                let saved = saved.map(|mut s| {
                    s.seal(&dir);
                    s
                });
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
                // `keep_deleted` left a failed branch delete's revert only if it deleted some.
                let partial = op.revert.as_ref().is_some_and(|u| u.cmds.first().is_some_and(|c| c[0] == BRANCH_RESTORE));
                if let (true, Some(undo)) = (res.is_ok() || partial, &op.revert) {
                    let redo = Op { confirm: None, revert: None, ..op.clone() };
                    self.undo.push(UndoEntry::new(op.label.clone(), UndoKind::Op { undo: undo.clone(), redo: Box::new(redo) }));
                    self.redo.clear();
                }
                if let Some(saved) = saved.filter(|s| (s.stash.is_some() && !(s.tracked.is_empty() && s.staged.is_empty())) || !s.files.is_empty()) {
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
                        // Ref snapshots are unreliable now. Discards and reverting ops (deleted
                        // branches, dropped stashes) stay: they check the state before acting.
                        from.retain(|e| !matches!(e.kind, UndoKind::Refs { .. }));
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
    /// Pushes the current branch, setting an upstream on first push. A force push leases the remote branch at the
    /// commit shown, so a background fetch can't make it overwrite unseen commits. Goes where a bare `git push` goes
    /// (pushRemote / pushDefault, not the upstream's remote).
    pub fn push_op(&self, force: bool) -> Option<Op> {
        let branch = self.status.branch.as_deref()?;
        let fork = self.cfg(&format!("branch.{branch}.pushremote")).or(self.cfg("remote.pushdefault"));
        let Some((up_remote, up_rb)) = self.status.upstream.as_deref().and_then(|up| self.push_dest(branch, up)) else {
            let remote = fork.or(self.remotes.iter().find(|r| r.0 == "origin").or(self.remotes.first()).map(|r| r.0.as_str()))?;
            return Some(Op::new("Push", &["push", "--progress", "-u", remote, branch]));
        };
        // Triangular: push.default simple/current push to the same name on the fork.
        let (remote, rb) = match fork {
            Some(f) if f != up_remote => (f, branch),
            _ => (up_remote, up_rb),
        };
        let up = format!("{remote}/{rb}");
        if !force {
            // A bare push keeps push.default / pushRemote, but `simple` refuses a differently named upstream.
            // To a fork that's a bare push too: `-u` would retarget the upstream at the fork.
            return Some(if remote != up_remote {
                Op::new("Push", &["push", "--progress"])
            } else if self.status.upstream.as_deref() != Some(up.as_str()) {
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
    use super::super::data::load_all;
    use super::super::{Action, RepoTab, View};
    use super::{exec, Op};
    use crate::git;

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

    /// Unstaging a copy (`status.renames=copies`) leaves its source's staged content alone.
    #[test]
    fn unstage_copy_keeps_source() {
        let dir = scratch("unstage-copy");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        sh(&dir, &["config", "status.renames", "copies"]);
        commit(&dir, "old", "a\nb\nc\nd\n");
        std::fs::copy(dir.join("old"), dir.join("new")).unwrap();
        std::fs::write(dir.join("old"), "STAGED 1\n").unwrap();
        sh(&dir, &["add", "old", "new"]);
        std::fs::write(dir.join("old"), "WORKTREE 1\n").unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let c = t.data.as_ref().unwrap().status.staged.iter().find(|c| c.path == "new").unwrap().clone();
        assert_eq!((c.code, c.old_path.as_deref()), ('C', Some("old")));
        t.apply(&ctx, Action::Git(super::unstage_op(&c)));
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["show", ":old"]), "STAGED 1\n");
        assert_eq!(sh(&dir, &["status", "--porcelain"]), "MM old\n?? new\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Skipping or aborting a stopped rebase resets hard: unrelated edits come back with ⌘Z.
    #[test]
    fn rebase_skip_and_abort_back_up_unrelated_edits() {
        let dir = scratch("rebase-skip");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "u", "u\n");
        commit(&dir, "f", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "f", "feat\n");
        let feat = sh(&dir, &["rev-parse", "HEAD"]);
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "f", "main\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        for abort in [false, true] {
            assert!(crate::git::run(&dir, &["rebase", "main", "feat"]).is_err());
            std::fs::write(dir.join("u"), "wip\n").unwrap();
            t.apply(&ctx, Action::Git(super::sequencer_op("rebase", abort)));
            let mut op = t.confirm.take().expect("asks first");
            op.confirm = None;
            t.apply(&ctx, Action::Git(op));
            settle(&ctx, &mut t);
            assert_eq!(std::fs::read_to_string(dir.join("u")).unwrap(), "u\n");
            assert_eq!(t.data.as_ref().unwrap().state, crate::git::State::Clean);
            t.apply(&ctx, Action::Undo);
            settle(&ctx, &mut t);
            assert_eq!(std::fs::read_to_string(dir.join("u")).unwrap(), "wip\n");
            sh(&dir, &["checkout", "-q", "-f", "main"]);
            sh(&dir, &["branch", "-f", "feat", feat.trim()]);
        }
        assert!(super::sequencer_op("cherry-pick", true).backup.is_none());
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

    /// Undo of a worktree-rename discard brings back the rename: new file's bytes, old one gone.
    #[test]
    fn undo_worktree_rename_discard() {
        let dir = scratch("wt-rename-undo");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let text = "1\n2\n3\n4\n5\n6\n7\n8\n";
        commit(&dir, "old.txt", text);
        std::fs::rename(dir.join("old.txt"), dir.join("new.txt")).unwrap();
        std::fs::write(dir.join("new.txt"), format!("{text}edit\n")).unwrap();
        sh(&dir, &["add", "-N", "new.txt"]);
        let c = crate::git::status(&dir).unwrap().unstaged.remove(0);
        assert_eq!((c.code, c.old_path.as_deref()), ('R', Some("old.txt")));
        let op = super::discard_op(&c);
        let saved = super::super::undo::save_worktree(&dir, op.backup.as_ref().unwrap()).unwrap();
        super::exec(&dir, &op).unwrap();
        assert!(!dir.join("new.txt").exists());
        super::super::undo::restore_worktree(&dir, &saved).unwrap();
        assert!(!dir.join("old.txt").exists());
        assert_eq!(std::fs::read_to_string(dir.join("new.txt")).unwrap(), format!("{text}edit\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fork setup (`remote.pushDefault=origin`, upstream on `upstream`): the force push the
    /// rejected push offers replaces the fork's branch, never the shared upstream's.
    #[test]
    fn force_push_goes_to_push_remote() {
        let root = scratch("push-remote");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "up.git"]);
        sh(&root, &["init", "-q", "--bare", "-b", "main", "fork.git"]);
        sh(&root, &["clone", "-q", "-o", "upstream", "up.git", "a"]);
        let a = root.join("a");
        identity(&a);
        commit(&a, "f.txt", "one\n");
        sh(&a, &["checkout", "-q", "-b", "feature"]);
        commit(&a, "g.txt", "two\n");
        sh(&a, &["push", "-q", "-u", "upstream", "feature"]);
        sh(&a, &["remote", "add", "origin", "../fork.git"]);
        sh(&a, &["push", "-q", "origin", "feature"]);
        sh(&a, &["config", "remote.pushDefault", "origin"]);
        sh(&a, &["commit", "-q", "--amend", "-m", "two v2"]);
        let tip = |r: &str| sh(&root.join(r), &["rev-parse", "feature"]);
        let up = tip("up.git");
        let d = load_all(&a, 100, View::default()).unwrap();
        assert!(exec(&a, &d.push_op(false).unwrap()).is_err(), "the fork rejects a non-fast-forward");
        exec(&a, &d.push_op(true).unwrap()).unwrap();
        assert_eq!(tip("fork.git"), sh(&a, &["rev-parse", "HEAD"]));
        assert_eq!(tip("up.git"), up);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A rollback step that fails stops the rollback and says so, instead of claiming success.
    #[test]
    fn failed_rollback_is_reported() {
        let dir = scratch("rollback-fail");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "c1");
        let mut op = Op::new("X", &["rev-parse", "-q", "--verify", "nope"]);
        op.on_fail = vec![vec!["read-tree".into(), "--reset".into(), "nope".into()], vec!["tag".into(), "ran".into()]];
        let e = exec(&dir, &op).unwrap_err();
        assert!(!e.contains("Rolled back") && e.contains("Rolling back failed"), "{e}");
        assert_eq!(sh(&dir, &["tag", "-l"]), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A branch delete that git refuses for one name (it is being rebased) still deletes
    /// the others: ⌘Z must bring those back. Clean up doesn't offer the rebased branch.
    #[test]
    fn partly_failed_branch_delete_is_undoable() {
        let dir = scratch("partial-delete");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base");
        commit(&dir, "a", "two");
        for b in ["x", "y", "tmp"] {
            sh(&dir, &["branch", b]);
        }
        sh(&dir, &["config", "branch.x.description", "keep me"]);
        sh(&dir, &["checkout", "-q", "tmp"]);
        assert!(git::run(&dir, &["rebase", "--force-rebase", "-x", "false", "HEAD~1"]).is_err());
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.open_cleanup();
        let names: Vec<String> = t.cleanup.take().unwrap().into_iter().map(|c| c.0).collect();
        assert_eq!(names, ["x", "y"]);
        let op = super::delete_branches(t.data.as_ref().unwrap(), "Clean up branches".into(), &["x", "tmp", "y"], true);
        t.apply(&ctx, Action::Git(op));
        settle(&ctx, &mut t);
        let branches = || sh(&dir, &["for-each-ref", "--format=%(refname:short)", "refs/heads"]);
        assert_eq!(branches(), "main\ntmp\ny\n", "stops at the refused branch");
        assert_eq!(t.undo_label(), Some("Clean up branches"));
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(branches(), "main\ntmp\nx\ny\n");
        assert_eq!(sh(&dir, &["config", "branch.x.description"]), "keep me\n");
        t.apply(&ctx, Action::Redo);
        settle(&ctx, &mut t);
        assert_eq!(branches(), "main\ntmp\ny\n");
        assert_eq!(t.undo_label(), Some("Clean up branches"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A branch that moved after the delete was built (confirm dialog open) isn't deleted:
    /// undo would bring it back at the old tip and lose the new commits.
    #[test]
    fn branch_delete_refuses_when_branch_moved() {
        let dir = scratch("delete-moved");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base");
        sh(&dir, &["branch", "feat"]);
        let op = super::delete_branches(&load_all(&dir, 100, View::default()).unwrap(), "Delete feat".into(), &["feat"], true);
        sh(&dir, &["checkout", "-q", "feat"]);
        commit(&dir, "a", "new");
        sh(&dir, &["checkout", "-q", "main"]);
        let y = sh(&dir, &["rev-parse", "feat"]);
        assert!(exec(&dir, &op).is_err());
        assert_eq!(sh(&dir, &["rev-parse", "feat"]), y);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pop names the stash by commit id: a stash made since the list was loaded shifts
    /// `stash@{n}`, and the clicked stash must still be the one popped.
    #[test]
    fn pop_stash_after_list_shifted() {
        let dir = scratch("pop-shifted");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base");
        let push = |m: &str| {
            std::fs::write(dir.join("a"), m).unwrap();
            sh(&dir, &["stash", "push", "-q", "-m", m]);
        };
        push("s0");
        push("s1");
        let all = load_all(&dir, 100, View::default()).unwrap().stashes;
        push("new");
        exec(&dir, &super::pop_stash(&all[1])).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "s0");
        assert_eq!(sh(&dir, &["stash", "list", "--format=%s"]), "On main: new\nOn main: s1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pull (rebase) keeps local merge commits (like `pull.rebase=merges`) instead of flattening them.
    #[test]
    fn pull_rebase_keeps_merges() {
        let root = scratch("pull-merges");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "r.git"]);
        sh(&root, &["clone", "-q", "r.git", "a"]);
        let a = root.join("a");
        identity(&a);
        commit(&a, "f", "base");
        sh(&a, &["push", "-q", "-u", "origin", "main"]);
        sh(&root, &["clone", "-q", "r.git", "b"]);
        let b = root.join("b");
        identity(&b);
        sh(&b, &["checkout", "-q", "-b", "side"]);
        commit(&b, "s", "side");
        sh(&b, &["checkout", "-q", "main"]);
        commit(&b, "m", "mine");
        sh(&b, &["merge", "-q", "--no-ff", "-m", "Merge side", "side"]);
        commit(&a, "r", "remote");
        sh(&a, &["push", "-q"]);
        exec(&b, &super::pull_op(super::PullMode::Rebase)).unwrap();
        assert_eq!(sh(&b, &["rev-list", "--count", "--merges", "origin/main..HEAD"]), "1\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Redo of a branch delete (the same op again) after the branch got a new commit
    /// outside Glasir refuses instead of deleting that commit's only branch.
    #[test]
    fn redo_delete_refuses_a_moved_branch() {
        let dir = scratch("redo-delete");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base");
        sh(&dir, &["branch", "x"]);
        let d = super::super::data::load_all(&dir, 100, View::default()).unwrap();
        let del = super::delete_branches(&d, "Delete x".into(), &["x"], true);
        super::exec(&dir, &del).unwrap();
        super::exec(&dir, del.revert.as_ref().unwrap()).unwrap();
        sh(&dir, &["switch", "-q", "x"]);
        commit(&dir, "a", "new work");
        let new = sh(&dir, &["rev-parse", "HEAD"]);
        sh(&dir, &["switch", "-q", "main"]);
        assert!(super::exec(&dir, &del).is_err());
        assert_eq!(sh(&dir, &["rev-parse", "x"]), new);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// "Remove worktree and delete b": ⌘Z brings b back with its upstream.
    #[test]
    fn remove_worktree_and_branch_undo_keeps_upstream() {
        let root = scratch("wt-delete");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        sh(&root, &["clone", "-q", "remote.git", "a"]);
        let a = root.join("a");
        identity(&a);
        commit(&a, "f", "one");
        sh(&a, &["push", "-q", "-u", "origin", "main", "main:b"]);
        sh(&a, &["branch", "--track", "b", "origin/b"]);
        let wt = root.join("wt").to_string_lossy().into_owned();
        sh(&a, &["worktree", "add", "-q", &wt, "b"]);
        let d = super::super::data::load_all(&a, 100, View::default()).unwrap();
        let op = super::remove_worktree_and_branch(&d, &["worktree", "remove", &wt], "b");
        super::exec(&a, &op).unwrap();
        assert!(crate::git::run(&a, &["rev-parse", "--verify", "-q", "refs/heads/b"]).is_err());
        super::exec(&a, op.revert.as_ref().expect("undo restores the branch config")).unwrap();
        assert_eq!(sh(&a, &["rev-parse", "--abbrev-ref", "b@{u}"]).trim(), "origin/b");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Force delete waiting for confirmation while the branch moves: refuses, so the new tip
    /// isn't lost (⌘Z would recreate the branch at the old one).
    #[test]
    fn delete_branch_refuses_when_it_moved() {
        let dir = scratch("stale-delete");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a");
        sh(&dir, &["branch", "feat"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let op = super::delete_branches(t.data.as_ref().unwrap(), "Force delete feat".into(), &["feat"], true);
        let b = sh(&dir, &["commit-tree", "-p", "feat", "-m", "B", "feat^{tree}"]);
        sh(&dir, &["update-ref", "refs/heads/feat", b.trim()]);
        t.apply(&ctx, Action::Git(op));
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["rev-parse", "feat"]), b);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pop on a stash row shown before another stash was made (CLI, another worktree's
    /// tab): pops the stash the row shows, not whatever is `stash@{n}` now.
    #[test]
    fn pop_stash_by_id_after_the_list_shifted() {
        let dir = scratch("stale-pop");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base");
        std::fs::write(dir.join("a"), "old").unwrap();
        sh(&dir, &["stash", "push", "-q", "-m", "old"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let shown = t.data.as_ref().unwrap().stashes[0].clone();
        std::fs::write(dir.join("a"), "new").unwrap();
        sh(&dir, &["stash", "push", "-q", "-m", "new"]);
        t.apply(&ctx, Action::Git(super::pop_stash(&shown)));
        settle(&ctx, &mut t);
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "old");
        assert_eq!(sh(&dir, &["stash", "list", "--format=%s"]), "On main: new\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
