//! Git mutations: the `Op` builder, running ops off the UI thread, finishing
//! them (toasts, undo entries, reloads) and common op builders (push/pull/fetch/commit).

use super::action::{Action, Msg, MultiOp};
use super::data::RepoData;
use super::message;
use super::offer::offer;
use super::undo::{Files, Refs, UndoEntry, backed_up, backup, on_remotes, UNDO_RETRY, UNDO_SKIP};
use super::task::Task;
use super::RepoTab;
use crate::git::{self, Ref, RefKind, Res, Snapshot};
use serde::{Deserialize, Serialize};
use std::path::Path;

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
    pub reword: Option<String>,
    pub parked: bool,
}

/// What an op is about to overwrite, backed up first so ⌘Z brings it back.
#[derive(Clone, Debug)]
pub enum Backup {
    /// These paths (tracked or untracked, folders with what's in them), as they are.
    Paths(Vec<String>),
    /// Every uncommitted change (`untracked`: untracked files too), and untracked or ignored
    /// files where the commits it resets to (`revs`, a stopped rebase's own) put a file.
    Tree { revs: Vec<String>, untracked: bool },
    /// The paths stash `id` (a commit) touches: an apply or pop.
    Stash(String),
    /// Those of these paths (none: all) whose staged version differs from HEAD and the file: a
    /// stage or unstage drops it. None such, the common case: no backup.
    Staged(Vec<String>),
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
            reword: None,
            parked: false,
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

const PUSH_AFTER: &str = "Commit & push";

/// A commit drop as worked out off the UI thread: its menu, the rewrite to run, or why not.
pub enum Dropped {
    Menu(String, Vec<(String, Option<Action>)>, egui::Pos2),
    Run(Op),
    Note(String),
    Editor(Box<super::rebase::Editor>),
    Split(Op, Box<super::split::Split>),
}

impl From<Res<Op>> for Dropped {
    fn from(r: Res<Op>) -> Self {
        r.map_or_else(Dropped::Note, Dropped::Run)
    }
}

pub(super) enum Done {
    /// Its ref snapshots before and after, with what [`on_remotes`] says about them, and the
    /// files it changed (Err: a note, they couldn't be backed up after it ran).
    Op(Op, Res<String>, Option<(Snapshot, Snapshot, usize)>, Result<Option<Files>, String>),
    /// Undo or redo of step `id`, and its result: a redone op's fresh undo, a note, whether the
    /// refs were valid.
    Undo(bool, u64, Res<(Option<Box<Op>>, Option<String>, bool)>),
    Skipped(String),
}

/// Pseudo-commands that only check state (and change nothing).
const GUARDS: [&str; 6] = [
    super::diff::DIFF_IS,
    super::rebase::HEAD_IS,
    super::rebase::REFS_AT,
    super::merge_opts::CLEAN,
    super::merge_opts::PICK_NEW,
    super::remotes::PR_GUARD,
];

/// `tag --sign` (as the tag dialog builds it). Older git (2.39 to at least 2.43) makes the tag
/// unsigned when signing fails, and exits 0: that tag is deleted again and it's a failure.
fn signed_tag(dir: &Path, a: &[&str], name: &str, input: Option<&[u8]>) -> Res<String> {
    let out = git::run_input(dir, a, input)?;
    let full = format!("refs/tags/{name}");
    let tag = git::run(dir, &["cat-file", "tag", &full]).unwrap_or_default();
    if tag.trim_end().lines().last().is_some_and(|l| l.starts_with("-----END ")) {
        return Ok(out);
    }
    git::run(dir, &["update-ref", "-d", &full])?;
    Err(format!("error: git failed to sign tag {name} (and made it unsigned: deleted again)"))
}

/// NUL-separated `paths` minus those gone from disk and not in the index (an untracked file
/// that vanished since the last status): `add` fails on them and stages nothing. A tracked
/// file that is gone is a deletion to stage, so it stays.
fn without_vanished(dir: &Path, paths: &[u8]) -> Vec<u8> {
    let gone: Vec<u8> = paths.split(|&b| b == 0).filter(|p| !p.is_empty()).filter(|p| std::fs::symlink_metadata(dir.join(String::from_utf8_lossy(p).as_ref())).is_err()).flat_map(|p| p.iter().copied().chain([0])).collect();
    if gone.is_empty() {
        return paths.to_vec();
    }
    let tracked = git::run_raw(dir, &["ls-files", "-z"], None).unwrap_or_default();
    let tracked: Vec<&[u8]> = tracked.split(|&b| b == 0).collect();
    let gone: Vec<&[u8]> = gone.split(|&b| b == 0).filter(|p| !tracked.contains(p)).collect();
    paths.split(|&b| b == 0).filter(|p| !p.is_empty() && !gone.contains(p)).flat_map(|p| p.iter().copied().chain([0])).collect()
}

/// Runs an op's commands in order, stopping at the first failure.
pub(super) fn exec(dir: &Path, op: &Op) -> Res<String> {
    let one = |args: &Vec<String>| {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        match a.as_slice() {
            [super::tools::SHELL, line] => git::run_shell(dir, line),
            [STASH_LIST | STASH_DROP | STASH_HAS | STASH_POP | STASH_APPLY | STASH_RENAME | STASH_UNPOP, ..] => stash_cmd(dir, &a),
            [super::restore::APPLY_STASH_FILE, stash, path] => super::restore::apply_stash(dir, stash, path),
            [BRANCH_DELETE | BRANCH_RESTORE, ..] => branch_cmd(dir, &a),
            [super::cleanup::REMOTE_DELETE, rest @ ..] => super::cleanup::remote_delete(dir, rest),
            [super::maintenance::MAINTAIN, cmd] => super::maintenance::run(dir, cmd),
            [super::remotes::PR_GUARD, b, fetched, seen] => super::remotes::pr_guard(dir, b, fetched, seen),
            [super::file_edit::FILE_SAVE, ..] => super::file_edit::save(dir, &a, op.input.as_deref().unwrap_or_default()),
            [super::diff::DIFF_IS, ..] => super::diff::diff_is(dir, &a),
            [super::diff::APPLY_HUNK, ..] => super::diff::apply_hunk(dir, &a, op.input.as_deref().unwrap_or_default()),
            [super::rebase::HEAD_IS, want] => match super::rebase::head_state(dir) == *want {
                true => Ok(String::new()),
                false => Err("The branch changed since this was prepared, so nothing was rewritten. Try again.".into()),
            },
            [super::merge_opts::CLEAN] => super::merge_opts::clean(dir),
            [super::merge_opts::PICK_NEW, ..] => super::merge_opts::pick_new(dir, &a),
            [super::rebase::REFS_AT, pairs @ ..] => super::rebase::refs_at(dir, pairs),
            [super::rebase::REWRITE, commit, tip @ ..] => super::rebase::rewrite(dir, commit, tip.first().copied().unwrap_or("HEAD"), op.input.as_deref().unwrap_or_default()),
            [super::stack::RESTACK, rest @ ..] => super::stack::restack(dir, rest),
            [_, "add", "-A", "--pathspec-from-file=-", ..] => git::run_input(dir, &a, Some(&without_vanished(dir, op.input.as_deref().unwrap_or_default()))),
            ["--literal-pathspecs", "checkout", "--", paths @ ..] => discard_checkout(dir, &a, paths),
            ["tag", "--sign", "-a", name, ..] => signed_tag(dir, &a, name, op.input.as_deref()),
            [super::rebase::ABORT_ANY] => match git::state(&git::git_dir(dir)?) {
                git::State::Rebasing => git::run(dir, &["rebase", "--abort"]),
                _ => Ok(String::new()),
            },
            ["--literal-pathspecs", "mergetool" | "difftool", ..] | ["mergetool" | "difftool", ..] => git::run_tool(dir, &a),
            _ => git::run_input(dir, &a, op.input.as_deref()),
        }
    };
    let all = || {
        let mut out = Ok(String::new());
        for (i, args) in op.cmds.iter().enumerate() {
            let prev = std::mem::replace(&mut out, one(args));
            // A restack after a commit that had nothing to move: keep the commit's output.
            if args[0] == super::stack::RESTACK && out.as_ref().is_ok_and(String::is_empty) {
                out = prev;
            }
            if out.is_err() {
                // Only guards ran (the failed one too): there is nothing to roll back.
                if !op.on_fail.is_empty() && !op.cmds[..=i].iter().all(|c| GUARDS.contains(&c[0].as_str())) {
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

pub fn fetch_op(d: &RepoData) -> Op {
    let (remotes, elsewhere) = git::tracking(&d.config);
    if !elsewhere {
        return Op::new("Fetch", &["fetch", "--all", "--prune", "--progress"]);
    }
    let mut op = Op::new("Fetch", &["fetch", "--all", "--no-prune", "--progress"]);
    for (remote, specs) in remotes {
        op.cmds.push(["fetch", "--prune", "--no-tags", "--no-write-fetch-head", "--refmap=", &remote].map(String::from).into_iter().chain(specs).collect());
    }
    op
}

/// Stage every unstaged and untracked change, but never a conflicted file: `add -A`
/// would mark it resolved, markers and all. None when there's nothing to stage (an
/// empty pathspec list would stage everything). Paths go over stdin: no argv limit.
pub fn stage_all_op(s: &git::Status) -> Option<Op> {
    stage_files_op("Stage all", &s.unstaged)
}

/// Stage these unstaged changes in one op (a rename's old path too, so its deletion is staged).
pub fn stage_files_op(label: &str, cs: &[git::Change]) -> Option<Op> {
    let all: Vec<String> = cs.iter().flat_map(|c| c.old_path.iter().chain([&c.path])).cloned().collect();
    let paths: String = all.iter().map(|p| format!("{p}\0")).collect();
    (!paths.is_empty()).then(|| Op::new(label, &["--literal-pathspecs", "add", "-A", "--pathspec-from-file=-", "--pathspec-file-nul"]).input(paths).light().backup(Backup::Staged(all)))
}

/// Unstage these staged changes in one op; None for an empty list (no pathspec would reset everything).
pub fn unstage_files_op(cs: &[git::Change]) -> Option<Op> {
    let all: Vec<String> = cs.iter().flat_map(|c| c.old_path.iter().filter(|_| c.code == 'R').chain([&c.path])).cloned().collect();
    let paths: String = all.iter().map(|p| format!("{p}\0")).collect();
    (!paths.is_empty()).then(|| Op::new("Unstage", &["--literal-pathspecs", "reset", "-q", "--pathspec-from-file=-", "--pathspec-file-nul"]).input(paths).light().backup(Backup::Staged(all)))
}

/// Unstage everything (WIP list, ⇧⌘U, palette).
pub fn unstage_all_op() -> Op {
    Op::new("Unstage all", &["reset", "-q"]).light().backup(Backup::Staged(vec![]))
}

/// Discard several unstaged changes as one op: a single confirmation and one backup, so one ⌘Z
/// brings them all back, except the submodules among them (`subs`: the status's), which go back
/// to the commit recorded here and no backup holds.
pub fn discard_files_op(cs: &[git::Change], subs: &std::collections::HashMap<String, String>) -> Option<Op> {
    let o = if cs.len() <= 1 { discard_op(cs.first()?) } else { discard_many(cs) };
    let sm: Vec<&str> = cs.iter().map(|c| c.path.as_str()).filter(|p| subs.contains_key(*p)).collect();
    Some(match sm[..] {
        [] => o,
        [p] if cs.len() == 1 => o.confirm(format!("Discard changes to {p}? The submodule goes back to the commit recorded here, which ⌘Z can't undo.")),
        _ => o.confirm(format!("Discard changes to {} files? ⌘Z brings them back, except {}: a submodule goes back to the commit recorded here for good.", cs.len(), sm.join(", "))),
    })
}

/// [`discard_files_op`] of two or more: one `rm` / `checkout` / `clean` per kind (and per ~16 KB
/// of paths: Windows caps a command line at 32K chars), not one git run per file.
fn discard_many(cs: &[git::Change]) -> Op {
    let (mut rm, mut checkout, mut clean) = (vec![], vec![], vec![]);
    for c in cs {
        match c.code {
            '?' => clean.push(c.path.as_str()),
            // Intent-to-add: removed, not emptied; a rename's old path is checked out again.
            'A' | 'R' | 'C' => {
                rm.push(c.path.as_str());
                checkout.extend(c.old_path.as_deref().filter(|_| c.code == 'R'));
            }
            _ => checkout.push(c.path.as_str()),
        }
    }
    let mut o = Op::new(format!("Discard {} files", cs.len()), &[]);
    o.cmds.clear();
    for (sub, paths) in [(&["rm", "-fq"][..], rm), (&["checkout"], checkout), (&["clean", "-fq"], clean)] {
        let mut rest = paths.as_slice();
        while !rest.is_empty() {
            let (mut n, mut len) = (0, 0);
            while n < rest.len() && (n == 0 || len + rest[n].len() < 16_000) {
                len += rest[n].len() + 1;
                n += 1;
            }
            o.cmds.push(["--literal-pathspecs"].iter().chain(sub).chain(&["--"]).chain(&rest[..n]).map(|s| s.to_string()).collect());
            rest = &rest[n..];
        }
    }
    let paths = cs.iter().flat_map(|c| c.old_path.iter().filter(|_| c.code == 'R').chain([&c.path])).cloned().collect();
    o.light().backup(Backup::Paths(paths)).confirm(format!("Discard changes to {} files? (Undo with ⌘Z.)", cs.len()))
}

/// A discard's `checkout` (`a`), which never moves a submodule: then those of `paths` that are
/// submodules check out the commit the index records. Refused first when one is at a commit
/// none of its branches, remote branches or tags holds, which that would orphan.
fn discard_checkout(dir: &Path, a: &[&str], paths: &[&str]) -> Res<String> {
    let mut ls = vec!["--literal-pathspecs", "ls-files", "-s", "-z", "--"];
    ls.extend(paths);
    let out = git::run(dir, &ls)?;
    let subs: Vec<(&str, &str)> = out.split('\0').filter_map(|e| e.strip_prefix("160000 ")?.split_once('\t')).map(|(m, p)| (m.split(' ').next().unwrap_or(""), p)).collect();
    for (oid, p) in &subs {
        let sm = dir.join(p);
        if !sm.join(".git").exists() || git::run(&sm, &["rev-parse", "HEAD"]).is_ok_and(|h| h.trim() == *oid) {
            continue;
        }
        let held = git::run(&sm, &["for-each-ref", "--count=1", "--contains", "HEAD", "--format=x", "refs/heads", "refs/remotes", "refs/tags"]);
        if held.is_ok_and(|o| o.trim().is_empty()) {
            return Err(format!("Submodule {p} is at a commit none of its branches or tags holds: going back to the recorded commit would lose it. Create a branch for it in {p} first."));
        }
    }
    let out = git::run(dir, a)?;
    if !subs.is_empty() {
        let mut up = vec!["--literal-pathspecs", "submodule", "update", "--init", "--"];
        up.extend(subs.iter().map(|s| s.1));
        git::run(dir, &up)?;
    }
    Ok(out)
}

/// Discard one unstaged change, backed up so ⌘Z brings the bytes back.
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
    let paths = args[4..].iter().map(|p| p.to_string()).collect();
    Op::new("Unstage", &args).light().backup(Backup::Staged(paths))
}

/// The in-progress banner's Skip / Abort of a merge, rebase, cherry-pick, revert or am.
/// Only `rebase --skip/--abort` run `reset --hard`, wiping unrelated uncommitted edits too.
pub fn sequencer_op(cmd: &str, abort: bool) -> Op {
    let (label, flag) = match (abort, cmd) {
        (true, _) => (format!("Abort {cmd}"), "--abort"),
        (false, "am") => ("Skip this patch".into(), "--skip"),
        _ => ("Skip this commit".into(), "--skip"),
    };
    // Backed up as every op that moves HEAD is (`run`).
    let op = Op::new(label, &[cmd, flag]).undo(Undo::Keep);
    match (cmd, abort) {
        ("rebase", false) => op.confirm("Skip discards all uncommitted changes, not just this commit's (⌘Z restores them)."),
        ("rebase", true) => op.confirm("Abort discards all uncommitted changes (⌘Z restores them)."),
        _ => op,
    }
}

/// `git reset <mode>` of the checked-out branch to commit `id` (commit, branch and tag menus).
/// Refuses if HEAD moved since `d` was loaded. A hard reset backs up uncommitted work first
/// (one ⌘Z brings back the refs and the files) and its confirmation lists the files it discards.
pub fn reset_op(d: &RepoData, id: &str, mode: &str) -> Op {
    let head = d.status.branch.as_deref().unwrap_or("HEAD");
    let short = &id[..id.len().min(7)];
    let undo = match mode {
        "--soft" => Undo::Soft,
        "--mixed" => Undo::Mixed,
        _ => Undo::Keep,
    };
    // As `head_state` puts it: no ref when detached.
    let want = format!("{} {}", d.status.branch.as_deref().map(|b| format!("refs/heads/{b}")).unwrap_or_default(), d.status.head.as_deref().unwrap_or(""));
    let op = Op::new(format!("Reset {head} to {short} ({})", &mode[2..]), &[super::rebase::HEAD_IS, &want]).then(&["reset", "-q", mode, id, "--"]).undo(undo);
    if mode != "--hard" {
        return op;
    }
    // Untracked files survive a hard reset (backed up anyway, in case the target tracks one).
    let mut lost: Vec<&str> = d.status.staged.iter().chain(&d.status.unstaged).chain(&d.status.conflicted).filter(|c| c.code != '?').map(|c| c.path.as_str()).collect();
    lost.sort_unstable();
    lost.dedup();
    let text = match lost.len() {
        0 => format!("Hard reset {head} to {short}? No uncommitted changes to tracked files (⌘Z undoes it)."),
        n => {
            let mut t = format!("Hard reset {head} to {short}? Uncommitted changes to {n} file{} are discarded (⌘Z restores them):", if n == 1 { "" } else { "s" });
            for p in lost.iter().take(10) {
                t += &format!("\n  {p}");
            }
            if n > 10 {
                t += &format!("\n  …and {} more", n - 10);
            }
            t
        }
    };
    op.backup(Backup::Tree { revs: vec![id.into()], untracked: false }).confirm(text)
}

/// Stash everything, untracked files included (toolbar, WIP menu, palette), asking for a
/// message first (empty = git's default).
pub fn stash_prompt() -> Action {
    super::action::prompt("Stash message (optional)", "", Op::new("Stash", &["stash", "push", "--include-untracked", "-m", "{}"]))
}

/// Stash just these paths (and their old paths, for renames), untracked ones included.
/// `untracked`: some are untracked (`-u`, which also makes git save the stash before it checks the paths).
pub fn stash_paths_op(paths: &[&str], untracked: bool) -> Op {
    let list: String = paths.iter().map(|p| format!("{p}\0")).collect();
    let mut args = vec!["--literal-pathspecs", "stash", "push", "--pathspec-from-file=-", "--pathspec-file-nul"];
    if untracked {
        args.insert(3, "--include-untracked");
    }
    Op::new(if paths.len() == 1 { "Stash file".to_owned() } else { format!("Stash {} files", paths.len()) }, &args).input(list)
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
pub(super) const BRANCH_RESTORE: &str = "!branch-restore";

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
            // Back already (a retry after a partly failed undo, or recreated outside): done.
            if git::run(dir, &["rev-parse", "--verify", "-q", &format!("refs/heads/{n}")]).is_ok_and(|o| o.trim() == *oid) {
                return Ok(String::new());
            }
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
/// `b`; ⌘Z restores the branch with its config (not the folder), and redo deletes it again.
pub fn remove_worktree_and_branch(d: &RepoData, remove: &[&str], b: &str) -> Op {
    let mut op = delete_branches(d, "Remove worktree".into(), &[b], true);
    let redo = Box::new(Op { revert: None, ..op.clone() });
    if let Some(back) = &mut op.revert {
        back.revert = Some(redo);
    }
    op.cmds.insert(0, remove.iter().map(|s| s.to_string()).collect());
    let Some(r) = d.local(b) else { return op };
    // Check the tip before removing the worktree, not only before the delete.
    op.cmds.insert(0, vec!["update-ref".into(), "--stdin".into()]);
    op.input(format!("verify refs/heads/{b} {}\n", r.target))
}

/// `exec` pseudo-commands naming stashes by commit id, since `stash@{n}` shifts when one is made or dropped elsewhere.
/// `STASH_LIST n ids..` fails unless the first `n` stashes are `ids`; `STASH_DROP`, `STASH_POP` and `STASH_APPLY id` act wherever `id` is now;
/// `STASH_HAS ids..` fails unless they all exist; `STASH_RENAME id msg` stores it again with a new message; `STASH_UNPOP id msg` lists a popped one again.
const STASH_LIST: &str = "!stash-list";
const STASH_DROP: &str = "!stash-drop";
const STASH_HAS: &str = "!stash-has";
pub(super) const STASH_POP: &str = "!stash-pop";
pub(super) const STASH_APPLY: &str = "!stash-apply";
pub(super) const STASH_RENAME: &str = "!stash-rename";
pub(super) const STASH_UNPOP: &str = "!stash-unpop";

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
        // At this position (entries can share a commit): the list must be as the op was made from.
        [STASH_DROP, id, at] => match git::stashes(dir)?.into_iter().nth(at.parse().unwrap_or(usize::MAX)).filter(|s| s.id == *id) {
            Some(s) => git::run(dir, &["stash", "drop", &s.name]),
            None => Err(format!("Stash {} is no longer where it was in the stash list.", short(id))),
        },
        [STASH_DROP, id] => match git::stashes(dir)?.into_iter().find(|s| s.id == *id) {
            Some(s) => git::run(dir, &["stash", "drop", &s.name]),
            None => Err(format!("Stash {} is no longer in the stash list.", short(id))),
        },
        [cmd @ (STASH_POP | STASH_APPLY), id] => {
            let pop = *cmd == STASH_POP;
            let target = match pop {
                true => git::stashes(dir)?.into_iter().find(|s| s.id == *id).ok_or_else(|| format!("Stash {} is no longer in the stash list.", short(id)))?.name,
                false => id.to_string(),
            };
            apply_stash(dir, id, &target, pop)
        }
        [STASH_RENAME, id, msg] => {
            let Some(at) = git::stashes(dir)?.iter().position(|s| s.id == *id) else {
                return Err(format!("Stash {} is no longer in the stash list.", short(id)));
            };
            // On top, `store` would add no entry (the ref doesn't move): drop it first.
            if at == 0 {
                git::run(dir, &["stash", "drop", "stash@{0}"])?;
                return git::run(dir, &["stash", "store", "-m", msg, id]).map_err(|e| format!("{e}\n`git stash store {id}` brings the stash back."));
            }
            // Store first, then drop the old entry (now below the top): nothing is ever out of the list.
            git::run(dir, &["stash", "store", "-m", msg, id])?;
            match git::stashes(dir)?.into_iter().skip(1).find(|s| s.id == *id) {
                Some(s) => git::run(dir, &["stash", "drop", &s.name]),
                None => Ok(String::new()),
            }
        }
        // Its files go back with the step's backup: this only puts the entry back.
        [STASH_UNPOP, id, msg] => match git::stashes(dir)?.iter().any(|s| s.id == *id) {
            true => Ok(String::new()),
            false => git::run(dir, &["stash", "store", "-m", msg, id]),
        },
        [STASH_HAS, ids @ ..] => match ids.iter().find(|id| git::run(dir, &["cat-file", "-e", &format!("{id}^{{commit}}")]).is_err()) {
            Some(id) => Err(format!("Stash {} no longer exists (git gc pruned it), so no stash was touched.", short(id))),
            None => Ok(String::new()),
        },
        _ => Err(format!("bad stash command {a:?}")),
    }
}

/// Apply or pop stash `id` (`target`: its `stash@{n}` for a pop) with its staged state (`--index`)
/// when it has one, else, or when that doesn't apply, without it, saying so. Not over staged
/// changes: a failing `--index` there leaves them unstaged.
fn apply_stash(dir: &Path, id: &str, target: &str, pop: bool) -> Res<String> {
    let verb = if pop { "pop" } else { "apply" };
    if git::run(dir, &["diff", "--quiet", &format!("{id}^1"), &format!("{id}^2")]).is_ok() {
        return git::run(dir, &["stash", verb, target]);
    }
    let why = if git::run(dir, &["diff", "--cached", "--quiet"]).is_err() {
        "other changes were already staged"
    } else {
        match git::run(dir, &["stash", verb, "--index", target]) {
            Err(e) if e.contains("--index") => "they conflict with the current index",
            r => return r,
        }
    };
    git::run(dir, &["stash", verb, target]).map(|_| format!("Its staged changes came back unstaged: {why}."))
}

/// Runs `op` like `exec`, and gives a stash save, pop or rename its undo, which needs what
/// only the run knows: the stash commit a save made, a popped stash's message (its files go
/// back with the op's backup), a renamed stash's old message.
pub(super) fn exec_stash(dir: &Path, op: &mut Op) -> Res<String> {
    let saving = op.cmds.iter().any(|c| c.iter().skip_while(|a| a.starts_with("--")).take(2).eq(["stash", "push"]));
    let top = || git::Cancel::default().scope(|| git::run(dir, &["rev-parse", "-q", "--verify", "refs/stash"])).map(|s| s.trim().to_owned()).unwrap_or_default();
    let before = saving.then(top);
    let find = |cmd: &str| op.cmds.iter().find(|c| c.len() >= 2 && c[0] == cmd).map(|c| c[1].clone());
    let stash = |id: Option<String>| id.and_then(|id| git::stashes(dir).ok()?.into_iter().find(|s| s.id == id));
    let popped = stash(find(STASH_POP));
    let renamed = stash(find(STASH_RENAME));
    let out = exec(dir, op);
    if let Err(e) = out {
        // `stash push -u` saves the stash before it checks the paths, then fails on one that matches nothing.
        return match before.is_some_and(|b| b != top()) {
            true => Err(format!("{e}\nA stash was saved anyway (the top one): your files may still hold these changes, so check both before you pop it.")),
            false => Err(e),
        };
    }
    let revert = |label: &str, cmd: Vec<String>| Some(Box::new(Op { cmds: vec![cmd], ..Op::new(label, &[]) }));
    let now = before.as_ref().map(|_| top()).unwrap_or_default();
    if before.is_some_and(|b| b != now) && !now.is_empty() {
        op.revert = revert("Pop stash", vec![STASH_POP.into(), now]);
    }
    if let Some(s) = popped {
        // Redo drops it again (the files come back with the step's backup).
        op.revert = revert("Restore stash", vec![STASH_UNPOP.into(), s.id.clone(), s.message]);
        op.revert.as_mut().into_iter().for_each(|r| r.revert = revert("Drop stash", vec![STASH_DROP.into(), s.id.clone()]));
    }
    if let Some(s) = renamed {
        op.revert = revert("Rename stash back", vec![STASH_RENAME.into(), s.id, s.message]);
    }
    out
}

/// Pop this stash wherever it is in the list now.
pub fn pop_stash(s: &git::Stash) -> Op {
    Op { cmds: vec![vec![STASH_POP.into(), s.id.clone()]], ..Op::new("Pop stash", &[]) }.backup(Backup::Stash(s.id.clone()))
}

/// Apply this stash, keeping it.
pub fn apply_stash_op(s: &git::Stash) -> Op {
    Op { cmds: vec![vec![STASH_APPLY.into(), s.id.clone()]], ..Op::new("Apply stash", &[]) }.backup(Backup::Stash(s.id.clone()))
}

/// Give this stash the message `{}` (a prompt); it moves to the top of the list.
pub fn rename_stash(s: &git::Stash) -> Op {
    Op { cmds: vec![vec![STASH_RENAME.into(), s.id.clone(), "{}".into()]], ..Op::new("Rename stash", &[]) }
}

/// `git stash branch {}`: a new branch at the stash's base, checked out, with the stash popped
/// onto it. One ⌘Z puts the stash back, returns to the branch you were on and deletes the new one.
pub fn stash_branch(s: &git::Stash) -> Op {
    Op::new("Create branch from stash", &["checkout", "-b", "{}", &s.base, "--"]).then(&[STASH_POP, &s.id]).undo(Undo::Keep).backup(Backup::Stash(s.id.clone()))
}

/// Drops these stashes (by `stash@{n}`) in one op, each at its position, after checking the list is unchanged.
/// `git stash store` only pushes on top, so ⌘Z re-stores all down to the deepest drop on top, oldest first, then
/// drops the old entries of the survivors among them: a store that fails part-way leaves every stash listed.
pub fn drop_stashes(all: &[git::Stash], names: &[String]) -> Op {
    let picked = |s: &git::Stash| names.contains(&s.name);
    let dropped: Vec<&git::Stash> = all.iter().filter(|s| picked(s)).collect();
    let (n, deepest) = (dropped.len(), all.iter().rposition(picked).unwrap_or(0));
    let (label, ask) = match dropped[..] {
        [s] => ("Drop stash".to_owned(), format!("Delete {}? (Undo with ⌘Z.)", s.name)),
        _ => (format!("Drop {n} stashes"), format!("Delete {n} stashes? (Undo with ⌘Z.)")),
    };
    let check = |n: usize, ss: &[&git::Stash]| [STASH_LIST.to_owned(), n.to_string()].into_iter().chain(ss.iter().map(|s| s.id.clone())).collect();
    // The undo below is built from `all`, and redo reruns this op: both need the list unchanged.
    let mut op = Op::new(label, &[]).confirm(ask);
    op.cmds = vec![check(all.len() + 1, &all.iter().collect::<Vec<_>>())];
    // Deepest first, so each position is still where the list showed it.
    op.cmds.extend(all.iter().enumerate().rev().filter(|(_, s)| picked(s)).map(|(i, s)| vec![STASH_DROP.to_owned(), s.id.clone(), i.to_string()]));
    // Check everything first: the survivors above the deepest drop and the one
    // below it must be where the drop left them, and every dropped commit must still exist.
    let survivors: Vec<&git::Stash> = all.iter().filter(|s| !picked(s)).collect();
    let above = deepest + 1 - n;
    let mut back = Op::new(format!("Restore {n} stashes"), &[]);
    back.cmds = vec![[STASH_HAS.to_owned()].into_iter().chain(dropped.iter().map(|s| s.id.clone())).collect()];
    back.cmds.push(check(above + 1, &survivors[..(above + 1).min(survivors.len())]));
    // A survivor on top holding the deepest drop's commit goes first: storing the top commit adds no entry.
    let lead = survivors[..above].iter().take_while(|s| s.id == all[deepest].id).count();
    back.cmds.extend(survivors[..lead].iter().map(|s| vec![STASH_DROP.to_owned(), s.id.clone(), "0".to_owned()]));
    back.cmds.extend(all[..=deepest].iter().rev().map(|s| ["stash", "store", "-m", &s.message, &s.id].map(String::from).to_vec()));
    back.cmds.extend(survivors[lead..above].iter().enumerate().rev().map(|(i, s)| vec![STASH_DROP.to_owned(), s.id.clone(), (deepest + 1 + i).to_string()]));
    op.revert(back)
}

pub(super) fn network(op: &Op) -> bool {
    op.cmds.iter().any(|c| matches!(c[0].as_str(), "fetch" | "pull" | "push" | "remote" | super::cleanup::REMOTE_DELETE | super::tools::SHELL))
}

fn amend_rewrites_pushed(d: &RepoData) -> Option<String> {
    let (s, head) = (&d.status, d.status.head.as_deref()?);
    if let Some(up) = s.upstream.as_deref().filter(|u| s.ahead == 0 && d.refs.iter().any(|r| r.kind != RefKind::Tag && r.name == *u)) {
        return Some(super::rebase::rewrites_pushed(1, Some(up)));
    }
    let row = d.row_of.get(head);
    let on = |r: &Ref| r.target == head || row.zip(d.row_of.get(&r.target)).is_some_and(|(&h, &t)| t <= h && d.reachable(t)[h]);
    let remotes: Vec<&Ref> = d.refs.iter().filter(|r| r.kind == RefKind::Remote).collect();
    let unsure = !remotes.is_empty() && (row.is_none() || d.view.first_parent || d.view.filtered());
    let pushed = remotes.iter().any(|r| on(r)) || (unsure && git::run(&d.git_dir, &["rev-list", "-n1", head, "--not", "--remotes", "--"]).is_ok_and(|o| o.trim().is_empty()));
    pushed.then(|| super::rebase::rewrites_pushed(1, None))
}

/// Check out a ref: local branch, remote branch (the local branch tracking it, else its local
/// twin, fast-forwarded; a new tracking branch; or, when the twin tracks something else, a new
/// branch named in a prompt), or a tag (detached). A branch checked out in another worktree
/// opens that worktree instead.
pub fn branch_checkout(d: &RepoData, r: &Ref) -> Option<Action> {
    match r.kind {
        RefKind::Local if r.head => None,
        // Checked out in another worktree: git refuses a second checkout, so open that one.
        RefKind::Local if r.worktree.is_some() => r.worktree.clone().map(Action::Switch),
        RefKind::Local => Some(Action::Git(Op::new(format!("Checkout {}", r.name), &["checkout", &r.name, "--"]).undo(Undo::Keep))),
        RefKind::Remote => {
            let split = d.split_remote(&r.name);
            let twin = split.map_or(r.name.as_str(), |s| s.1);
            let tracks = |l: &&Ref| l.kind == RefKind::Local && l.upstream == r.name;
            let local = d.local(twin).filter(tracks).or_else(|| d.refs.iter().find(tracks)).map_or(twin, |l| l.name.as_str());
            if d.local(local).is_some_and(|l| !l.upstream.is_empty() && l.upstream != r.name) {
                let base = split.map_or_else(|| r.name.replace('/', "-"), |(rem, b)| format!("{rem}-{b}"));
                let name = (1..).map(|i| if i == 1 { base.clone() } else { format!("{base}-{i}") }).find(|n| d.local(n).is_none())?;
                let op = Op::new(format!("Checkout {}", r.name), &["checkout", "-b", "{}", "--track", &r.full, "--"]).undo(Undo::Keep);
                return Some(super::action::prompt(format!("New branch for {} (your {local} doesn't track it)", r.name), name, op));
            }
            if let Some(p) = d.local(local).and_then(|l| l.worktree.clone().filter(|_| !l.head)) {
                return Some(Action::Switch(p));
            }
            Some(Action::Git(match d.local(local) {
                Some(l) if l.head => Op::new(format!("Update {local} to {}", r.name), &["merge", "--ff-only", &r.full]).undo(Undo::Keep),
                Some(_) => Op::new(format!("Checkout {local}"), &["checkout", local, "--"]).then(&["merge", "--ff-only", &r.full]).undo(Undo::Keep),
                None => Op::new(format!("Checkout {}", r.name), &["checkout", "--track", &r.full]).undo(Undo::Keep),
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
            let body = message::split(&old).1;
            let message = if body.is_empty() { msg.clone() } else { format!("{msg}\n\n{body}") };
            return self.apply(ctx, Action::Reword { commit: commit.clone(), message });
        }
        if let [cmd, commit] = op.cmds[0].as_slice()
            && cmd == super::split::CONFIRMED
        {
            return self.start_split(ctx, commit.clone(), true);
        }
        if let [cmd, undo, id] = op.cmds[0].as_slice()
            && (cmd == UNDO_RETRY || cmd == UNDO_SKIP)
        {
            return self.retry_or_skip(ctx, cmd == UNDO_SKIP, undo == "1", id);
        }
        if self.tool_blocks(&op.label, |p| super::tools::touches(&op, p)) {
            return if op.parked { self.unpark_rebase() };
        }
        if op.confirm.is_some() {
            self.confirm = Some(op);
            return;
        }
        if let Some((busy, _)) = &self.running {
            self.notes.push(Msg { text: format!("'{}' skipped: '{busy}' is still running.", op.label), error: true });
            return if op.parked { self.unpark_rebase() };
        }
        if op.parked {
            self.parked_rebase = None;
        }
        if let Some(kind) = super::tools::kind(&op.label).filter(|_| !op.custom) {
            return self.open_tool(ctx, op, kind);
        }
        let common = network(&op).then(|| self.common.clone());
        let op = super::stack::follow(op, self.data.as_ref());
        let dir = self.dir.clone();
        let label = op.label.clone();
        let cancel = git::Cancel::default();
        self.cancel = Some(cancel.clone());
        self.running = Some((
            label,
            Task::spawn(ctx, move || {
                let _turn = common.map(super::fetch::Fetching::user);
                // Durable before anything runs: Err refuses the op. One that moves HEAD (checkout,
                // merge, rebase, continue, abort...) backs up what git may overwrite there: ignored
                // files where its commits put one, and a stopped merge's resolutions.
                let named = || Backup::Tree { revs: op.cmds.iter().flatten().cloned().collect(), untracked: false };
                let b = op.backup.clone().or_else(|| matches!(op.undo, Undo::Keep | Undo::Mixed).then(named));
                let head = git::head(&dir);
                // ponytail: a pull, or an op that fetches what it checks out (a pull request), fetches
                // first so its backup sees those commits (a second, empty fetch).
                let fetch: Option<Vec<&str>> = match op.cmds[0][0].as_str() {
                    "pull" => Some(vec!["fetch", "--progress"]),
                    "fetch" if op.cmds.len() > 1 => Some(op.cmds[0].iter().map(String::as_str).collect()),
                    _ => None,
                };
                if let (None, Some(_), Some(f)) = (&op.backup, &b, fetch) {
                    let _ = cancel.scope(|| git::run(&dir, &f));
                }
                if let Some(e) = super::undo::recursed(&dir, &op) {
                    return Done::Op(op, Err(e), None, Ok(None));
                }
                let saved = match b.as_ref().map(|b| backup(&dir, b, &op.label)).transpose() {
                    Ok(s) => s.flatten(),
                    Err(e) => return Done::Op(op, Err(e), None, Ok(None)),
                };
                let before = (op.undo != Undo::No).then(|| git::snapshot(&dir).ok()).flatten();
                let mut op = op;
                let out = cancel.scope(|| exec_stash(&dir, &mut op));
                if out.is_err() {
                    keep_deleted(&dir, &mut op);
                }
                let files = b.map_or(Ok(None), |b| backed_up(&dir, &b, saved, head.as_deref(), &op.label));
                // Even a partly failed op may have moved refs: always offer undo.
                let snaps = before.and_then(|b| git::snapshot(&dir).ok().map(|a| {
                    let n = if a == b { 0 } else { on_remotes(&dir, &b, &a) };
                    (b, a, n)
                }));
                Done::Op(op, out, snaps, files)
            }),
        ));
    }

    /// Finishes the running op once its thread is done.
    pub(super) fn poll_op(&mut self, ctx: &egui::Context, msgs: &mut Vec<Msg>) {
        let Some(done) = self.running.as_mut().and_then(|(_, t)| t.0.try_recv().ok()) else { return };
        self.running = None;
        self.cancel = None;
        match done {
            Done::Op(mut op, res, snaps, files) => {
                let commit_next = std::mem::take(&mut self.commit_next) && res.is_ok() && !op.custom && op.label == "Stage all";
                // The push runs as its own op: its failure gets the push fixes, and ⌘Z only the commit.
                let push_next = !op.custom && op.label == PUSH_AFTER && res.is_ok();
                if push_next {
                    op.label = "Commit".into();
                }
                let fetch_next = op.cmds[0].get(..3).filter(|c| res.is_ok() && c[..2] == ["remote", "add"]).map(|c| c[2].clone());
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
                    self.switch_to = Some(self.dir.join(p));
                }
                if let (Ok(_), Some(c)) = (&res, &op.reword) {
                    self.mark_saved(c);
                }
                // A squash merge leaves git's prepared message: put it in the commit box (never over typed text).
                if res.is_ok() && !op.custom && op.cmds.iter().any(|c| c[0] == "merge" && c.iter().any(|a| a == "--squash")) {
                    let untouched = (self.summary.trim().is_empty() && self.body.trim().is_empty())
                        || self.template.as_ref().is_some_and(|t| t.summary == self.summary && t.body == self.body);
                    if let (Some(d), true) = (&self.data, untouched)
                        && let Some((s, b)) = super::merge_opts::squash_message(&d.git_dir)
                    {
                        (self.summary, self.body) = (s, b);
                        self.template = None;
                    }
                    self.sel = None;
                }
                // Not when text was typed into the box while the commit (a hook) ran.
                if res.is_ok() && !op.custom && op.label.starts_with("Commit") && (op.input.is_none() || op.input == self.commit_op().input) {
                    self.reset_message();
                }
                let files = files.unwrap_or_else(|note| {
                    msgs.push(Msg { text: note, error: true });
                    None
                });
                let refs = snaps.filter(|(b, a, _)| b != a).map(|(before, after, on_remote)| Refs { before, after, mode: op.undo, on_remote });
                // `keep_deleted` left a failed branch delete's revert only if it deleted some.
                let partial = op.revert.as_ref().is_some_and(|u| u.cmds.first().is_some_and(|c| c[0] == BRANCH_RESTORE));
                // Its revert's own revert is its redo (a pop's: drop the stash again), else the op
                // itself, refreshed for a stash save (`exec_stash`), bar its remote deletes: undo doesn't bring those back.
                let ops = op.revert.clone().filter(|_| res.is_ok() || partial).map(|mut u| {
                    let mut redo = u.revert.take().unwrap_or_else(|| Box::new(Op { confirm: None, ..op.clone() }));
                    redo.cmds.retain(|c| c[0] != super::cleanup::REMOTE_DELETE);
                    (u, redo)
                });
                // Light ops toast nothing, but a discard deserves a visible way back.
                if files.is_some() && op.light && res.is_ok() {
                    msgs.push(Msg { text: format!("{} ✔ changes backed up", op.label), error: false });
                }
                if refs.is_some() || files.is_some() || ops.is_some() {
                    self.undo.push(UndoEntry::new(op.label.clone(), refs, files.into_iter().collect(), ops));
                    self.clear_redo();
                }
                // A guided split ends (or never started): see `split.rs`.
                match (op.label.as_str(), res.is_ok()) {
                    _ if op.custom => {}
                    (super::split::FINISH, true) => self.finish_split(),
                    // Stopped on a conflict in a later commit: the split ends with that rebase (below).
                    (super::split::FINISH, false) => match self.split.as_mut() {
                        Some(sp) if sp.rebase && self.data.as_ref().is_some_and(|d| git::state(&d.git_dir) == git::State::Rebasing) => {
                            sp.replaying = git::run(&self.dir, &["rev-parse", "-q", "--verify", "REBASE_HEAD"]).is_ok_and(|h| h.trim() != sp.id)
                        }
                        // A HEAD split's commit refused (a hook): HEAD is where it was, the split goes on.
                        Some(sp) if !sp.rebase => {}
                        _ => self.split = None,
                    },
                    (super::split::ABORT, true) => self.split_aborted(),
                    (l, false) if l.starts_with("Split ") => self.split = None,
                    // Show the WIP panel with the uncommitted parts (selected on reload).
                    (l, true) if l.starts_with("Split ") => self.sel = None,
                    // A failed compose step leaves the original HEAD's tree staged, which would
                    // revert the new commits: unstage, so only the uncommitted rest remains.
                    (l, false) if l.starts_with("Compose ") => self.run(ctx, Op::new("Unstage all", &["reset", "-q"]).light()),
                    _ => {}
                }
                if self.split.as_ref().is_some_and(|sp| sp.replaying) && self.data.as_ref().is_some_and(|d| git::state(&d.git_dir) != git::State::Rebasing) {
                    self.split_replayed(op.label == "Abort rebase");
                }
                // The editor closes once its save lands (and runs what waited on it); a refusal keeps the edits.
                if op.label == super::file_edit::SAVE {
                    match (&res, &mut self.edit) {
                        (Ok(_), Some(_)) => {
                            let then = self.edit.take().and_then(|e| e.then);
                            self.refresh_diff(ctx);
                            if let Some(a) = then {
                                self.apply(ctx, a);
                            }
                        }
                        (Err(_), Some(ed)) => (ed.saving, ed.then) = (false, None),
                        _ => {}
                    }
                }
                // Hunk buttons patch the shown diff: mark it stale now, not after the status reload,
                // or a second click re-applies the old patch wherever its context still matches.
                self.refresh_diff(ctx);
                // Applying a stash moves no ref: only the status changed.
                self.reload(ctx, !op.light && !(!op.custom && op.cmds.iter().all(|c| c[0] == STASH_APPLY)));
                if commit_next {
                    self.run(ctx, self.commit_op());
                }
                if let (true, Some(p)) = (push_next, self.push_op(false)) {
                    self.run(ctx, p);
                }
                if let Some(name) = fetch_next {
                    self.run(ctx, Op::new(format!("Fetch {name}"), &["fetch", "--progress", &name]));
                }
            }
            Done::Undo(was_undo, id, res) => {
                // Its refs moved back: else a rebase that runs now isn't the split's (a terminal's).
                let moved = res.as_ref().is_ok_and(|r| r.2);
                self.undone(was_undo, id, res, msgs);
                // Undone past its first step: the split is over, an older commit's rebase too (the
                // undo put HEAD and the files back; `--quit` leaves them).
                if let Some(sp) = self.split.take_if(|sp| self.undo.len() <= sp.undo_len)
                    && sp.rebase
                    && moved
                    && git::git_dir(&self.dir).is_ok_and(|g| git::state(&g) == git::State::Rebasing)
                {
                    let _ = git::run(&self.dir, &["rebase", "--quit"]);
                }
                self.reload(ctx, true);
            }
            Done::Skipped(text) => {
                msgs.push(Msg { text, error: false });
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
                return self.prepare(ctx, move |dir| match crate::repo::rebase::Editor::cherry_pick(dir, &ids) {
                    Ok(ed) => Dropped::Editor(Box::new(ed)),
                    Err(e) => Dropped::Note(e),
                });
            }
            MultiOp::CherryPick => {
                let oldest_first: Vec<&str> = ids.iter().rev().map(String::as_str).collect();
                super::merge_opts::pick_op(format!("Cherry-pick {n} commits"), &oldest_first, false, false)
            }
            MultiOp::Revert => {
                let mut args = vec!["revert", "--no-edit"];
                args.extend(ids.iter().map(String::as_str)); // newest first
                Op::new(format!("Revert {n} commits"), &args).undo(Undo::Keep)
            }
            MultiOp::Squash | MultiOp::Drop => {
                self.multi.clear();
                let drop = kind == MultiOp::Drop;
                return self.prepare(ctx, move |dir| {
                    let act = if drop { crate::repo::rebase::Act::Drop } else { crate::repo::rebase::Act::Squash };
                    let op = crate::repo::rebase::multi(dir, &git_dir, &ids, act);
                    Dropped::from(op.map(|o| {
                        let ask = format!("Drop {n} commits from the current branch? (Undo with ⌘Z.)");
                        let text = o.confirm.as_ref().map_or_else(|| ask.clone(), |pushed| format!("{ask}\n\n{pushed}"));
                        if drop { o.confirm(text) } else { o }
                    }))
                });
            }
        };
        self.multi.clear();
        self.run(ctx, op);
    }

    /// The menu for a commit dropped onto a commit and/or a branch; ⌥ squashes right away.
    /// Worked out in a task: it takes several git calls.
    pub(super) fn drop_commit(&mut self, ctx: &egui::Context, src: String, dst: Option<String>, branch: Option<String>, squash: bool) {
        let Some(d) = &self.data else { return };
        let short = |id: &str| -> String {
            let s = d.row_of.get(id).and_then(|&r| d.commit(r)).map_or(&id[..id.len().min(7)], |c| c.summary.as_str());
            if s.chars().count() > 40 { format!("{}…", s.chars().take(39).collect::<String>()) } else { s.to_owned() }
        };
        let (title, to) = (short(&src), dst.as_deref().map(short).unwrap_or_default());
        let branch = branch.and_then(|b| d.refs.iter().find(|r| r.full == b && r.kind == RefKind::Local)).map(|b| (b.name.clone(), b.head));
        let at = ctx.input(|i| i.pointer.interact_pos()).unwrap_or_default();
        let git_dir = d.git_dir.clone();
        self.prepare(ctx, move |dir| Self::drop_menu(dir, &git_dir, src, dst, branch, squash, (title, to, at)));
    }

    /// [`Self::drop_commit`]'s git work. `labels`: the dropped commit's, the target's, where.
    fn drop_menu(dir: &Path, git_dir: &Path, src: String, dst: Option<String>, branch: Option<(String, bool)>, squash: bool, labels: (String, String, egui::Pos2)) -> Dropped {
        use super::rebase::{self, Act};
        let (title, to, at) = labels;
        let on_head = git::run(dir, &["merge-base", "--is-ancestor", &src, "HEAD"]).is_ok();
        let mut items: Vec<(String, Option<Action>)> = vec![];
        if let Some(dst) = dst.filter(|t| on_head && *t != src) {
            match rebase::drag_editor(dir, &src, &dst) {
                Ok(_) if squash => return rebase::drag(dir, git_dir, &src, &dst, Act::Squash).map_or_else(Dropped::Note, Dropped::Run),
                Ok(ed) => {
                    let pos = |id: &str| ed.items.iter().position(|i| i.id == id);
                    let side = if pos(&src) > pos(&dst) { "above" } else { "below" }; // newest first
                    let drag = |act| Some(Action::DragCommit { src: src.clone(), dst: dst.clone(), act });
                    items.push((format!("Move {side} \"{to}\""), drag(Act::Pick)));
                    items.push((format!("Squash into \"{to}\""), drag(Act::Squash)));
                    items.push((format!("Fixup into \"{to}\" (drop this message)"), drag(Act::Fixup)));
                }
                Err(e) => items.push((e, None)),
            }
        }
        if let Some((name, head)) = branch {
            if !head {
                items.push((format!("Check out {name} to cherry-pick onto it"), None));
            } else if git::run(dir, &["rev-parse", "-q", "--verify", &format!("{src}^2")]).is_ok() {
                items.push(("Merge commits can't be cherry-picked by dragging.".into(), None));
            } else if !on_head {
                let mut op = super::merge_opts::pick_op(format!("Cherry-pick {}", &src[..src.len().min(7)]), &[&src], false, false);
                // The menu may wait while HEAD moves: pick onto the branch it names or not at all.
                op.cmds.insert(0, vec![rebase::HEAD_IS.into(), rebase::head_state(dir)]);
                items.push((format!("Cherry-pick onto {name}"), Some(Action::Git(op))));
            }
        }
        if items.is_empty() {
            return Dropped::Note("Drop a commit onto an unpushed commit of the current branch to move or squash it, or onto the current branch to cherry-pick it.".into());
        }
        Dropped::Menu(title, items, at)
    }

    /// Op that commits the message in the editor (and pushes, if asked).
    pub fn commit_op(&self) -> Op {
        let body = message::body(&self.body);
        let mut msg = if body.trim().is_empty() { self.summary.trim().to_owned() } else { format!("{}\n\n{body}", self.summary.trim()) };
        if let (Some(t), false) = (&self.template, self.amend) {
            msg = message::strip_comments(&msg, t);
        }
        let msg = message::add_coauthors(&msg, &self.coauthors);
        let mut args = vec!["commit", "-F", "-"];
        if self.amend {
            args.push("--amend");
        }
        match self.sign {
            Some(true) => args.push("-S"),
            Some(false) => args.push("--no-gpg-sign"),
            None => {}
        }
        if self.no_verify {
            args.push("--no-verify");
        }
        // Not after an amend: that push would need a force. `poll_op` pushes once it's committed.
        let push = self.push_after && !self.amend && self.push_op(false).is_some();
        let mut op = Op::new(if push { PUSH_AFTER } else if self.amend { "Commit (amend)" } else { "Commit" }, &args).input(msg).undo(Undo::Soft);
        if let Some((_, _, head)) = self.amend_prev.as_ref().filter(|_| self.amend) {
            op.cmds.insert(0, vec![super::rebase::HEAD_IS.into(), head.clone()]);
        }
        match self.data.as_ref().filter(|_| self.amend).and_then(amend_rewrites_pushed) {
            Some(text) => op.confirm(text),
            None => op,
        }
    }

    /// Empties the message fields for the next commit (and prefills the template again).
    fn reset_message(&mut self) {
        self.summary.clear();
        self.body.clear();
        self.amend = false;
        (self.amend_task, self.amend_prev) = (None, None);
        self.coauthors.clear();
        // A message still streaming in belongs to the commit just made.
        if matches!(self.ai.as_ref().map(|j| &j.purpose), Some(crate::ai::Purpose::CommitMessage { .. })) {
            (self.ai, self.ai_draft) = (None, None);
        }
        self.prefill_template();
    }

    /// Amend ticked on an empty box: fetch HEAD's message off the UI thread, remembering what was
    /// there and which HEAD that is (as loaded).
    pub(super) fn fill_amend(&mut self, ctx: &egui::Context) {
        let Some(s) = self.data.as_ref().map(|d| &d.status) else { return };
        let head = s.head.clone().unwrap_or_default();
        let pin = format!("{} {head}", s.branch.as_deref().map(|b| format!("refs/heads/{b}")).unwrap_or_default());
        let dir = self.dir.clone();
        self.amend_prev = Some(((self.summary.clone(), self.body.clone()), Default::default(), pin));
        self.amend_task = Some(super::Task::spawn(ctx, move || git::run(&dir, &["log", "-1", "--format=%B", &head]).ok()));
    }

    /// Amend unticked: the message it prefilled goes, unless it was edited.
    pub(super) fn unfill_amend(&mut self) {
        self.amend_task = None;
        if let Some((prev, filled, _)) = self.amend_prev.take()
            && (&self.summary, &self.body) == (&filled.0, &filled.1)
        {
            (self.summary, self.body) = prev;
        }
    }

    pub(super) fn poll_amend(&mut self) {
        if let Some(Some(msg)) = super::Task::poll(&mut self.amend_task)
            && let Some((prev, filled, _)) = &mut self.amend_prev
            && self.amend
            && (&self.summary, &self.body) == (&prev.0, &prev.1)
        {
            *filled = message::split(&msg);
            (self.summary, self.body) = filled.clone();
        }
    }

    /// Fills an empty commit box from `commit.template`.
    pub(super) fn prefill_template(&mut self) {
        if self.summary.is_empty() && self.body.is_empty() {
            self.template = self.data.as_ref().and_then(|d| message::template(d, &self.dir));
            if let Some(t) = &self.template {
                (self.summary, self.body) = (t.summary.clone(), t.body.clone());
            }
        }
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
        let src = format!("refs/heads/{branch}");
        let fork = self.cfg(&format!("branch.{branch}.pushremote")).or(self.cfg("remote.pushdefault"));
        let dest = self.status.upstream.as_deref().and_then(|up| self.push_dest(branch, up));
        let (up_remote, up_rb) = match dest {
            Some(dest) => dest,
            None if force => (fork.or(self.default_remote())?, branch),
            None => return Some(Op::new("Push", &["push", "--progress", "-u", fork.or(self.default_remote())?, &src])),
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
                Op::new("Push", &["push", "--progress", "-u", remote, &src])
            } else if rb == branch {
                Op::new("Push", &["push", "--progress"])
            } else {
                Op::new("Push", &["push", "--progress", remote, &format!("{src}:refs/heads/{rb}")])
            });
        }
        let sha = self.refs.iter().find(|r| r.kind == RefKind::Remote && r.name == up).map_or("", |r| r.target.as_str());
        let (lease, spec) = (format!("--force-with-lease=refs/heads/{rb}:{sha}"), format!("{src}:refs/heads/{rb}"));
        let mut args = vec!["push", "--progress", &lease, "--force-if-includes", remote, &spec];
        if dest.is_none() {
            args.insert(2, "-u");
        }
        let at = if sha.is_empty() { "doesn't exist".into() } else { format!("is still at {}", &sha[..7.min(sha.len())]) };
        let text = format!(
            "Replace {up} with your {branch}? Commits on {up} that aren't in your branch will be lost. Only goes through if {up} {at}.\n$ git {}",
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

    #[test]
    fn amending_a_pushed_head_asks_first() {
        let (dir, remote) = (scratch("amend-pushed"), scratch("amend-pushed-remote"));
        sh(&remote, &["init", "-q", "--bare", "-b", "main"]);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "one");
        sh(&dir, &["remote", "add", "origin", &remote.to_string_lossy()]);
        sh(&dir, &["push", "-q", "-u", "origin", "main"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        (t.amend, t.summary) = (true, "one, better".into());
        assert!(t.commit_op().confirm.is_some_and(|c| c.contains("1 commit already pushed to origin/main")));
        commit(&dir, "b", "two");
        t.reload(&ctx, true);
        settle(&ctx, &mut t);
        assert!(t.commit_op().confirm.is_none(), "an unpushed HEAD is amended at once");
        sh(&dir, &["checkout", "-q", "-b", "side"]);
        sh(&dir, &["update-ref", "refs/remotes/origin/side", "HEAD"]);
        t.reload(&ctx, true);
        settle(&ctx, &mut t);
        assert!(t.commit_op().confirm.is_some_and(|c| c.contains("1 commit already on a remote branch")));
        t.amend = false;
        assert!(t.commit_op().confirm.is_none());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&remote);
    }

    /// Amend asks by what the remotes have: not for a gone upstream, but for a HEAD on another
    /// remote's branch (triangular: tracking upstream/main, pushed to origin), there below its tip too.
    #[test]
    fn amend_asks_when_any_remote_branch_has_head() {
        let dir = scratch("amend-remotes");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "one");
        commit(&dir, "b", "two");
        for r in ["origin", "upstream"] {
            sh(&dir, &["remote", "add", r, "https://example.invalid/r.git"]);
        }
        sh(&dir, &["config", "branch.main.remote", "origin"]);
        sh(&dir, &["config", "branch.main.merge", "refs/heads/main"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        (t.amend, t.summary) = (true, "two, better".into());
        assert!(t.commit_op().confirm.is_none(), "a gone upstream holds nothing");
        let child = sh(&dir, &["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "three"]);
        sh(&dir, &["update-ref", "refs/remotes/origin/main", child.trim()]);
        sh(&dir, &["update-ref", "refs/remotes/upstream/main", "HEAD~1"]);
        sh(&dir, &["branch", "-q", "--set-upstream-to=upstream/main"]);
        let mut ask = || {
            t.reload(&ctx, true);
            settle(&ctx, &mut t);
            t.commit_op().confirm
        };
        assert!(ask().is_some_and(|c| c.contains("1 commit already on a remote branch")));
        sh(&dir, &["branch", "--unset-upstream"]);
        assert!(ask().is_some_and(|c| c.contains("1 commit already on a remote branch")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Where the graph can't tell (a first-parent view drops the merge edge, HEAD past the page),
    /// git does: a HEAD merged into a remote branch asks, an unpushed one doesn't.
    #[test]
    fn amend_asks_for_a_pushed_head_the_graph_cannot_see() {
        let dir = scratch("amend-unsure");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "one");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "f", "feat");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "a", "two");
        sh(&dir, &["merge", "-q", "--no-ff", "--no-edit", "feat"]);
        sh(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        sh(&dir, &["checkout", "-q", "feat"]);
        let ctx = egui::Context::default();
        let first_parent = View { first_parent: true, ..View::default() };
        let ask = |limit: usize, view: View| {
            let mut t = RepoTab::new(&ctx, dir.clone(), limit, view);
            settle(&ctx, &mut t);
            (t.amend, t.summary) = (true, "better".into());
            t.commit_op().confirm
        };
        assert!(ask(100, first_parent.clone()).is_some_and(|c| c.contains("1 commit already on a remote branch")));
        assert!(ask(1, View::default()).is_some_and(|c| c.contains("1 commit already on a remote branch")));
        commit(&dir, "f", "more");
        assert!(ask(100, first_parent).is_none(), "an unpushed HEAD is amended at once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Dropping pushed commits of a multi-selection asks once, with the pushed warning kept.
    #[test]
    fn multi_drop_of_pushed_commits_keeps_the_warning() {
        let dir = scratch("multi-drop-pushed");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for f in ["a", "b", "c"] {
            commit(&dir, f, f);
        }
        sh(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD~1"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let ids = ["HEAD", "HEAD~1"].map(|r| sh(&dir, &["rev-parse", r]).trim().to_owned()).to_vec();
        t.apply(&ctx, Action::MultiOp(crate::repo::MultiOp::Drop, ids));
        settle(&ctx, &mut t);
        let ask = t.confirm.take().and_then(|o| o.confirm).unwrap_or_default();
        assert!(ask.starts_with("Drop 2 commits") && ask.contains("1 commit already on a remote branch"), "{ask}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn amend_refuses_once_head_moved_from_the_commit_it_filled_in() {
        let dir = scratch("amend-pinned");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "one");
        sh(&dir, &["branch", "other"]);
        commit(&dir, "a", "two");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.amend = true;
        t.apply(&ctx, Action::AmendToggled(true));
        super::super::testutil::wait_for("the message", || {
            t.poll(&ctx, &mut vec![]);
            t.summary == "two"
        });
        sh(&dir, &["checkout", "-q", "other"]);
        let other = sh(&dir, &["rev-parse", "other"]);
        let op = t.commit_op();
        let msgs = run(&ctx, &mut t, Action::Git(op));
        assert_eq!(sh(&dir, &["rev-parse", "other"]), other);
        assert!(msgs.iter().any(|m| m.error && m.text.contains("changed since")), "{:?}", msgs.iter().map(|m| &m.text).collect::<Vec<_>>());
        sh(&dir, &["checkout", "-q", "main"]);
        t.summary = "two, better".into();
        let op = t.commit_op();
        run(&ctx, &mut t, Action::Git(op));
        assert_eq!(sh(&dir, &["log", "--format=%s", "main"]), "two, better\none\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_parked_plan_comes_back_when_its_rewrite_is_skipped() {
        use super::super::rebase::{Act, Editor};
        let dir = scratch("parked-plan");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "one");
        sh(&dir, &["checkout", "-q", "-b", "side"]);
        commit(&dir, "s", "side");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "a", "two");
        commit(&dir, "a", "three");
        let side = sh(&dir, &["rev-parse", "side"]).trim().to_owned();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let git_dir = t.data.as_ref().unwrap().git_dir.clone();
        let mut rebase = Editor::load(&dir, "HEAD~2", "one").unwrap();
        rebase.items[0].act = Act::Drop;
        for ed in [rebase, Editor::cherry_pick(&dir, &[side]).unwrap()] {
            let start = ed.start(&git_dir).unwrap();
            assert!(matches!(start, Action::RunRebase { .. }));
            t.rebase = Some(ed);
            t.apply(&ctx, start);
            t.apply(&ctx, Action::Git(Op::new("Slow", &["-c", "alias.slow=!sleep 3", "slow"])));
            settle(&ctx, &mut t);
            assert!(t.rebase.take().is_some(), "the plan is back");
            assert_eq!(sh(&dir, &["log", "--format=%s"]), "three\ntwo\none\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Double-clicking R/x fast-forwards the local x that tracks it (or nothing). One tracking something else
    /// (fork workflow: feat tracks origin/feat, mate/feat is a teammate's) is never moved: the
    /// prompt asks for a new branch tracking R/x.
    #[test]
    fn remote_double_click_fast_forwards_its_local() {
        let dir = scratch("remote-ff");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "1\n");
        sh(&dir, &["branch", "feat"]);
        commit(&dir, "a", "2\n");
        sh(&dir, &["remote", "add", "origin", "."]);
        sh(&dir, &["fetch", "-q", "origin"]);
        sh(&dir, &["branch", "-u", "origin/main", "main"]);
        sh(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
        let d = load_all(&dir, 100, View::default()).unwrap();
        let r = d.refs.iter().find(|r| r.name == "origin/main").unwrap();
        let Some(Action::Git(op)) = super::branch_checkout(&d, r) else { panic!() };
        for c in &op.cmds {
            sh(&dir, &c.iter().map(String::as_str).collect::<Vec<_>>());
        }
        assert_eq!(sh(&dir, &["rev-parse", "main"]), sh(&dir, &["rev-parse", "origin/main"]));
        let r = d.refs.iter().find(|r| r.name == "origin/feat").unwrap();
        let Some(Action::Git(op)) = super::branch_checkout(&d, r) else { panic!() };
        assert_eq!(op.cmds[0], ["checkout", "feat", "--"]);

        sh(&dir, &["branch", "-u", "origin/feat", "feat"]);
        sh(&dir, &["remote", "add", "mate", "."]);
        sh(&dir, &["fetch", "-q", "mate"]);
        let d = load_all(&dir, 100, View::default()).unwrap();
        let r = d.refs.iter().find(|r| r.name == "mate/feat").unwrap();
        let Some(Action::Prompt(p)) = super::branch_checkout(&d, r) else { panic!("asks for a new branch") };
        assert_eq!(p.value, "mate-feat");
        let feat = sh(&dir, &["rev-parse", "feat"]);
        exec(&dir, &fill(Action::Prompt(p), "mate-feat")).unwrap();
        assert_eq!(sh(&dir, &["rev-parse", "--abbrev-ref", "mate-feat@{u}"]), "mate/feat\n");
        assert_eq!((sh(&dir, &["rev-parse", "feat"]), sh(&dir, &["rev-parse", "--abbrev-ref", "feat@{u}"])), (feat, "origin/feat\n".into()));
        sh(&dir, &["checkout", "-q", "main"]);
        let d = load_all(&dir, 100, View::default()).unwrap();
        let r = d.refs.iter().find(|r| r.name == "mate/feat").unwrap();
        let Some(Action::Git(op)) = super::branch_checkout(&d, r) else { panic!("checks out the branch made for it") };
        assert_eq!(op.cmds[0], ["checkout", "mate-feat", "--"]);
        sh(&dir, &["branch", "--unset-upstream", "mate-feat"]);
        let d = load_all(&dir, 100, View::default()).unwrap();
        let r = d.refs.iter().find(|r| r.name == "mate/feat").unwrap();
        let Some(Action::Prompt(p)) = super::branch_checkout(&d, r) else { panic!("asks for a new branch") };
        assert_eq!(p.value, "mate-feat-2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_checkout_opens_its_twins_worktree_and_counts_changes() {
        let dir = scratch("remote-twin");
        let wt = scratch("remote-twin-wt");
        std::fs::remove_dir_all(&wt).unwrap();
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "1\n");
        sh(&dir, &["branch", "feat"]);
        sh(&dir, &["remote", "add", "origin", "."]);
        sh(&dir, &["fetch", "-q", "origin"]);
        sh(&dir, &["branch", "-u", "origin/feat", "feat"]);
        sh(&dir, &["worktree", "add", "-q", &wt.to_string_lossy(), "feat"]);
        std::fs::write(wt.join("new"), "x").unwrap();
        std::fs::write(dir.join("a"), "2\n").unwrap();
        let d = load_all(&dir, 100, View::default()).unwrap();
        let r = d.refs.iter().find(|r| r.name == "origin/feat").unwrap();
        assert!(matches!(super::branch_checkout(&d, r), Some(Action::Switch(_))));
        let n = |p: &std::path::Path| d.worktrees.iter().find(|w| std::path::Path::new(&w.path).canonicalize().unwrap() == p.canonicalize().unwrap()).unwrap().changes;
        assert_eq!((n(&dir), n(&wt)), (1, 1));
    }

    /// A signing program that "succeeds" without a signature: old git (< 2.45) then makes the
    /// tag unsigned and exits 0, newer git fails. Either way: an error and no tag left behind.
    #[cfg(unix)]
    #[test]
    fn a_tag_that_was_not_signed_is_not_kept() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("unsigned-tag");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "1\n");
        let fake = dir.join(".git/fake-gpg");
        std::fs::write(&fake, "#!/bin/sh\ncat >/dev/null\nexit 0\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        sh(&dir, &["config", "gpg.program", &fake.to_string_lossy()]);
        let r = super::signed_tag(&dir, &["tag", "-s", "-m", "m", "v1"], "v1", None);
        assert!(r.is_err(), "{r:?}");
        assert!(git::run(&dir, &["rev-parse", "-q", "--verify", "refs/tags/v1"]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A listed untracked file that vanished must not make the whole stage fail; a tracked
    /// file that is gone is still a deletion to stage.
    #[test]
    fn stage_skips_vanished_untracked_files() {
        let dir = scratch("stage-vanished");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a\n");
        commit(&dir, "del", "d\n");
        std::fs::write(dir.join("a"), "b\n").unwrap();
        std::fs::write(dir.join("new.rs"), "n\n").unwrap();
        std::fs::write(dir.join(".a.swp"), "s\n").unwrap();
        std::fs::remove_file(dir.join("del")).unwrap();
        let op = super::stage_all_op(&git::status(&dir).unwrap()).unwrap();
        std::fs::remove_file(dir.join(".a.swp")).unwrap();
        exec(&dir, &op).unwrap();
        let s = git::status(&dir).unwrap();
        let mut staged: Vec<_> = s.staged.iter().map(|c| c.path.as_str()).collect();
        staged.sort();
        assert_eq!(staged, ["a", "del", "new.rs"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `checkout --` never moves a submodule: discarding its pointer change must. No backup holds
    /// a submodule, so no ⌘Z is promised; a commit made on its detached HEAD (as `submodule
    /// update` leaves it) is on no branch, so the discard refuses instead of orphaning it.
    #[test]
    fn discard_resets_a_changed_submodule() {
        let (dir, sm) = (scratch("discard-sub"), scratch("discard-sub-src"));
        for d in [&dir, &sm] {
            sh(d, &["init", "-q", "-b", "main"]);
            identity(d);
        }
        commit(&sm, "f", "1\n");
        commit(&dir, "a", "a\n");
        sh(&dir, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", sm.to_str().unwrap(), "sm"]);
        sh(&dir, &["commit", "-q", "-m", "sub"]);
        identity(&dir.join("sm"));
        commit(&dir.join("sm"), "f", "2\n");
        let c = git::status(&dir).unwrap().unstaged.into_iter().find(|c| c.path == "sm").unwrap();
        exec(&dir, &super::discard_op(&c)).unwrap();
        assert!(git::status(&dir).unwrap().unstaged.iter().all(|c| c.path != "sm"));
        std::fs::write(dir.join("a"), "x\n").unwrap();
        let c = git::status(&dir).unwrap().unstaged.into_iter().find(|c| c.path == "a").unwrap();
        exec(&dir, &super::discard_op(&c)).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "a\n");
        let sub = dir.join("sm");
        sh(&sub, &["checkout", "-q", "--detach"]);
        commit(&sub, "f", "3\n");
        let head = sh(&sub, &["rev-parse", "HEAD"]);
        let s = git::status(&dir).unwrap();
        let c = s.unstaged.iter().find(|c| c.path == "sm").unwrap();
        let op = super::discard_files_op(std::slice::from_ref(c), &s.submodules).unwrap();
        assert!(op.confirm.as_deref().is_some_and(|t| t.contains("can't undo") && !t.contains("Undo with")));
        let err = exec(&dir, &op).unwrap_err();
        assert!(err.contains("none of its branches or tags holds"), "{err}");
        assert_eq!(sh(&sub, &["rev-parse", "HEAD"]), head);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&sm);
    }

    /// Multi-file stage / unstage / discard touch exactly the chosen files (glob characters, a
    /// leading dash, a rename), and the discard is one backup for all of them.
    #[test]
    fn multi_file_ops_hit_exact_files() {
        let dir = scratch("multi-files");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for f in ["weird[A].txt", "weirdA.txt", "./-dash.txt", "old.txt"] {
            commit(&dir, f, "base\n");
        }
        for f in ["weird[A].txt", "weirdA.txt", "-dash.txt"] {
            std::fs::write(dir.join(f), "edit\n").unwrap();
        }
        std::fs::rename(dir.join("old.txt"), dir.join("new.txt")).unwrap();
        sh(&dir, &["add", "-N", "new.txt"]);
        std::fs::write(dir.join("untracked u.txt"), "u\n").unwrap();
        let pick = |list: Vec<git::Change>, names: &[&str]| -> Vec<git::Change> { list.into_iter().filter(|c| names.contains(&c.path.as_str())).collect() };
        let unstaged = |names: &[&str]| pick(git::status(&dir).unwrap().unstaged, names);
        let staged = |names: &[&str]| pick(git::status(&dir).unwrap().staged, names);
        assert!(super::stage_files_op("Stage", &[]).is_none() && super::unstage_files_op(&[]).is_none() && super::discard_files_op(&[], &Default::default()).is_none());

        exec(&dir, &super::stage_files_op("Stage", &unstaged(&["weird[A].txt", "-dash.txt", "new.txt"])).unwrap()).unwrap();
        let s = git::status(&dir).unwrap();
        let mut names: Vec<&str> = s.staged.iter().map(|c| c.path.as_str()).collect();
        names.sort();
        assert_eq!(names, ["-dash.txt", "new.txt", "weird[A].txt"]);
        assert!(s.unstaged.iter().any(|c| c.path == "weirdA.txt") && s.unstaged.iter().any(|c| c.path == "untracked u.txt"));
        assert_eq!(s.staged.iter().find(|c| c.path == "new.txt").unwrap().old_path.as_deref(), Some("old.txt"));

        exec(&dir, &super::unstage_files_op(&staged(&["weird[A].txt", "new.txt"])).unwrap()).unwrap();
        let s = git::status(&dir).unwrap();
        assert_eq!(s.staged.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["-dash.txt"]);

        let both = unstaged(&["weirdA.txt", "untracked u.txt"]);
        let op = super::discard_files_op(&both, &Default::default()).unwrap();
        assert_eq!((op.label.as_str(), op.confirm.is_some()), ("Discard 2 files", true));
        assert!(matches!(&op.backup, Some(super::Backup::Paths(p)) if p.len() == 2));
        exec(&dir, &op).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("weirdA.txt")).unwrap(), "base\n");
        assert!(!dir.join("untracked u.txt").exists());
        assert_eq!(std::fs::read_to_string(dir.join("weird[A].txt")).unwrap(), "edit\n");
        assert_eq!(op.cmds.len(), 2, "one git run per kind, not per file: {:?}", op.cmds);

        // Everything else (an edit, the unstaged rename and 50 untracked files) in three runs at most.
        for i in 0..50 {
            std::fs::write(dir.join(format!("u{i}.txt")), "u\n").unwrap();
        }
        let rest = git::status(&dir).unwrap().unstaged;
        let op = super::discard_files_op(&rest, &Default::default()).unwrap();
        assert!(op.cmds.len() <= 3, "{:?}", op.cmds);
        exec(&dir, &op).unwrap();
        let s = git::status(&dir).unwrap();
        assert!(s.unstaged.is_empty(), "{:?}", s.unstaged);
        assert_eq!(std::fs::read_to_string(dir.join("old.txt")).unwrap(), "base\n");
        // A long list is split into command lines of at most ~16 KB, every path once.
        let many: Vec<git::Change> = (0..2000).map(|i| git::Change { path: format!("some/deep/folder/file{i:05}.txt"), old_path: None, code: 'M' }).collect();
        let op = super::discard_files_op(&many, &Default::default()).unwrap();
        assert!(op.cmds.len() > 1 && op.cmds.iter().all(|c| c.iter().map(|a| a.len() + 1).sum::<usize>() < 16_100));
        assert_eq!(op.cmds.iter().map(|c| c.len() - 3).sum::<usize>(), 2000);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
            // One step: restored before the refs, the conflicted `f` would block moving them back.
            assert_eq!(t.undo_label(), None);
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
        let mut op = super::drop_stashes(&all, &[all[0].name.clone(), all[2].name.clone()]);
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

    /// Two entries sharing one commit (`git stash store`): deleting one keeps the other, and ⌘Z restores it in place.
    #[test]
    fn drop_stash_with_duplicate_commit() {
        let dir = scratch("drop-dup-stash");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base");
        std::fs::write(dir.join("a"), "x").unwrap();
        sh(&dir, &["stash", "push", "-q", "-m", "first"]);
        let x = sh(&dir, &["rev-parse", "stash@{0}"]);
        std::fs::write(dir.join("a"), "y").unwrap();
        sh(&dir, &["stash", "push", "-q", "-m", "other"]);
        sh(&dir, &["stash", "store", "-m", "second", x.trim()]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let all = t.data.as_ref().unwrap().stashes.clone();
        let mut op = super::drop_stashes(&all, &[all[2].name.clone()]);
        assert_eq!(op.label, "Drop stash");
        op.confirm = None;
        t.apply(&ctx, Action::Git(op));
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["stash", "list", "--format=%gs"]), "second\nOn main: other\n");
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["stash", "list", "--format=%gs"]), "second\nOn main: other\nOn main: first\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ⌘Z of a drop whose `stash store` fails (a hook refuses the dropped stash's commit): the
    /// survivor above it is still listed, not left dangling for gc.
    #[cfg(unix)]
    #[test]
    fn drop_stashes_undo_that_fails_keeps_the_survivors() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("drop-stashes-fail");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base");
        for m in ["bottom", "top"] {
            std::fs::write(dir.join("a"), m).unwrap();
            sh(&dir, &["stash", "push", "-q", "-m", m]);
        }
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let all = t.data.as_ref().unwrap().stashes.clone();
        let mut op = super::drop_stashes(&all, &[all[1].name.clone()]);
        op.confirm = None;
        t.apply(&ctx, Action::Git(op));
        settle(&ctx, &mut t);
        let hooks = dir.join(".git/test-hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        sh(&dir, &["config", "core.hooksPath", &hooks.to_string_lossy()]);
        let hook = hooks.join("reference-transaction");
        std::fs::write(&hook, format!("#!/bin/sh\n[ \"$1\" = prepared ] || exit 0\n! grep -q {}\n", all[1].id)).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert!(t.choice.as_ref().is_some_and(|c| c.title == "Undo failed"));
        assert_eq!(sh(&dir, &["stash", "list", "--format=%gs"]), "On main: top\n");
        sh(&dir, &["config", "--unset", "core.hooksPath"]);
        let skip = t.choice.take().unwrap().ops.remove(1).1;
        let msgs = run(&ctx, &mut t, Action::Git(skip));
        assert_eq!(sh(&dir, &["stash", "list", "--format=%gs"]), "On main: bottom\nOn main: top\n", "{:?}", msgs.iter().map(|m| &m.text).collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ⌘Z of a rename on top drops the stash before it stores it again: when the store fails,
    /// Skip lists the stash again instead of leaving it dangling for gc.
    #[cfg(unix)]
    #[test]
    fn skipping_a_failed_stash_rename_undo_keeps_the_stash() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("rename-stash-fail");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base");
        std::fs::write(dir.join("a"), "wip").unwrap();
        sh(&dir, &["stash", "push", "-q", "-m", "old"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let s = t.data.as_ref().unwrap().stashes[0].clone();
        run(&ctx, &mut t, Action::Git(fill(super::super::action::prompt("", "", super::rename_stash(&s)), "renamed")));
        assert_eq!(sh(&dir, &["stash", "list", "--format=%gs"]), "renamed\n");
        let hooks = dir.join(".git/test-hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        sh(&dir, &["config", "core.hooksPath", &hooks.to_string_lossy()]);
        let hook = hooks.join("reference-transaction");
        std::fs::write(&hook, format!("#!/bin/sh\n[ \"$1\" = prepared ] || exit 0\n! grep -q ' {} '\n", s.id)).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        run(&ctx, &mut t, Action::Undo);
        assert!(t.choice.as_ref().is_some_and(|c| c.title == "Undo failed"));
        assert_eq!(sh(&dir, &["stash", "list"]), "");
        sh(&dir, &["config", "--unset", "core.hooksPath"]);
        let skip = t.choice.take().unwrap().ops.remove(1).1;
        run(&ctx, &mut t, Action::Git(skip));
        assert_eq!(sh(&dir, &["stash", "list", "--format=%H %gs"]), format!("{} On main: old\n", s.id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skipping_a_failed_branch_restore_keeps_its_tip() {
        let dir = scratch("skip-restore");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base");
        sh(&dir, &["checkout", "-q", "-b", "x"]);
        commit(&dir, "a", "only on x");
        let tip = sh(&dir, &["rev-parse", "x"]);
        sh(&dir, &["checkout", "-q", "main"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let op = super::delete_branches(t.data.as_ref().unwrap(), "Delete x".into(), &["x"], true);
        run(&ctx, &mut t, Action::Git(op));
        sh(&dir, &["branch", "x"]);
        run(&ctx, &mut t, Action::Undo);
        let skip = t.choice.take().expect("x can't come back").ops.remove(1).1;
        let msgs = run(&ctx, &mut t, Action::Git(skip));
        assert_eq!(sh(&dir, &["rev-parse", "stash@{0}^1"]), tip);
        assert!(msgs.iter().any(|m| m.text.contains("(branch x at ")), "{:?}", msgs.iter().map(|m| &m.text).collect::<Vec<_>>());
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
            let mut op = super::drop_stashes(&all, &at.iter().map(|&i| all[i].name.clone()).collect::<Vec<_>>());
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

    /// Undo of a worktree-rename discard brings back the new file's bytes and removes the old
    /// one again (the new file comes back untracked, not intent-to-add).
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
        let saved = super::super::testutil::run_backed(&dir, &op).unwrap().unwrap();
        assert!(!dir.join("new.txt").exists());
        super::super::testutil::undo_files(&dir, &saved).unwrap();
        assert!(!dir.join("old.txt").exists());
        assert_eq!(std::fs::read_to_string(dir.join("new.txt")).unwrap(), format!("{text}edit\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A hard reset lists what it discards, backs it up (also untracked and ignored files the
    /// target tracks) and one ⌘Z brings back refs and files; a stale or edited-since state refuses.
    #[test]
    fn hard_reset_undo_restores_files() {
        let dir = scratch("hard-reset");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "one\n");
        commit(&dir, "u", "theirs\n");
        commit(&dir, "x.log", "theirs\n");
        let c1 = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        sh(&dir, &["rm", "-q", "u", "x.log"]);
        commit(&dir, ".gitignore", "*.log\n");
        commit(&dir, "a", "two\n");
        let c2 = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        let w = |p: &str, t: &str| std::fs::write(dir.join(p), t).unwrap();
        let read = |p: &str| std::fs::read_to_string(dir.join(p)).unwrap_or_default();
        w("a", "edited\n");
        w("s", "staged\n");
        sh(&dir, &["add", "s"]);
        w("u", "mine\n");
        w("x.log", "mine\n");
        w("keep", "keep\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let op = super::reset_op(t.data.as_ref().unwrap(), &c1, "--hard");
        let text = op.confirm.clone().unwrap();
        assert!(text.contains("2 files") && text.contains("\n  a") && text.contains("\n  s"), "{text}");

        // HEAD moved since the menu was drawn: nothing happens.
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "outside"]);
        let mut stale = op.clone();
        stale.confirm = None;
        t.apply(&ctx, Action::Git(stale));
        settle(&ctx, &mut t);
        assert_eq!(read("a"), "edited\n");
        sh(&dir, &["reset", "-q", "--soft", &c2]);
        settle(&ctx, &mut t);

        t.apply(&ctx, Action::Git(op));
        let mut op = t.confirm.take().expect("asks first");
        op.confirm = None;
        t.apply(&ctx, Action::Git(op));
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]).trim(), c1);
        assert_eq!((read("a"), read("u"), read("x.log"), read("keep")), ("one\n".into(), "theirs\n".into(), "theirs\n".into(), "keep\n".into()));
        assert!(!dir.join("s").exists());

        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]).trim(), c2);
        assert_eq!((read("a"), read("s"), read("u"), read("x.log"), read("keep")), ("edited\n".into(), "staged\n".into(), "mine\n".into(), "mine\n".into(), "keep\n".into()));
        assert_eq!(sh(&dir, &["diff", "--cached", "--name-only"]).trim(), "s");
        assert!(sh(&dir, &["status", "--porcelain"]).contains("?? u"));
        // Redo resets again, and its undo is back.
        assert!(t.undo_label().is_none() && t.redo_label().is_some_and(|l| l.starts_with("Reset main")));
        t.apply(&ctx, Action::Redo);
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]).trim(), c1);
        assert_eq!((read("a"), read("u"), read("x.log"), read("keep")), ("one\n".into(), "theirs\n".into(), "theirs\n".into(), "keep\n".into()));
        assert!(!dir.join("s").exists());

        // Edited since: the undo goes through around the edit, and keeps a's backup in the stash list.
        w("a", "later\n");
        t.apply(&ctx, Action::Undo);
        let msgs: Vec<String> = settle(&ctx, &mut t).into_iter().map(|m| m.text).collect();
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]).trim(), c2);
        assert_eq!((read("a"), read("s"), read("u"), read("x.log")), ("later\n".into(), "staged\n".into(), "mine\n".into(), "mine\n".into()), "{msgs:?}");
        assert_eq!(sh(&dir, &["show", "stash@{0}:a"]), "edited\n");
        assert_eq!(msgs.iter().filter(|m| m.contains("stash")).count(), 1, "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A ref step changed outside Glasir: its undo still puts the files back, and says so;
    /// the steps around it stay undoable.
    #[test]
    fn hard_reset_backup_survives_a_change_outside() {
        let dir = scratch("hard-reset-kept");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "one\n");
        let c1 = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        commit(&dir, "a", "two\n");
        std::fs::write(dir.join("a"), "MINE\n").unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let mut op = super::reset_op(t.data.as_ref().unwrap(), &c1, "--hard");
        op.confirm = None;
        run(&ctx, &mut t, Action::Git(op));
        run(&ctx, &mut t, Action::Git(Op::new("Create branch x", &["branch", "x"]).undo(super::Undo::Keep)));
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "outside"]);
        // `x` didn't move outside: its step undoes as usual.
        run(&ctx, &mut t, Action::Undo);
        assert!(git::run(&dir, &["rev-parse", "--verify", "x"]).is_err());
        assert!(t.undo_label().is_some_and(|l| l.starts_with("Reset main")), "{:?}", t.undo_label());
        let msgs = run(&ctx, &mut t, Action::Undo);
        assert!(msgs.iter().any(|m| m.error && m.text.contains("changed outside")), "{:?}", msgs.iter().map(|m| &m.text).collect::<Vec<_>>());
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "MINE\n");
        assert_eq!(sh(&dir, &["log", "-1", "--format=%s"]), "outside\n");
        // Both stay redoable: the reset's files (its refs moved outside since), then `x`.
        assert_eq!(t.redo.iter().map(|e| &e.label[..8]).collect::<Vec<_>>(), ["Create b", "Reset ma"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undoing a discard moves no refs: the redo of a later step stays, and its own redo too.
    #[test]
    fn undoing_a_discard_keeps_redo() {
        let dir = scratch("discard-redo");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "one\n");
        std::fs::write(dir.join("a"), "MINE\n").unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        run(&ctx, &mut t, Action::Git(Op::new("Discard", &["checkout", "--", "a"]).light().backup(super::Backup::Paths(vec!["a".into()]))));
        run(&ctx, &mut t, Action::Git(Op::new("Create branch x", &["branch", "x"]).undo(super::Undo::Keep)));
        run(&ctx, &mut t, Action::Undo);
        run(&ctx, &mut t, Action::Undo);
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "MINE\n");
        assert_eq!(t.redo.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(), ["Create branch x", "Discard"]);
        run(&ctx, &mut t, Action::Redo);
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "one\n");
        assert_eq!(t.redo_label(), Some("Create branch x"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undoing a hard reset keeps its redo, and the redo of a later step.
    #[test]
    fn undoing_a_hard_reset_keeps_redo() {
        let dir = scratch("hard-reset-redo");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "one\n");
        let c1 = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        commit(&dir, "a", "two\n");
        std::fs::write(dir.join("a"), "MINE\n").unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let mut op = super::reset_op(t.data.as_ref().unwrap(), &c1, "--hard");
        op.confirm = None;
        run(&ctx, &mut t, Action::Git(op));
        run(&ctx, &mut t, Action::Git(Op::new("Create branch x", &["branch", "x"]).undo(super::Undo::Keep)));
        run(&ctx, &mut t, Action::Undo);
        assert_eq!(t.redo_label(), Some("Create branch x"));
        run(&ctx, &mut t, Action::Undo);
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "MINE\n");
        assert!(t.redo_label().is_some_and(|l| l.starts_with("Reset main")));
        run(&ctx, &mut t, Action::Redo);
        run(&ctx, &mut t, Action::Redo);
        assert_eq!((std::fs::read_to_string(dir.join("a")).unwrap(), sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned()), ("one\n".into(), c1));
        assert!(git::run(&dir, &["rev-parse", "--verify", "x"]).is_ok());
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
        // A failed guard ran nothing: no rollback either.
        let mut op = Op::new("X", &[super::super::rebase::HEAD_IS, "refs/heads/gone 0"]).then(&["tag", "ran"]);
        op.on_fail = vec![vec!["tag".into(), "rolled".into()]];
        let e = exec(&dir, &op).unwrap_err();
        assert!(!e.contains("Rolled back"), "{e}");
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
        t.apply(&ctx, Action::Cleanup);
        while t.cleanup.as_ref().unwrap().rows.is_none() {
            settle(&ctx, &mut t);
        }
        let names: Vec<String> = t.cleanup.take().unwrap().rows.unwrap().unwrap().into_iter().map(|c| c.name).collect();
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

    /// ⌘Z of "Remove worktree and delete b" brings back only the branch: redo deletes it again
    /// instead of removing the folder that is already gone (which failed on every retry).
    #[test]
    fn remove_worktree_and_branch_redo_deletes_the_branch_again() {
        let root = scratch("wt-redo");
        let a = root.join("a");
        std::fs::create_dir_all(&a).unwrap();
        sh(&a, &["init", "-q", "-b", "main"]);
        identity(&a);
        commit(&a, "f", "one");
        let wt = root.join("wt").to_string_lossy().into_owned();
        sh(&a, &["worktree", "add", "-q", "-b", "b", &wt]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, a.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let gone = || git::run(&a, &["rev-parse", "--verify", "-q", "refs/heads/b"]).is_err();
        let op = super::remove_worktree_and_branch(t.data.as_ref().unwrap(), &["worktree", "remove", &wt], "b");
        for (act, deleted) in [(Action::Git(op), true), (Action::Undo, false), (Action::Redo, true)] {
            t.apply(&ctx, act);
            settle(&ctx, &mut t);
            assert_eq!((gone(), t.choice.is_none()), (deleted, true));
        }
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

    /// A prompt's op with `{}` filled in, as the prompt dialog does.
    fn fill(a: Action, v: &str) -> Op {
        let Action::Prompt(p) = a else { panic!("not a prompt") };
        let mut op = p.op;
        let revert = op.revert.as_deref_mut().into_iter().flat_map(|r| r.cmds.iter_mut().flatten());
        for a in op.cmds.iter_mut().flatten().chain(revert) {
            *a = a.replace("{}", v);
        }
        op
    }

    fn stash_repo(name: &str) -> (std::path::PathBuf, egui::Context, RepoTab) {
        let dir = scratch(name);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "b", "b\n");
        commit(&dir, "a", "1\n2\n3\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        (dir, ctx, t)
    }

    fn run(ctx: &egui::Context, t: &mut RepoTab, a: Action) -> Vec<super::Msg> {
        t.apply(ctx, a);
        settle(ctx, t)
    }

    /// Stash with a message (or git's default), ⌘Z pops it back with its staged part, redo stashes again.
    #[test]
    fn stash_save_message_undo_redo() {
        let (dir, ctx, mut t) = stash_repo("stash-save");
        let read = |p: &str| std::fs::read_to_string(dir.join(p)).ok();
        let dirty = || {
            std::fs::write(dir.join("a"), "1\n2x\n3\n").unwrap();
            sh(&dir, &["add", "a"]);
            std::fs::write(dir.join("a"), "1\n2x\n3y\n").unwrap();
            std::fs::write(dir.join("new"), "n\n").unwrap();
        };
        dirty();
        run(&ctx, &mut t, Action::Git(fill(super::stash_prompt(), "my work")));
        assert_eq!(sh(&dir, &["stash", "list", "--format=%gs"]), "On main: my work\n");
        assert_eq!((read("a").unwrap().as_str(), read("new")), ("1\n2\n3\n", None));
        for round in 0..2 {
            run(&ctx, &mut t, Action::Undo);
            assert_eq!(sh(&dir, &["stash", "list"]), "", "round {round}");
            assert_eq!(sh(&dir, &["status", "--porcelain"]), "MM a\n?? new\n");
            assert_eq!(sh(&dir, &["show", ":a"]), "1\n2x\n3\n");
            run(&ctx, &mut t, Action::Redo);
            assert_eq!(sh(&dir, &["stash", "list", "--format=%gs"]), "On main: my work\n");
            assert_eq!(sh(&dir, &["status", "--porcelain"]), "");
        }
        sh(&dir, &["stash", "clear"]);
        dirty();
        run(&ctx, &mut t, Action::Git(fill(super::stash_prompt(), "")));
        assert!(sh(&dir, &["stash", "list", "--format=%gs"]).starts_with("WIP on main: "));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pop restores the staged state; ⌘Z puts the same stash back and the files as they were,
    /// redo pops again; a popped file changed since stays (its backup goes to the stash list).
    #[test]
    fn stash_pop_undo_redo() {
        let (dir, ctx, mut t) = stash_repo("stash-pop");
        std::fs::write(dir.join("a"), "1\n2x\n3\n").unwrap();
        sh(&dir, &["add", "a"]);
        std::fs::write(dir.join("new"), "n\n").unwrap();
        sh(&dir, &["stash", "push", "-q", "-u", "-m", "s"]);
        std::fs::write(dir.join("b"), "other edit\n").unwrap(); // not the stash's: untouched throughout
        let id = sh(&dir, &["rev-parse", "refs/stash"]).trim().to_owned();
        run(&ctx, &mut t, Action::Reload);
        let s = t.data.as_ref().unwrap().stashes[0].clone();
        for _ in 0..2 {
            run(&ctx, &mut t, Action::Git(super::pop_stash(&s)));
            assert_eq!(sh(&dir, &["stash", "list"]), "");
            assert_eq!(sh(&dir, &["status", "--porcelain"]), "M  a\n M b\n?? new\n");
            run(&ctx, &mut t, Action::Undo);
            assert_eq!(sh(&dir, &["stash", "list", "--format=%H %gs"]), format!("{id} On main: s\n"));
            assert_eq!(sh(&dir, &["status", "--porcelain"]), " M b\n");
            run(&ctx, &mut t, Action::Redo);
            assert_eq!(sh(&dir, &["status", "--porcelain"]), "M  a\n M b\n?? new\n");
        }
        std::fs::write(dir.join("new"), "edited after the pop\n").unwrap();
        let msgs = run(&ctx, &mut t, Action::Undo);
        assert!(t.choice.is_none() && msgs.iter().any(|m| m.text.contains("new changed since")));
        assert_eq!(std::fs::read_to_string(dir.join("new")).unwrap(), "edited after the pop\n");
        assert_eq!(sh(&dir, &["stash", "list", "--format=%gs"]), "On main: s\nGlasir: Undo Pop stash\n");
        assert_eq!(sh(&dir, &["status", "--porcelain"]), " M b\n?? new\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pop merged into a file that had staged changes: undo puts the stash back and the file as it was.
    #[test]
    fn stash_pop_onto_staged_file_undo_puts_it_back() {
        let (dir, ctx, mut t) = stash_repo("stash-pop-dirty");
        std::fs::write(dir.join("a"), "1x\n2\n3\n").unwrap();
        sh(&dir, &["stash", "push", "-q"]);
        std::fs::write(dir.join("a"), "1\n2\n3z\n").unwrap();
        sh(&dir, &["add", "a"]);
        run(&ctx, &mut t, Action::Reload);
        let s = t.data.as_ref().unwrap().stashes[0].clone();
        run(&ctx, &mut t, Action::Git(super::pop_stash(&s)));
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "1x\n2\n3z\n");
        run(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "refs/stash"]).trim(), s.id);
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "1\n2\n3z\n");
        assert_eq!(sh(&dir, &["status", "--porcelain"]), "M  a\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Apply restores the staged state, or, when it no longer applies, applies without it and says so.
    #[test]
    fn stash_apply_index_fallback() {
        let (dir, ctx, mut t) = stash_repo("stash-apply");
        std::fs::write(dir.join("a"), "1x\n2\n3\n").unwrap();
        sh(&dir, &["add", "a"]);
        sh(&dir, &["stash", "push", "-q"]);
        run(&ctx, &mut t, Action::Reload);
        let s = t.data.as_ref().unwrap().stashes[0].clone();
        run(&ctx, &mut t, Action::Git(super::apply_stash_op(&s)));
        assert_eq!(sh(&dir, &["status", "--porcelain"]), "M  a\n");
        sh(&dir, &["reset", "-q", "--hard"]);
        commit(&dir, "a", "1\n2\n3y\n"); // the staged patch's context changed; the merge is clean
        let msgs = run(&ctx, &mut t, Action::Git(super::apply_stash_op(&s)));
        assert!(msgs.iter().any(|m| m.text.contains("came back unstaged")));
        assert_eq!(sh(&dir, &["status", "--porcelain"]), " M a\n");
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "1x\n2\n3y\n");
        assert_eq!(sh(&dir, &["stash", "list"]).lines().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Rename stores the same stash with the new message; undo and redo swap it back and forth.
    #[test]
    fn stash_rename_undo_redo() {
        let (dir, ctx, mut t) = stash_repo("stash-rename");
        for m in ["old", "other"] {
            std::fs::write(dir.join("a"), m).unwrap();
            sh(&dir, &["stash", "push", "-q", "-m", m]);
        }
        run(&ctx, &mut t, Action::Reload);
        let s = t.data.as_ref().unwrap().stashes[1].clone();
        let list = || sh(&dir, &["stash", "list", "--format=%H %gs"]);
        run(&ctx, &mut t, Action::Git(fill(Action::Prompt(super::super::Prompt { title: String::new(), value: String::new(), op: super::rename_stash(&s), second: None, sign: None }), "renamed")));
        assert!(list().starts_with(&format!("{} renamed\n", s.id)) && list().lines().count() == 2, "{}", list());
        run(&ctx, &mut t, Action::Undo);
        assert!(list().starts_with(&format!("{} On main: old\n", s.id)) && list().lines().count() == 2, "{}", list());
        run(&ctx, &mut t, Action::Redo);
        assert!(list().starts_with(&format!("{} renamed\n", s.id)) && list().lines().count() == 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Branch from a stash: at its base, with the stash popped; one ⌘Z undoes all of it.
    #[test]
    fn stash_branch_and_undo() {
        let (dir, ctx, mut t) = stash_repo("stash-branch");
        std::fs::write(dir.join("a"), "1\n2s\n3\n").unwrap();
        sh(&dir, &["add", "a"]);
        std::fs::write(dir.join("u"), "u\n").unwrap();
        sh(&dir, &["stash", "push", "-q", "-u"]);
        let base = sh(&dir, &["rev-parse", "HEAD"]);
        commit(&dir, "b", "later\n");
        run(&ctx, &mut t, Action::Reload);
        let s = t.data.as_ref().unwrap().stashes[0].clone();
        let op = super::stash_branch(&s);
        run(&ctx, &mut t, Action::Git(fill(super::super::action::prompt("", "", op), "from-stash")));
        assert_eq!(sh(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), "from-stash\n");
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), base);
        assert_eq!(sh(&dir, &["status", "--porcelain"]), "M  a\n?? u\n");
        assert_eq!(sh(&dir, &["stash", "list"]), "");
        run(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), "main\n");
        assert_eq!(sh(&dir, &["branch", "--list", "from-stash"]), "");
        assert_eq!(sh(&dir, &["status", "--porcelain"]), "");
        assert_eq!(sh(&dir, &["rev-parse", "refs/stash"]).trim(), s.id);
        run(&ctx, &mut t, Action::Redo);
        assert_eq!(sh(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), "from-stash\n");
        assert_eq!(sh(&dir, &["stash", "list"]), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stash one file (an untracked one too) leaves the rest; ⌘Z pops it back.
    #[test]
    fn stash_files_and_undo() {
        let (dir, ctx, mut t) = stash_repo("stash-files");
        std::fs::write(dir.join("a"), "a edit\n").unwrap();
        std::fs::write(dir.join("b"), "b edit\n").unwrap();
        std::fs::write(dir.join("[ab]"), "glob\n").unwrap(); // as a pathspec it would take `b` too
        run(&ctx, &mut t, Action::Git(super::stash_paths_op(&["a", "[ab]"], true)));
        assert_eq!(sh(&dir, &["status", "--porcelain"]), " M b\n");
        assert_eq!(sh(&dir, &["stash", "list"]).lines().count(), 1);
        run(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["status", "--porcelain"]), " M a\n M b\n?? [ab]\n");
        assert_eq!(sh(&dir, &["stash", "list"]), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `stash push -u` with a path that matches nothing saves a stash, then fails: the failure
    /// names that stray stash. Without untracked files the path is refused before anything is saved.
    #[test]
    fn stash_files_reports_a_stray_stash() {
        let (dir, ctx, mut t) = stash_repo("stash-files-stray");
        sh(&dir, &["rm", "-q", "a"]);
        let msgs = run(&ctx, &mut t, Action::Git(super::stash_paths_op(&["a"], false)));
        assert!(msgs.iter().any(|m| m.error) && sh(&dir, &["stash", "list"]).is_empty());
        let msgs = run(&ctx, &mut t, Action::Git(super::stash_paths_op(&["a"], true)));
        assert!(msgs.iter().any(|m| m.error && m.text.contains("stash was saved anyway")), "{:?}", msgs.iter().map(|m| &m.text).collect::<Vec<_>>());
        assert!(t.undo_label().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Unstage, Unstage all and Stage of a partly staged file drop its staged version: backed
    /// up, so ⌘Z puts it back (and not an older step). Nothing lost: light, no step, no backup.
    #[test]
    fn staging_over_a_staged_version_undoes() {
        type Build = fn(&git::Status) -> Op;
        let ops: [(&str, Build); 4] = [
            ("unstage", |s| super::unstage_op(&s.staged[0])),
            ("unstage files", |s| super::unstage_files_op(&s.staged).unwrap()),
            ("unstage all", |_| super::unstage_all_op()),
            ("stage", |s| super::stage_files_op("Stage", &s.unstaged).unwrap()),
        ];
        for (name, build) in ops {
            let dir = scratch(&format!("partly-staged-{}", name.replace(' ', "-")));
            sh(&dir, &["init", "-q", "-b", "main"]);
            identity(&dir);
            commit(&dir, "f", "HEAD\n");
            let ctx = egui::Context::default();
            let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
            settle(&ctx, &mut t);
            run(&ctx, &mut t, Action::Git(Op::new("Create branch x", &["branch", "x"]).undo(super::Undo::Keep)));
            std::fs::write(dir.join("f"), "STAGED\n").unwrap();
            sh(&dir, &["add", "f"]);
            std::fs::write(dir.join("f"), "WORK\n").unwrap();
            run(&ctx, &mut t, Action::Reload);
            let op = build(&t.data.as_ref().unwrap().status);
            let label = op.label.clone();
            run(&ctx, &mut t, Action::Git(op));
            assert_ne!(sh(&dir, &["show", ":f"]), "STAGED\n", "{name}");
            assert_eq!(t.undo_label(), Some(label.as_str()), "{name}");
            run(&ctx, &mut t, Action::Undo);
            assert_eq!(sh(&dir, &["show", ":f"]), "STAGED\n", "{name}");
            assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "WORK\n", "{name}");
            assert!(git::run(&dir, &["rev-parse", "--verify", "x"]).is_ok(), "{name}");
            // Fully staged (or not staged at all): nothing to lose, no step and no backup.
            sh(&dir, if name == "stage" { &["reset", "-q", "--", "f"][..] } else { &["add", "f"] });
            run(&ctx, &mut t, Action::Reload);
            let (n, refs) = (t.undo.len(), sh(&dir, &["for-each-ref", "refs/glasir/backups/"]));
            let op = build(&t.data.as_ref().unwrap().status);
            run(&ctx, &mut t, Action::Git(op));
            assert_eq!(sh(&dir, &["show", ":f"]), if name == "stage" { "WORK\n" } else { "HEAD\n" }, "{name}");
            assert_eq!((t.undo.len(), sh(&dir, &["for-each-ref", "refs/glasir/backups/"])), (n, refs), "{name}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
