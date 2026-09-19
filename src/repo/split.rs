//! Guided commit split: the commit is uncommitted (`rebase::split`), the user
//! stages and commits the parts, then Continue replays the rest of the branch.
//! The whole split becomes one undo step.

use super::undo::{Files, Refs, UndoEntry, at, copy, moved};
use super::{Backup, Choice, Msg, Op, RepoTab, Undo};
use crate::git;

pub struct Split {
    pub id: String,
    /// An older commit: a rebase is stopped at it (a HEAD split needs none).
    pub rebase: bool,
    /// Paths the commit touched: what "left over" means at Continue.
    files: Vec<String>,
    /// The original message, for the last part.
    message: String,
    /// Undo entries before the split; the ones after are merged when it ends.
    pub(super) undo_len: usize,
    /// `symbolic-ref HEAD` at the start (empty when detached): Abort resets only this branch.
    branch: String,
    /// Continue stopped on a conflict in a later commit: no banner, the split ends with the rebase.
    pub replaying: bool,
}

/// Label of the ops that end a split (matched in `poll_op`).
pub(super) const FINISH: &str = "Finish split";
pub(super) const ABORT: &str = "Abort split";

impl RepoTab {
    pub(super) fn start_split(&mut self, ctx: &egui::Context, commit: String) {
        let Some(d) = &self.data else { return };
        let s = &d.status;
        if !s.staged.is_empty() || !s.conflicted.is_empty() || s.unstaged.iter().any(|c| c.code != '?') {
            return self.notes.push(Msg { text: "Commit or stash your changes before splitting a commit.".into(), error: true });
        }
        let is_head = s.head.as_deref() == Some(commit.as_str());
        match super::rebase::split(&self.dir, &d.git_dir, &commit, is_head) {
            Ok(op) => {
                let message = git::run(&self.dir, &["log", "-1", "--format=%B", &commit]).unwrap_or_default().trim().to_owned();
                let files = git::run(&self.dir, &["diff-tree", "--no-commit-id", "--name-only", "-r", "-z", "--no-renames", &commit]).unwrap_or_default();
                let (summary, body) = message.split_once('\n').unwrap_or((&message, ""));
                (self.summary, self.body, self.amend) = (summary.into(), body.trim().into(), false);
                let files = files.split('\0').filter(|f| !f.is_empty()).map(String::from).collect();
                let branch = git::run(&self.dir, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default().trim().to_owned();
                self.split = Some(Split { id: commit, rebase: !is_head, files, message, undo_len: self.undo.len(), branch, replaying: false });
                self.run(ctx, op);
            }
            Err(e) => self.notes.push(Msg { text: e, error: true }),
        }
    }

    /// Continue: replay the rest when nothing of the commit is left uncommitted,
    /// otherwise offer to commit the rest (or Cancel to stash / discard it first). Abort: back to the original commit.
    pub(super) fn end_split(&mut self, ctx: &egui::Context, abort: bool) {
        let (Some(sp), Some(d)) = (self.split.as_ref().filter(|sp| !sp.replaying), &self.data) else { return };
        // The banner outlives a split finished in a terminal or a checkout elsewhere,
        // and resetting whatever branch HEAD is on now would drop its commits.
        let live = if sp.rebase {
            let name = std::fs::read_to_string(d.git_dir.join("rebase-merge/head-name")).unwrap_or_default();
            git::state(&d.git_dir) == git::State::Rebasing && name.trim() == if sp.branch.is_empty() { "detached HEAD" } else { sp.branch.as_str() }
        } else {
            git::run(&self.dir, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default().trim() == sp.branch
                && git::run(&self.dir, &["merge-base", "--is-ancestor", &format!("{}^", sp.id), "HEAD"]).is_ok()
        };
        if !live {
            self.split = None;
            return self.notes.push(Msg { text: "The split already ended or HEAD moved; nothing was reset.".into(), error: true });
        }
        if abort {
            let op = match sp.rebase {
                // Track the commit's files again first: `rebase --abort` won't overwrite its now-untracked added files.
                true => Op::new(ABORT, &["reset", "-q", "--hard", &sp.id]).then(&["rebase", "--abort"]).undo(Undo::Keep),
                // Undoable: it also drops whatever was committed on top since, in the app or not.
                false => Op::new(ABORT, &["reset", "-q", &sp.id, "--"]).backup(Backup::Tree { revs: vec![], untracked: false }).undo(Undo::Mixed),
            };
            return self.run(ctx, op);
        }
        let s = &d.status;
        // The commit's force-added ignored files are untracked now but missing from status.
        let mut ign = vec!["--literal-pathspecs", "ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--"];
        ign.extend(sp.files.iter().map(String::as_str));
        let ign = git::run(&self.dir, &ign).unwrap_or_default();
        let mut left: Vec<&str> = s.staged.iter().chain(&s.unstaged).map(|c| c.path.as_str()).filter(|p| sp.files.iter().any(|f| f == p)).collect();
        left.extend(ign.split('\0').filter(|p| !p.is_empty()));
        left.sort_unstable();
        left.dedup();
        // Replaying the later commits (backed up as any op that moves HEAD).
        if left.is_empty() {
            return match sp.rebase {
                true => self.run(ctx, Op::new(FINISH, &["-c", "core.editor=true", "rebase", "--continue"]).undo(Undo::Keep)),
                false => self.finish_split(),
            };
        }
        let message = if self.summary.trim().is_empty() { sp.message.clone() } else { format!("{}\n\n{}", self.summary.trim(), self.body.trim()).trim().to_owned() };
        let mut add = vec!["--literal-pathspecs", "add", "-A", "-f", "--"];
        add.extend(&left);
        let rest = Op::new(FINISH, &add).then(&["commit", "-q", "-F", "-"]).input(message);
        // No "continue anyway": git refuses `rebase --continue` with uncommitted tracked changes.
        // A HEAD split's add replaces staged versions: backed up, for a refused commit (a hook) too.
        let rest = match sp.rebase {
            true => rest.then(&["-c", "core.editor=true", "rebase", "--continue"]).undo(Undo::Keep),
            false => rest.undo(Undo::Soft).backup(Backup::Paths(left.iter().map(|p| p.to_string()).collect())),
        };
        let ops = vec![("Commit the rest as the last part".to_owned(), rest)];
        let n = left.len();
        let text = format!("{n} file{} from {} not committed yet: {}.", if n == 1 { " is" } else { "s are" }, &sp.id[..7.min(sp.id.len())], left.join(", "));
        self.choice = Some(Choice { title: "Changes left over".into(), text, ops });
    }

    /// Aborted: back to where the split started. Its one step only brings back what the split
    /// discarded (none at all without), so the next ⌘Z undoes what came before the split.
    pub(super) fn split_aborted(&mut self) {
        if let Some(sp) = self.split.take() {
            self.fold_split(&sp, true);
        }
    }

    /// The split is done: merge its undo entries into one.
    pub(super) fn finish_split(&mut self) {
        let Some(sp) = self.split.take() else { return };
        (self.summary, self.body) = (String::new(), String::new());
        self.fold_split(&sp, false);
        self.notes.push(Msg { text: "Split done ✔".into(), error: false });
    }

    /// The rebase a failed Continue left stopped is over: aborted, or continued to the end.
    pub(super) fn split_replayed(&mut self, abort: bool) {
        match abort {
            true => self.split_aborted(),
            false => self.finish_split(),
        }
    }

    /// The split's own steps folded into one, backups oldest first, on top. Each ref goes from before its first move to after its last;
    /// other steps (a branch, tag or stash made meanwhile) stay their own, under it.
    fn fold_split(&mut self, sp: &Split, abort: bool) {
        let (mut refs, mut files, mut rest): (Option<Refs>, Vec<Files>, Vec<UndoEntry>) = (None, vec![], vec![]);
        let own = |r: &Refs| moved(&r.before, &r.after).iter().any(|m| m == "HEAD" || *m == sp.branch);
        for e in self.undo.split_off(sp.undo_len.min(self.undo.len())) {
            if e.ops.is_some() || e.refs.as_ref().is_some_and(|r| !own(r)) {
                rest.push(e);
                continue;
            }
            let UndoEntry { refs: r, files: f, .. } = e;
            files.extend(f);
            refs = match (refs, r) {
                (Some(mut acc), Some(r)) => {
                    let done = moved(&acc.before, &acc.after);
                    for m in moved(&r.before, &r.after) {
                        // Moved outside since (a commit in a terminal): the fold's ⌘Z keeps that.
                        if !done.contains(&m) || at(&acc.after, &m) != at(&r.before, &m) {
                            copy(&mut acc.before, &r.before, &m);
                        }
                        copy(&mut acc.after, &r.after, &m);
                    }
                    // Several steps: a HEAD split's abort alone keeps its mixed reset.
                    Some(Refs { mode: Undo::Keep, on_remote: 0, ..acc })
                }
                (acc, r) => acc.or(r),
            };
        }
        let refs = refs.filter(|r| r.before != r.after);
        if refs.is_some() || !files.is_empty() {
            let label = if abort { ABORT.to_owned() } else { format!("Split {}", &sp.id[..7.min(sp.id.len())]) };
            rest.push(UndoEntry::new(label, refs, files, None));
        }
        self.undo.extend(rest);
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{commit, identity, scratch, settle, sh};
    use super::super::{Action, View};
    use super::*;
    use std::path::Path;

    /// Split an older commit through the app flow: banner state, parts, Continue,
    /// and the whole split undone in one step.
    #[test]
    fn split_flow_is_one_undo_step() {
        let dir = scratch("split-flow");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "c1");
        for f in ["x", "y"] {
            std::fs::write(dir.join(f), f).unwrap();
        }
        sh(&dir, &["add", "x", "y"]);
        sh(&dir, &["commit", "-q", "-m", "x and y"]);
        commit(&dir, "z", "c3");
        let tip = sh(&dir, &["rev-parse", "HEAD"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Split(sh(&dir, &["rev-parse", "HEAD~1"]).trim().into()));
        settle(&ctx, &mut t);
        assert!(t.split.as_ref().is_some_and(|s| s.rebase && s.files == ["x", "y"]));
        assert_eq!(t.summary, "x and y");
        for f in ["x", "y"] {
            t.apply(&ctx, Action::Git(Op::new("Stage", &["add", f]).light()));
            settle(&ctx, &mut t);
            t.apply(&ctx, Action::Git(Op::new("Commit", &["commit", "-q", "-m", f]).undo(Undo::Soft)));
            settle(&ctx, &mut t);
        }
        t.apply(&ctx, Action::SplitEnd { abort: false });
        settle(&ctx, &mut t);
        assert!(t.split.is_none());
        assert_eq!(sh(&dir, &["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["c3", "y", "x", "c1"]);
        assert_eq!(t.undo_label().map(|l| l.starts_with("Split ")), Some(true));
        assert_eq!(t.undo.len(), 1);
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Abort right after splitting an older commit that added files; a force-added
    /// ignored file of the commit counts as left over at Continue.
    #[test]
    fn split_abort_and_ignored_files() {
        let dir = scratch("split-abort");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, ".gitignore", "*.log\n");
        for f in ["x", "build.log"] {
            std::fs::write(dir.join(f), f).unwrap();
        }
        sh(&dir, &["add", "-f", "x", "build.log"]);
        sh(&dir, &["commit", "-q", "-m", "x"]);
        commit(&dir, "z", "c3");
        let tip = sh(&dir, &["rev-parse", "HEAD"]);
        let x = sh(&dir, &["rev-parse", "HEAD~1"]).trim().to_owned();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Split(x.clone()));
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::SplitEnd { abort: true });
        settle(&ctx, &mut t);
        assert!(t.split.is_none());
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip);
        assert_eq!(sh(&dir, &["symbolic-ref", "--short", "HEAD"]), "main\n");
        assert!(t.undo.iter().all(|e| !e.label.starts_with("Split ")));
        assert_eq!(t.undo_label(), None);

        t.apply(&ctx, Action::Split(x));
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Stage", &["add", "x"]).light()));
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Commit", &["commit", "-q", "-m", "x"]).undo(Undo::Soft)));
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::SplitEnd { abort: false });
        settle(&ctx, &mut t);
        assert!(t.split.is_some());
        assert!(t.choice.as_ref().is_some_and(|c| c.text.contains("build.log")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stale banner's Abort (HEAD moved to another branch, or the rebase was
    /// finished in a terminal) resets nothing.
    #[test]
    fn stale_split_abort_resets_nothing() {
        let dir = scratch("split-stale");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "c1");
        sh(&dir, &["branch", "feature"]);
        commit(&dir, "a", "c2");
        let c2 = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        sh(&dir, &["checkout", "-q", "feature"]);
        commit(&dir, "f", "feature work");
        sh(&dir, &["checkout", "-q", "main"]);
        let log = |r: &str| sh(&dir, &["log", "--format=%s", r]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        // HEAD split, then the uncommitted parts are carried to `feature`.
        t.apply(&ctx, Action::Split(c2.clone()));
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Checkout feature", &["checkout", "feature", "--"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::SplitEnd { abort: true });
        settle(&ctx, &mut t);
        assert_eq!(log("feature"), "feature work\nc1\n");
        assert!(t.split.is_none());

        // Older-commit split finished in a terminal.
        sh(&dir, &["checkout", "-q", "-f", "main"]);
        sh(&dir, &["reset", "-q", "--hard", &c2]);
        commit(&dir, "b", "c3");
        t.apply(&ctx, Action::Reload);
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Split(c2));
        settle(&ctx, &mut t);
        assert!(t.split.as_ref().is_some_and(|s| s.rebase));
        sh(&dir, &["add", "-A"]);
        sh(&dir, &["commit", "-q", "-m", "c2 part"]);
        sh(&dir, &["-c", "core.editor=true", "rebase", "--continue"]);
        t.apply(&ctx, Action::Reload);
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::SplitEnd { abort: true });
        settle(&ctx, &mut t);
        assert_eq!(log("main"), "c3\nc2 part\nc1\n");
        assert!(t.split.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Aborting a HEAD split also drops commits made after the parts (in a
    /// terminal, say): ⌘Z brings the branch back as it was.
    #[test]
    fn head_split_abort_is_undoable() {
        let dir = scratch("split-head-abort");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "c1");
        for f in ["x", "y"] {
            std::fs::write(dir.join(f), f).unwrap();
        }
        sh(&dir, &["add", "x", "y"]);
        sh(&dir, &["commit", "-q", "-m", "xy"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Split(sh(&dir, &["rev-parse", "HEAD"]).trim().into()));
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Stage", &["add", "x"]).light()));
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Commit", &["commit", "-q", "-m", "x"]).undo(Undo::Soft)));
        settle(&ctx, &mut t);
        sh(&dir, &["add", "y"]);
        sh(&dir, &["commit", "-q", "-m", "y"]);
        commit(&dir, "later", "later work");
        let tip = sh(&dir, &["rev-parse", "HEAD"]);
        t.apply(&ctx, Action::Reload);
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::SplitEnd { abort: true });
        settle(&ctx, &mut t);
        assert!(t.split.is_none());
        assert_eq!(sh(&dir, &["log", "--format=%s"]), "xy\nc1\n");
        assert_eq!(t.undo_label(), Some(ABORT));
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip);
        assert_eq!(sh(&dir, &["status", "--porcelain"]), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only edits made during the split are an undoable discard, not the commit's content.
    #[test]
    fn split_abort_backs_up_only_new_edits() {
        let dir = scratch("split-abort-edits");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for n in ["1", "2", "3"] {
            commit(&dir, "a", n);
            commit(&dir, &format!("f{n}"), n);
        }
        let x = sh(&dir, &["rev-parse", "HEAD~3"]).trim().to_owned();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let split_abort = |t: &mut RepoTab, edit: bool| {
            t.apply(&ctx, Action::Split(x.clone()));
            settle(&ctx, t);
            if edit {
                std::fs::write(dir.join("f1"), "mine").unwrap();
            }
            t.apply(&ctx, Action::SplitEnd { abort: true });
            settle(&ctx, t);
        };
        split_abort(&mut t, false);
        assert_eq!(t.undo_label(), None);
        split_abort(&mut t, true);
        assert_eq!(std::fs::read_to_string(dir.join("f1")).unwrap(), "1");
        assert_eq!(t.undo_label(), Some(ABORT));
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(std::fs::read_to_string(dir.join("f1")).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "3");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Part of the split discarded by a hard reset: one ⌘Z undoes the split and brings the
    /// discarded edit back (restored first, it would block moving the branch back).
    #[test]
    fn finished_split_undoes_a_hard_reset_with_it() {
        let dir = scratch("split-hard-reset");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "1");
        std::fs::write(dir.join("a"), "2").unwrap();
        std::fs::write(dir.join("x"), "x").unwrap();
        sh(&dir, &["add", "a", "x"]);
        sh(&dir, &["commit", "-q", "-m", "ax"]);
        let tip = sh(&dir, &["rev-parse", "HEAD"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Split(tip.trim().into()));
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Stage", &["add", "x"]).light()));
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Commit", &["commit", "-q", "-m", "x"]).undo(Undo::Soft)));
        settle(&ctx, &mut t);
        let head = sh(&dir, &["rev-parse", "HEAD"]);
        let mut reset = super::super::reset_op(t.data.as_ref().unwrap(), head.trim(), "--hard");
        reset.confirm = None;
        t.apply(&ctx, Action::Git(reset));
        settle(&ctx, &mut t);
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "1");
        t.apply(&ctx, Action::SplitEnd { abort: false });
        settle(&ctx, &mut t);
        assert!(t.split.is_none());
        t.apply(&ctx, Action::Undo);
        let msgs: Vec<String> = settle(&ctx, &mut t).into_iter().map(|m| m.text).collect();
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "2");
        assert_eq!(t.undo_label(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A HEAD commit `a=2, b=2, c=2, x` on `a=1, b=1, c=1, q=1`, a branch made after it, the split started.
    fn split_repo(name: &str) -> (std::path::PathBuf, egui::Context, RepoTab, String) {
        let dir = scratch(name);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for f in ["a", "b", "c", "q"] {
            std::fs::write(dir.join(f), "1").unwrap();
        }
        sh(&dir, &["add", "a", "b", "c", "q"]);
        sh(&dir, &["commit", "-q", "-m", "base"]);
        for f in ["a", "b", "c", "x"] {
            std::fs::write(dir.join(f), "2").unwrap();
        }
        sh(&dir, &["add", "a", "b", "c", "x"]);
        sh(&dir, &["commit", "-q", "-m", "abcx"]);
        let tip = sh(&dir, &["rev-parse", "HEAD"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Create branch y", &["branch", "y"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Split(tip.trim().into()));
        settle(&ctx, &mut t);
        (dir, ctx, t, tip)
    }

    fn step(ctx: &egui::Context, t: &mut RepoTab, a: Action) -> Vec<String> {
        t.apply(ctx, a);
        settle(ctx, t).into_iter().map(|m| m.text).collect()
    }

    fn discard(p: &str) -> Action {
        Action::Git(Op::new("Discard", &["checkout", "--", p]).light().backup(Backup::Paths(vec![p.into()])))
    }

    fn commit_part(ctx: &egui::Context, t: &mut RepoTab, p: &str) {
        step(ctx, t, Action::Git(Op::new("Stage", &["add", p]).light()));
        step(ctx, t, Action::Git(Op::new("Commit", &["commit", "-q", "-m", p]).undo(Undo::Soft)));
    }

    /// Every backup made during a split comes back with its ⌘Z, newest first (an older copy of
    /// the same file goes to the stash list), and the steps before the split stay undoable.
    #[test]
    fn finished_split_restores_every_backup() {
        let read = |dir: &Path, p: &str| std::fs::read_to_string(dir.join(p)).unwrap();
        for hard in [false, true] {
            let (dir, ctx, mut t, tip) = split_repo(if hard { "split-backups-3" } else { "split-backups-2" });
            std::fs::write(dir.join("b"), "FIVE-unique").unwrap();
            step(&ctx, &mut t, discard("b"));
            step(&ctx, &mut t, discard("a"));
            commit_part(&ctx, &mut t, "x");
            if hard {
                std::fs::write(dir.join("b"), "SIX").unwrap();
                let head = sh(&dir, &["rev-parse", "HEAD"]);
                let mut reset = super::super::reset_op(t.data.as_ref().unwrap(), head.trim(), "--hard");
                reset.confirm = None;
                step(&ctx, &mut t, Action::Git(reset));
                assert_eq!(read(&dir, "c"), "1");
            } else {
                commit_part(&ctx, &mut t, "c");
            }
            step(&ctx, &mut t, Action::SplitEnd { abort: false });
            assert!(t.split.is_none());
            let msgs = step(&ctx, &mut t, Action::Undo);
            assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
            let b = if hard { "SIX" } else { "FIVE-unique" };
            assert_eq!((read(&dir, "a"), read(&dir, "b"), read(&dir, "c")), ("2".into(), b.into(), "2".into()), "{msgs:?}");
            if hard {
                assert!(sh(&dir, &["stash", "list"]).lines().count() == 1, "{msgs:?}");
                assert_eq!(sh(&dir, &["show", "stash@{0}:b"]), "FIVE-unique");
            }
            assert_eq!(t.undo_label(), Some("Create branch y"), "{msgs:?}");
            step(&ctx, &mut t, Action::Undo);
            assert!(sh(&dir, &["branch", "--list", "y"]).is_empty());
            assert_eq!(t.undo_label(), None);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A file discarded, then edited and committed in the split: ⌘Z undoes the split. One edited
    /// again and left uncommitted: the split is undone around it, its discarded copy kept in
    /// the stash list (said once), and the steps before stay undoable.
    #[test]
    fn split_undo_never_sticks_on_a_backup() {
        let (dir, ctx, mut t, tip) = split_repo("split-discard-commit");
        step(&ctx, &mut t, discard("a"));
        std::fs::write(dir.join("a"), "3").unwrap();
        commit_part(&ctx, &mut t, "a");
        step(&ctx, &mut t, Action::Git(Op::new("Stage", &["add", "-A"]).light()));
        step(&ctx, &mut t, Action::Git(Op::new("Commit", &["commit", "-q", "-m", "rest"]).undo(Undo::Soft)));
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
        assert_eq!(sh(&dir, &["status", "--porcelain"]), "");
        assert_eq!(t.undo_label(), Some("Create branch y"));
        let _ = std::fs::remove_dir_all(&dir);

        let (dir, ctx, mut t, tip) = split_repo("split-discard-edit");
        std::fs::write(dir.join("q"), "E1").unwrap();
        step(&ctx, &mut t, discard("q"));
        commit_part(&ctx, &mut t, ".");
        std::fs::write(dir.join("q"), "E2").unwrap();
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        assert!(t.split.is_none());
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("stash")), "{msgs:?}");
        assert_eq!(std::fs::read_to_string(dir.join("q")).unwrap(), "E2");
        assert_eq!(t.undo_label(), Some("Create branch y"), "{msgs:?}");
        sh(&dir, &["checkout", "--", "q"]);
        sh(&dir, &["stash", "apply", "-q"]);
        assert_eq!(std::fs::read_to_string(dir.join("q")).unwrap(), "E1");
        assert_eq!(sh(&dir, &["status", "--porcelain"]), " M q\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Like `split_repo`, but with a later commit `b=3` on top: the older `abcx` is split.
    fn older_repo(name: &str) -> (std::path::PathBuf, egui::Context, RepoTab, String) {
        let (dir, ctx, mut t, _) = split_repo(name);
        step(&ctx, &mut t, Action::SplitEnd { abort: true });
        commit(&dir, "b", "3");
        let tip = sh(&dir, &["rev-parse", "HEAD"]);
        t.undo.clear();
        sh(&dir, &["branch", "-D", "y"]);
        step(&ctx, &mut t, Action::Git(Op::new("Create branch y", &["branch", "y"]).undo(Undo::Keep)));
        step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", "HEAD~1"]).trim().into()));
        assert!(t.split.as_ref().is_some_and(|s| s.rebase));
        (dir, ctx, t, tip)
    }

    fn clean(p: &str) -> Action {
        Action::Git(Op::new("Discard", &["clean", "-fq", "--", p]).light().backup(Backup::Paths(vec![p.into()])))
    }

    /// A discarded untracked file whose path something else took since (another
    /// case on a case-insensitive disk, a folder) is never lost: it goes to the stash list.
    #[test]
    fn split_undo_keeps_a_discarded_file_whose_path_was_taken() {
        for (name, p) in [("split-taken-case", "Notes"), ("split-taken-dir", "u")] {
            let (dir, ctx, mut t, tip) = split_repo(name);
            std::fs::write(dir.join(p), "PRECIOUS").unwrap();
            step(&ctx, &mut t, clean(p));
            commit_part(&ctx, &mut t, ".");
            step(&ctx, &mut t, Action::SplitEnd { abort: false });
            if p == "u" {
                std::fs::create_dir(dir.join("u")).unwrap();
                std::fs::write(dir.join("u/f"), "other").unwrap();
            } else {
                std::fs::write(dir.join("notes"), "other").unwrap();
            }
            let msgs = step(&ctx, &mut t, Action::Undo);
            assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
            let on_disk = std::fs::read_to_string(dir.join(p)).is_ok_and(|s| s == "PRECIOUS");
            let stashed = stashed(&dir, p, "PRECIOUS");
            assert!(on_disk || stashed, "{msgs:?}");
            if stashed {
                assert!(msgs.iter().any(|m| m.contains("stash")), "{msgs:?}");
            }
            let other = if p == "u" { "u/f" } else { "notes" };
            assert_eq!(std::fs::read_to_string(dir.join(other)).unwrap(), "other");
            assert_eq!(t.undo_label(), Some("Create branch y"), "{msgs:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// One backup can't go back (a file sits where its folder goes): the others are
    /// restored, it goes to the stash list, the split is undone and the steps before stay.
    #[test]
    fn split_undo_restores_what_it_can_and_stashes_the_rest() {
        let (dir, ctx, mut t, tip) = split_repo("split-partial");
        std::fs::write(dir.join("b"), "FIVE").unwrap();
        step(&ctx, &mut t, discard("b"));
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/f"), "DF").unwrap();
        step(&ctx, &mut t, clean("d/f"));
        commit_part(&ctx, &mut t, ".");
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        let _ = std::fs::remove_dir_all(dir.join("d"));
        std::fs::write(dir.join("d"), "in the way").unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
        assert_eq!(std::fs::read_to_string(dir.join("b")).unwrap(), "FIVE", "{msgs:?}");
        assert_eq!(std::fs::read_to_string(dir.join("d")).unwrap(), "in the way");
        assert_eq!(sh(&dir, &["stash", "list"]).lines().count(), 1, "{msgs:?}");
        assert_eq!(sh(&dir, &["show", "stash@{0}^3:d/f"]), "DF");
        assert_eq!(msgs.iter().filter(|m| m.contains("stash")).count(), 1, "{msgs:?}");
        assert_eq!(t.undo_label(), Some("Create branch y"), "{msgs:?}");
        std::fs::remove_file(dir.join("d")).unwrap();
        step(&ctx, &mut t, Action::Undo);
        assert!(sh(&dir, &["branch", "--list", "y"]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// After aborting a HEAD split, ⌘Z brings back what the split discarded, then
    /// undoes the steps before the split.
    #[test]
    fn head_split_abort_keeps_the_steps_before_it() {
        for discarded in [false, true] {
            let (dir, ctx, mut t, tip) = split_repo(if discarded { "split-abort-d" } else { "split-abort-n" });
            if discarded {
                std::fs::write(dir.join("b"), "FIVE").unwrap();
                step(&ctx, &mut t, discard("b"));
            }
            commit_part(&ctx, &mut t, "x");
            step(&ctx, &mut t, Action::SplitEnd { abort: true });
            assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip);
            if discarded {
                let msgs = step(&ctx, &mut t, Action::Undo);
                assert_eq!(std::fs::read_to_string(dir.join("b")).unwrap(), "FIVE", "{msgs:?}");
            }
            assert_eq!(t.undo_label(), Some("Create branch y"));
            let msgs = step(&ctx, &mut t, Action::Undo);
            assert!(sh(&dir, &["branch", "--list", "y"]).is_empty(), "{msgs:?}");
            assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// Continue of an older split stops on a conflict in a later commit, then the
    /// rebase is aborted: ⌘Z brings back what the abort discarded (the conflicted file), keeps the
    /// older discard of that file in the stash list, then undoes the steps before the split.
    #[test]
    fn failed_split_continue_then_abort_undoes_in_turn() {
        let (dir, ctx, mut t, tip) = older_repo("split-continue-conflict");
        std::fs::write(dir.join("b"), "FIVE").unwrap();
        step(&ctx, &mut t, discard("b"));
        step(&ctx, &mut t, Action::Git(Op::new("Stage", &["add", "-A"]).light()));
        step(&ctx, &mut t, Action::Git(Op::new("Commit", &["commit", "-q", "-m", "rest"]).undo(Undo::Soft)));
        let msgs = step(&ctx, &mut t, Action::SplitEnd { abort: false });
        assert_eq!(t.data.as_ref().unwrap().state, git::State::Rebasing, "{msgs:?}");
        let mut abort = super::super::sequencer_op("rebase", true);
        abort.confirm = None;
        step(&ctx, &mut t, Action::Git(abort));
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip);
        assert!(t.split.is_none());
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert!(std::fs::read_to_string(dir.join("b")).unwrap().contains("<<<<<<<"), "{msgs:?}");
        assert!(stashed(&dir, "b", "FIVE"), "{msgs:?}");
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
        assert_eq!(t.undo_label(), Some("Create branch y"), "{msgs:?}");
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert!(sh(&dir, &["branch", "--list", "y"]).is_empty(), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undoing the split's own first step ends it (no live banner on the old tip).
    #[test]
    fn undoing_past_the_split_start_ends_it() {
        let (dir, ctx, mut t, tip) = split_repo("split-undo-start");
        std::fs::write(dir.join("b"), "FIVE").unwrap();
        step(&ctx, &mut t, discard("b"));
        commit_part(&ctx, &mut t, "x");
        for _ in 0..2 {
            step(&ctx, &mut t, Action::Undo);
            assert!(t.split.is_some());
        }
        step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip);
        assert!(t.split.is_none());
        assert_eq!(t.undo_label(), Some("Create branch y"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A hard reset inside an older split (detached HEAD) runs, and ⌘Z brings the files back.
    #[test]
    fn hard_reset_inside_an_older_split() {
        let (dir, ctx, mut t, _) = older_repo("split-detached-reset");
        let head = sh(&dir, &["rev-parse", "HEAD"]);
        let mut reset = super::super::reset_op(t.data.as_ref().unwrap(), head.trim(), "--hard");
        reset.confirm = None;
        let msgs = step(&ctx, &mut t, Action::Git(reset));
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "1", "{msgs:?}");
        step(&ctx, &mut t, Action::Undo);
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Some stash entry holds `want` at `p` (in its files, `^2` its index, or `^3` untracked).
    fn stashed(dir: &Path, p: &str, want: &str) -> bool {
        sh(dir, &["stash", "list", "--format=%H"]).lines().any(|h| ["", "^2", "^3"].iter().any(|s| git::run(dir, &["show", &format!("{h}{s}:{p}")]).is_ok_and(|s| s == want)))
    }

    /// `base` (a=1, plus `files`), then a HEAD commit `C` with a=2 and `change`, a branch made after it, the split started.
    fn custom_split(name: &str, files: &[(&str, &str)], change: &[(&str, &str)]) -> (std::path::PathBuf, egui::Context, RepoTab, String) {
        let dir = scratch(name);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for (i, set) in [files, change].into_iter().enumerate() {
            for (f, v) in set.iter().chain(&[("a", if i == 0 { "1" } else { "2" })]) {
                std::fs::create_dir_all(dir.join(f).parent().unwrap()).unwrap();
                std::fs::write(dir.join(f), v).unwrap();
            }
            sh(&dir, &["add", "-A"]);
            sh(&dir, &["commit", "-q", "-m", if i == 0 { "base" } else { "C" }]);
        }
        let tip = sh(&dir, &["rev-parse", "HEAD"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        step(&ctx, &mut t, Action::Git(Op::new("Create branch y", &["branch", "y"]).undo(Undo::Keep)));
        step(&ctx, &mut t, Action::Split(tip.trim().into()));
        (dir, ctx, t, tip)
    }

    /// A discarded file `d` where the split's tip has a folder `d` (and the reverse) comes
    /// back (or goes to the stash list); ⌘Z never sticks on it.
    #[test]
    fn split_undo_keeps_a_backup_that_clashes_with_the_tree() {
        for file in [true, false] {
            let (dir, ctx, mut t, tip) = match file {
                true => custom_split("split-clash-file", &[], &[("d/f", "df")]),
                false => custom_split("split-clash-dir", &[("d", "1")], &[("d", "2"), ("x", "x")]),
            };
            let p = if file { "d" } else { "d/f" };
            let _ = std::fs::remove_dir_all(dir.join("d"));
            let _ = std::fs::remove_file(dir.join("d"));
            std::fs::create_dir_all(dir.join(p).parent().unwrap()).unwrap();
            std::fs::write(dir.join(p), "PRECIOUS").unwrap();
            step(&ctx, &mut t, clean(p));
            if !file {
                std::fs::remove_dir(dir.join("d")).unwrap();
                std::fs::write(dir.join("d"), "2").unwrap();
            }
            commit_part(&ctx, &mut t, if file { "a" } else { "." });
            step(&ctx, &mut t, Action::SplitEnd { abort: false });
            assert!(t.split.is_none());
            let msgs = step(&ctx, &mut t, Action::Undo);
            assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
            let on_disk = std::fs::read_to_string(dir.join(p)).is_ok_and(|s| s == "PRECIOUS");
            assert!(on_disk || stashed(&dir, p, "PRECIOUS"), "{msgs:?}");
            assert!(on_disk || msgs.iter().filter(|m| m.contains("stash")).count() == 1, "{msgs:?}");
            assert_eq!(t.undo_label(), Some("Create branch y"), "{msgs:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A staged version that differs from the file, discarded by a hard reset in the split,
    /// comes back staged with ⌘Z of the split.
    #[test]
    fn split_undo_restores_a_staged_version() {
        let (dir, ctx, mut t, tip) = custom_split("split-staged", &[("b", "1")], &[("x", "x")]);
        commit_part(&ctx, &mut t, "x");
        std::fs::write(dir.join("b"), "STAGED").unwrap();
        step(&ctx, &mut t, Action::Git(Op::new("Stage", &["add", "b"]).light()));
        std::fs::write(dir.join("b"), "WORK").unwrap();
        let head = sh(&dir, &["rev-parse", "HEAD"]);
        let mut reset = super::super::reset_op(t.data.as_ref().unwrap(), head.trim(), "--hard");
        reset.confirm = None;
        step(&ctx, &mut t, Action::Git(reset));
        step(&ctx, &mut t, Action::Git(Op::new("Restore a", &["checkout", "-q", tip.trim(), "--", "a"]).light()));
        step(&ctx, &mut t, Action::Git(Op::new("Commit", &["commit", "-q", "-m", "a"]).undo(Undo::Soft)));
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
        assert_eq!(std::fs::read_to_string(dir.join("b")).unwrap(), "WORK", "{msgs:?}");
        assert_eq!(sh(&dir, &["show", ":b"]), "STAGED", "{msgs:?}");
        assert_eq!(sh(&dir, &["stash", "list"]), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An ignored file made during an older split, where a later commit tracks that path:
    /// the abort overwrites it, so it's backed up and ⌘Z brings it back (or keeps it stashed).
    #[test]
    fn split_abort_backs_up_ignored_files_in_the_way() {
        let dir = scratch("split-abort-ignored");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, ".gitignore", ".env\n");
        commit(&dir, "a", "1");
        std::fs::write(dir.join("x"), "x").unwrap();
        commit(&dir, "a", "2");
        std::fs::write(dir.join(".env"), "committed").unwrap();
        sh(&dir, &["add", "-f", ".env"]);
        sh(&dir, &["commit", "-q", "-m", "D"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", "HEAD~1"]).trim().into()));
        assert!(t.split.as_ref().is_some_and(|s| s.rebase));
        std::fs::write(dir.join(".env"), "PRECIOUS").unwrap();
        step(&ctx, &mut t, Action::SplitEnd { abort: true });
        assert_eq!(t.undo_label(), Some(ABORT));
        let msgs = step(&ctx, &mut t, Action::Undo);
        let on_disk = std::fs::read_to_string(dir.join(".env")).is_ok_and(|s| s == "PRECIOUS");
        assert!(on_disk || stashed(&dir, ".env", "PRECIOUS"), "{msgs:?}");
        assert_eq!(t.undo_label(), None, "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `.gitignore` = `.env`, a=1, then `C` (a=2, x) and `D` (force-added `.env`): an ignored
    /// path a later commit tracks.
    fn ignored_repo(name: &str) -> (std::path::PathBuf, egui::Context, RepoTab, String) {
        let dir = scratch(name);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, ".gitignore", ".env\n");
        commit(&dir, "a", "1");
        std::fs::write(dir.join("x"), "x").unwrap();
        sh(&dir, &["add", "x"]);
        commit(&dir, "a", "2");
        std::fs::write(dir.join(".env"), "committed").unwrap();
        sh(&dir, &["add", "-f", ".env"]);
        sh(&dir, &["commit", "-q", "-m", "D"]);
        let tip = sh(&dir, &["rev-parse", "HEAD"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        step(&ctx, &mut t, Action::Git(Op::new("Create branch y", &["branch", "y"]).undo(Undo::Keep)));
        (dir, ctx, t, tip)
    }

    /// ⌘Z of a finished HEAD split, and of an older split's start, moves the refs onto a
    /// commit that tracks an ignored file made since: that file stays as it is, said once.
    #[test]
    fn split_undo_keeps_an_ignored_file_in_its_way() {
        let dir = scratch("split-undo-ignored");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, ".gitignore", ".env\n");
        commit(&dir, "a", "1");
        std::fs::write(dir.join("a"), "2").unwrap();
        std::fs::write(dir.join(".env"), "committed").unwrap();
        sh(&dir, &["add", "-f", "a", ".env"]);
        sh(&dir, &["commit", "-q", "-m", "C"]);
        let tip = sh(&dir, &["rev-parse", "HEAD"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        step(&ctx, &mut t, Action::Split(tip.trim().into()));
        std::fs::remove_file(dir.join(".env")).unwrap();
        commit_part(&ctx, &mut t, "a");
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        assert_eq!(t.undo_label().map(|l| l.starts_with("Split ")), Some(true));
        std::fs::write(dir.join(".env"), "PRECIOUS").unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
        assert_eq!(std::fs::read_to_string(dir.join(".env")).unwrap(), "PRECIOUS");
        assert_eq!(msgs.iter().filter(|m| m.contains(".env")).count(), 1, "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);

        let (dir, ctx, mut t, tip) = ignored_repo("split-undo-start-ignored");
        step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", "HEAD~1"]).trim().into()));
        std::fs::write(dir.join(".env"), "PRECIOUS").unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
        assert!(t.split.is_none());
        assert_eq!(t.data.as_ref().unwrap().state, git::State::Clean);
        assert_eq!(std::fs::read_to_string(dir.join(".env")).unwrap(), "PRECIOUS");
        // The commit's own content was uncommitted: it is the tip's again.
        assert_eq!(sh(&dir, &["status", "--porcelain"]), " M .env\n", "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Finish of an older split replays a later commit that tracks an ignored file made
    /// during the split: it's backed up, and ⌘Z keeps it (in the stash list: the path is tracked).
    #[test]
    fn split_finish_backs_up_an_ignored_file_a_later_commit_tracks() {
        let (dir, ctx, mut t, tip) = ignored_repo("split-finish-ignored");
        step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", "HEAD~1"]).trim().into()));
        assert!(t.split.as_ref().is_some_and(|s| s.rebase));
        std::fs::write(dir.join(".env"), "PRECIOUS").unwrap();
        commit_part(&ctx, &mut t, "x");
        commit_part(&ctx, &mut t, "a");
        let msgs = step(&ctx, &mut t, Action::SplitEnd { abort: false });
        assert!(t.split.is_none(), "{msgs:?}");
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
        let on_disk = std::fs::read_to_string(dir.join(".env")).is_ok_and(|s| s == "PRECIOUS");
        assert!(on_disk || stashed(&dir, ".env", "PRECIOUS"), "{msgs:?}");
        assert_eq!(t.undo_label(), Some("Create branch y"), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A staged-only version an older split's Abort discards comes back staged with ⌘Z.
    #[test]
    fn split_abort_undo_restores_a_staged_version() {
        let (dir, ctx, mut t, _) = older_repo("split-abort-staged");
        std::fs::write(dir.join("q"), "STAGED").unwrap();
        step(&ctx, &mut t, Action::Git(Op::new("Stage", &["add", "q"]).light()));
        std::fs::write(dir.join("q"), "WORK").unwrap();
        step(&ctx, &mut t, Action::SplitEnd { abort: true });
        assert_eq!(std::fs::read_to_string(dir.join("q")).unwrap(), "1");
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(std::fs::read_to_string(dir.join("q")).unwrap(), "WORK", "{msgs:?}");
        assert!(sh(&dir, &["show", ":q"]) == "STAGED" || stashed(&dir, "q", "STAGED"), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A name git won't store (`git~1`) is never discarded: no backup could hold it.
    #[cfg(not(windows))] // there `git~1` is .git's 8.3 short name: no such file can exist
    #[test]
    fn a_name_git_cannot_store_is_not_discarded() {
        let (dir, ctx, mut t, _) = split_repo("split-unstorable");
        std::fs::write(dir.join("git~1"), "PRECIOUS").unwrap();
        let msgs = step(&ctx, &mut t, clean("git~1"));
        assert_eq!(std::fs::read_to_string(dir.join("git~1")).ok().as_deref(), Some("PRECIOUS"), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("Nothing was changed")), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }


    /// `want` in some file on disk or in some stash entry (its files, index or untracked files).
    fn somewhere(dir: &Path, want: &str) -> bool {
        fn walk(p: &Path, want: &str) -> bool {
            std::fs::read_dir(p).into_iter().flatten().flatten().any(|e| {
                let q = e.path();
                match std::fs::symlink_metadata(&q) {
                    _ if q.file_name().is_some_and(|n| n == ".git") => false,
                    Ok(m) if m.is_dir() => walk(&q, want),
                    _ => std::fs::read(&q).is_ok_and(|b| b == want.as_bytes()),
                }
            })
        }
        walk(dir, want)
            || sh(dir, &["stash", "list", "--format=%H"]).lines().any(|h| {
                ["", "^2", "^3"].iter().any(|s| {
                    let tree = git::run(dir, &["ls-tree", "-r", "-z", &format!("{h}{s}")]).unwrap_or_default();
                    tree.split('\0').filter_map(|l| l.split_once('\t')).any(|(m, _)| git::run(dir, &["cat-file", "blob", m.rsplit(' ').next().unwrap()]).is_ok_and(|b| b == want))
                })
            })
    }

    fn labels(t: &RepoTab) -> Vec<String> {
        t.undo.iter().map(|e| e.label.clone()).collect()
    }

    /// The change at `p` as the WIP list shows it, discarded the way its menu does.
    fn discard_row(ctx: &egui::Context, t: &mut RepoTab, p: &str) -> Vec<String> {
        step(ctx, t, Action::Reload);
        let c = t.data.as_ref().unwrap().status.unstaged.iter().find(|c| c.path == p).cloned().unwrap_or_else(|| panic!("no {p}"));
        let mut op = crate::repo::discard_op(&c);
        op.confirm = None;
        step(ctx, t, Action::Git(op))
    }

    /// A HEAD commit `C` that turns file `d` into folder `d/f` (or, `reverse`, the other way), split.
    fn swap_repo(name: &str, reverse: bool) -> (std::path::PathBuf, egui::Context, RepoTab, String) {
        let dir = scratch(name);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let (old, new) = if reverse { ("d/f", "d") } else { ("d", "d/f") };
        for (p, v) in [(old, "1"), (new, "C")] {
            let _ = std::fs::remove_dir_all(dir.join("d"));
            let _ = std::fs::remove_file(dir.join("d"));
            std::fs::create_dir_all(dir.join(p).parent().unwrap()).unwrap();
            std::fs::write(dir.join(p), v).unwrap();
            std::fs::write(dir.join("a"), v).unwrap();
            sh(&dir, &["add", "-A"]);
            sh(&dir, &["commit", "-q", "-m", v]);
        }
        let tip = sh(&dir, &["rev-parse", "HEAD"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        step(&ctx, &mut t, Action::Git(Op::new("Create branch y", &["branch", "y"]).undo(Undo::Keep)));
        step(&ctx, &mut t, Action::Split(tip.trim().into()));
        (dir, ctx, t, tip)
    }

    /// In a split of a commit that turns a file into a folder (or the
    /// reverse), the new path edited, then the old path's deletion discarded: the edit is backed
    /// up (git's checkout removes it) and ⌘Z brings it back.
    #[test]
    fn p1_discarding_a_swapped_path_backs_up_what_is_in_its_place() {
        for reverse in [false, true] {
            let (dir, ctx, mut t, _) = swap_repo(if reverse { "split-p1r" } else { "split-p1" }, reverse);
            let (old, new) = if reverse { ("d/f", "d") } else { ("d", "d/f") };
            std::fs::write(dir.join(new), "PRECIOUS").unwrap();
            let msgs = discard_row(&ctx, &mut t, old);
            let msgs = [msgs, step(&ctx, &mut t, Action::Undo)].concat();
            assert_eq!(std::fs::read_to_string(dir.join(new)).ok().as_deref(), Some("PRECIOUS"), "{msgs:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// ⌘Z of a HEAD split's start (a mixed reset) never reads the commit's
    /// big added file (sparse, past the diff limit): it goes through, and the steps before stay.
    #[test]
    fn p9_undo_of_a_split_start_with_a_big_added_file() {
        let dir = scratch("split-p9");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "1");
        std::fs::OpenOptions::new().create(true).write(true).open(dir.join("big.bin")).unwrap().set_len(16 << 20).unwrap();
        std::fs::write(dir.join("a"), "2").unwrap();
        sh(&dir, &["add", "-A"]);
        sh(&dir, &["commit", "-q", "-m", "C"]);
        let tip = sh(&dir, &["rev-parse", "HEAD"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        step(&ctx, &mut t, Action::Git(Op::new("Create branch y", &["branch", "y"]).undo(Undo::Keep)));
        step(&ctx, &mut t, Action::Split(tip.trim().into()));
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
        assert!(t.split.is_none() && t.choice.is_none());
        assert_eq!(labels(&t), ["Create branch y"]);
        assert_eq!(sh(&dir, &["status", "--porcelain"]), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A staged version made during a split is never lost by its
    /// Abort: a HEAD split's (a mixed reset) and an older split's (worktree back as the commit).
    #[test]
    fn split_abort_keeps_a_staged_version() {
        for older in [false, true] {
            let (dir, ctx, mut t) = match older {
                false => {
                    let (d, c, t, _) = split_repo("split-p2");
                    (d, c, t)
                }
                true => {
                    let (d, c, t, _) = older_repo("split-p3");
                    (d, c, t)
                }
            };
            std::fs::write(dir.join("q"), "STAGED").unwrap();
            step(&ctx, &mut t, Action::Git(Op::new("Stage", &["add", "q"]).light()));
            std::fs::write(dir.join("q"), if older { "1" } else { "WORK" }).unwrap();
            let msgs = step(&ctx, &mut t, Action::SplitEnd { abort: true });
            let msgs = [msgs, step(&ctx, &mut t, Action::Undo)].concat();
            assert!(sh(&dir, &["show", ":q"]) == "STAGED" || stashed(&dir, "q", "STAGED"), "{older} {msgs:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A failed Continue, the rebase then aborted in a terminal: the next
    /// op ends the split, and ⌘Z of everything keeps the discarded file and never wipes the steps.
    #[test]
    fn p4_split_ended_in_a_terminal_undoes_cleanly() {
        let (dir, ctx, mut t, tip) = older_repo("split-p4");
        std::fs::write(dir.join("b"), "FIVE-P4").unwrap();
        step(&ctx, &mut t, discard("b"));
        step(&ctx, &mut t, Action::Git(Op::new("Stage", &["add", "-A"]).light()));
        step(&ctx, &mut t, Action::Git(Op::new("Commit", &["commit", "-q", "-m", "rest"]).undo(Undo::Soft)));
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        sh(&dir, &["rebase", "--abort"]);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip);
        step(&ctx, &mut t, Action::Reload);
        std::fs::write(dir.join("new"), "MINE").unwrap();
        step(&ctx, &mut t, Action::Git(Op::new("Stage", &["add", "new"]).light()));
        step(&ctx, &mut t, Action::Git(Op::new("Commit", &["commit", "-q", "-m", "mine"]).undo(Undo::Soft)));
        for _ in 0..4 {
            let msgs = step(&ctx, &mut t, Action::Undo);
            assert!(t.choice.is_none(), "{msgs:?}");
        }
        assert!(t.undo.is_empty(), "{:?}", labels(&t));
        assert!(sh(&dir, &["branch", "--list", "y"]).is_empty());
        assert!(somewhere(&dir, "FIVE-P4") && somewhere(&dir, "MINE"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Case-only renames in a split, with a discard of that file:
    /// finish, undo, redo, undo: nothing lost (the file keeps one of its names).
    #[test]
    fn case_renames_in_a_split_lose_nothing() {
        for older in [false, true] {
            let dir = scratch(if older { "split-case-older" } else { "split-case-head" });
            sh(&dir, &["init", "-q", "-b", "main"]);
            identity(&dir);
            std::fs::write(dir.join("notes"), "1").unwrap();
            commit(&dir, "a", "1");
            sh(&dir, &["add", "-A"]);
            sh(&dir, &["commit", "-q", "-m", "base"]);
            sh(&dir, &["mv", "notes", "Notes"]);
            std::fs::write(dir.join("Notes"), "2").unwrap();
            std::fs::write(dir.join("a"), "2").unwrap();
            sh(&dir, &["add", "-A"]);
            sh(&dir, &["commit", "-q", "-m", "C"]);
            if older {
                commit(&dir, "z", "z");
            }
            let ctx = egui::Context::default();
            let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
            settle(&ctx, &mut t);
            step(&ctx, &mut t, Action::Git(Op::new("Create branch y", &["branch", "y"]).undo(Undo::Keep)));
            step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", if older { "HEAD~1" } else { "HEAD" }]).trim().into()));
            std::fs::write(dir.join("Notes"), "PRECIOUS5").unwrap();
            step(&ctx, &mut t, Action::Reload);
            // The change holding the edit: `notes` on a case-insensitive disk; on a case-sensitive
            // one `notes` is a deletion and the edit is the untracked `Notes`.
            let unstaged = &t.data.as_ref().unwrap().status.unstaged;
            let edit = unstaged.iter().find(|c| c.path == "Notes").or_else(|| unstaged.iter().find(|c| c.path.eq_ignore_ascii_case("notes")));
            if let Some(c) = edit.cloned() {
                let mut o = crate::repo::discard_op(&c);
                o.confirm = None;
                step(&ctx, &mut t, Action::Git(o));
            }
            commit_part(&ctx, &mut t, "a");
            step(&ctx, &mut t, Action::SplitEnd { abort: false });
            if let Some(c) = t.choice.take() {
                step(&ctx, &mut t, Action::Git(c.ops[0].1.clone()));
            }
            if t.data.as_ref().unwrap().state == git::State::Rebasing {
                let mut ab = super::super::sequencer_op("rebase", true);
                ab.confirm = None;
                step(&ctx, &mut t, Action::Git(ab));
            }
            for a in [Action::Undo, Action::Redo, Action::Undo, Action::Undo] {
                step(&ctx, &mut t, a);
                assert!(t.choice.is_none());
            }
            assert!(somewhere(&dir, "PRECIOUS5"), "{older}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// ⌘Z of an older split's start goes
    /// through (the commit's changes stay as they are) and ends the split; nothing is lost.
    #[test]
    fn p6_undo_past_an_older_split_start() {
        let (dir, ctx, mut t, tip) = older_repo("split-p6");
        std::fs::write(dir.join("u"), "PRECIOUS6").unwrap();
        step(&ctx, &mut t, clean("u"));
        commit_part(&ctx, &mut t, "x");
        for _ in 0..3 {
            let msgs = step(&ctx, &mut t, Action::Undo);
            assert!(t.choice.is_none(), "{msgs:?}");
        }
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip);
        assert!(t.split.is_none());
        assert_eq!(t.data.as_ref().unwrap().state, git::State::Clean);
        assert!(somewhere(&dir, "PRECIOUS6"));
        assert_eq!(labels(&t), ["Create branch y"]);
        step(&ctx, &mut t, Action::Undo);
        assert!(sh(&dir, &["branch", "--list", "y"]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Three backups (a tracked file twice, an untracked one) in a
    /// HEAD or older split, aborted or finished, then undo, redo, undo: none is lost, ⌘Z never
    /// sticks, and it gets past the split.
    #[test]
    fn three_backups_in_a_split_survive_undo_and_redo() {
        for (n, abort, older) in [(0, true, false), (1, false, false), (2, true, true), (3, false, true)] {
            let dir = scratch(&format!("split-three-{n}"));
            sh(&dir, &["init", "-q", "-b", "main"]);
            identity(&dir);
            for f in ["a", "b"] {
                std::fs::write(dir.join(f), "1").unwrap();
            }
            sh(&dir, &["add", "."]);
            sh(&dir, &["commit", "-q", "-m", "base"]);
            std::fs::write(dir.join("a"), "2").unwrap();
            std::fs::write(dir.join("x"), "x").unwrap();
            sh(&dir, &["add", "."]);
            sh(&dir, &["commit", "-q", "-m", "C"]);
            if older {
                commit(&dir, "z", "z");
            }
            let ctx = egui::Context::default();
            let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
            settle(&ctx, &mut t);
            step(&ctx, &mut t, Action::Git(Op::new("Create branch y", &["branch", "y"]).undo(Undo::Keep)));
            step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", if older { "HEAD~1" } else { "HEAD" }]).trim().into()));
            std::fs::write(dir.join("b"), "B1").unwrap();
            step(&ctx, &mut t, discard("b"));
            std::fs::write(dir.join("u"), "U1").unwrap();
            step(&ctx, &mut t, clean("u"));
            std::fs::write(dir.join("b"), "B2").unwrap();
            step(&ctx, &mut t, discard("b"));
            commit_part(&ctx, &mut t, "x");
            if !abort {
                commit_part(&ctx, &mut t, "a");
            }
            step(&ctx, &mut t, Action::SplitEnd { abort });
            for a in [Action::Undo, Action::Redo, Action::Undo, Action::Undo] {
                let msgs = step(&ctx, &mut t, a);
                assert!(t.choice.is_none(), "[{n}] {msgs:?}");
            }
            for w in ["B1", "B2", "U1"] {
                assert!(somewhere(&dir, w), "[{n}] lost {w}");
            }
            assert!(sh(&dir, &["branch", "--list", "y"]).is_empty(), "[{n}] {:?}", labels(&t));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// An older split of a file -> folder commit, the folder's file edited,
    /// Abort, ⌘Z: the edit comes back (or waits in the stash list).
    #[test]
    fn p8_older_file_to_folder_abort_undo() {
        let dir = scratch("split-p8");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        std::fs::write(dir.join("d"), "1").unwrap();
        commit(&dir, "a", "1");
        sh(&dir, &["add", "d"]);
        sh(&dir, &["commit", "-q", "-m", "d"]);
        std::fs::remove_file(dir.join("d")).unwrap();
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/f"), "C").unwrap();
        sh(&dir, &["add", "-A"]);
        sh(&dir, &["commit", "-q", "-m", "C"]);
        commit(&dir, "b", "3");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        step(&ctx, &mut t, Action::Git(Op::new("Create branch y", &["branch", "y"]).undo(Undo::Keep)));
        step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", "HEAD~1"]).trim().into()));
        std::fs::write(dir.join("d/f"), "PRECIOUS8").unwrap();
        step(&ctx, &mut t, Action::SplitEnd { abort: true });
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(std::fs::read_to_string(dir.join("d/f")).ok().as_deref(), Some("PRECIOUS8"), "{msgs:?}");
        assert_eq!(labels(&t), ["Create branch y"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A HEAD split of a file -> folder commit, a discard, finish through "Commit the
    /// rest"; undo, redo, undo, undo: nothing lost, never stuck.
    #[test]
    fn p10_file_to_folder_split_finish_undo_redo() {
        let (dir, ctx, mut t, _) = swap_repo("split-p10", false);
        std::fs::write(dir.join("a"), "P10-A").unwrap();
        step(&ctx, &mut t, discard("a"));
        commit_part(&ctx, &mut t, "a");
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        if let Some(c) = t.choice.take() {
            step(&ctx, &mut t, Action::Git(c.ops[0].1.clone()));
        }
        for a in [Action::Undo, Action::Redo, Action::Undo, Action::Undo] {
            let msgs = step(&ctx, &mut t, a);
            assert!(t.choice.is_none(), "{msgs:?}");
        }
        assert!(somewhere(&dir, "P10-A"));
        assert!(t.undo.is_empty(), "{:?}", labels(&t));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An older split of a case-only rename, with a later commit editing it:
    /// finish (the replay conflicts, then abort), abort, or just undo: nothing lost.
    #[test]
    fn p12_case_rename_in_an_older_split() {
        for mode in ["finish", "abort", "undo"] {
            let dir = scratch(&format!("split-p12{mode}"));
            sh(&dir, &["init", "-q", "-b", "main"]);
            identity(&dir);
            std::fs::write(dir.join("readme"), "1\n").unwrap();
            commit(&dir, "a", "1");
            sh(&dir, &["add", "-A"]);
            sh(&dir, &["commit", "-q", "-m", "r"]);
            sh(&dir, &["mv", "readme", "README"]);
            std::fs::write(dir.join("README"), "1\n2\n").unwrap();
            std::fs::write(dir.join("a"), "2").unwrap();
            sh(&dir, &["add", "-A"]);
            sh(&dir, &["commit", "-q", "-m", "C"]);
            std::fs::write(dir.join("README"), "1\n2\n3\n").unwrap();
            sh(&dir, &["add", "-A"]);
            sh(&dir, &["commit", "-q", "-m", "D"]);
            let ctx = egui::Context::default();
            let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
            settle(&ctx, &mut t);
            step(&ctx, &mut t, Action::Git(Op::new("Create branch y", &["branch", "y"]).undo(Undo::Keep)));
            step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", "HEAD~1"]).trim().into()));
            std::fs::write(dir.join("u"), "P12-U").unwrap();
            std::fs::write(dir.join("README"), "1\n2\nMINE\n").unwrap();
            match mode {
                "finish" => {
                    commit_part(&ctx, &mut t, "a");
                    step(&ctx, &mut t, Action::SplitEnd { abort: false });
                    if let Some(c) = t.choice.take() {
                        step(&ctx, &mut t, Action::Git(c.ops[0].1.clone()));
                    }
                    if t.data.as_ref().unwrap().state == git::State::Rebasing {
                        let mut ab = super::super::sequencer_op("rebase", true);
                        ab.confirm = None;
                        step(&ctx, &mut t, Action::Git(ab));
                    }
                }
                "abort" => drop(step(&ctx, &mut t, Action::SplitEnd { abort: true })),
                _ => {}
            }
            for _ in 0..3 {
                let msgs = step(&ctx, &mut t, Action::Undo);
                assert!(t.choice.is_none(), "{mode} {msgs:?}");
            }
            assert!(somewhere(&dir, "P12-U"), "{mode} lost u");
            assert!(somewhere(&dir, "1\n2\nMINE\n"), "{mode} lost README");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A backed-up `d/f` whose folder became a symlink to a
    /// folder outside the repo: ⌘Z never writes through it.
    #[cfg(unix)]
    #[test]
    fn split_undo_never_writes_through_a_symlinked_folder() {
        let (dir, ctx, mut t, tip) = split_repo("split-symlink");
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/f"), "PRECIOUS").unwrap();
        step(&ctx, &mut t, clean("d/f"));
        commit_part(&ctx, &mut t, ".");
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        let _ = std::fs::remove_dir_all(dir.join("d"));
        let other = scratch("split-symlink-target");
        std::fs::write(other.join("f"), "OTHER").unwrap();
        std::os::unix::fs::symlink(&other, dir.join("d")).unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
        assert_eq!(std::fs::read_to_string(other.join("f")).unwrap(), "OTHER");
        assert!(somewhere(&dir, "PRECIOUS"), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An older split's Continue conflicts, ⌘Z waits for the
    /// rebase, the user resolves (and makes an untracked file) and continues; ⌘Z then undoes it all.
    #[test]
    fn split_continue_conflict_resolved_then_undone() {
        let dir = scratch("split-cont");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for f in ["a", "b"] {
            std::fs::write(dir.join(f), "1").unwrap();
        }
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-q", "-m", "base"]);
        for f in ["a", "b"] {
            std::fs::write(dir.join(f), "2").unwrap();
        }
        sh(&dir, &["commit", "-q", "-am", "C"]);
        commit(&dir, "b", "3");
        let tip = sh(&dir, &["rev-parse", "HEAD"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        step(&ctx, &mut t, Action::Git(Op::new("Create branch y", &["branch", "y"]).undo(Undo::Keep)));
        step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", "HEAD~1"]).trim().into()));
        std::fs::write(dir.join("u"), "U1").unwrap();
        step(&ctx, &mut t, clean("u"));
        commit_part(&ctx, &mut t, "a");
        std::fs::write(dir.join("b"), "X").unwrap();
        commit_part(&ctx, &mut t, "b");
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        assert_eq!(t.data.as_ref().unwrap().state, git::State::Rebasing);
        step(&ctx, &mut t, Action::Undo);
        assert!(t.notes.drain(..).any(|n| n.text.contains("Finish or abort")));
        std::fs::write(dir.join("w"), "W1").unwrap();
        std::fs::write(dir.join("b"), "3").unwrap();
        step(&ctx, &mut t, Action::Git(Op::new("Stage", &["add", "b"]).light()));
        step(&ctx, &mut t, Action::Git(Op::new("Continue rebase", &["-c", "core.editor=true", "rebase", "--continue"]).undo(Undo::Keep)));
        for _ in 0..3 {
            let msgs = step(&ctx, &mut t, Action::Undo);
            assert!(t.choice.is_none(), "{msgs:?}");
        }
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip);
        assert!(somewhere(&dir, "U1") && somewhere(&dir, "W1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// After a finished split, an edit to a file that differs between the parts and
    /// the tip: ⌘Z goes through around it (said once) instead of failing.
    #[test]
    fn split_undo_keeps_an_edit_made_after_it() {
        let (dir, ctx, mut t, tip) = split_repo("split-p11");
        std::fs::write(dir.join("a"), "3").unwrap();
        commit_part(&ctx, &mut t, "a");
        commit_part(&ctx, &mut t, ".");
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        std::fs::write(dir.join("a"), "LATE").unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "LATE");
        assert_eq!(msgs.iter().filter(|m| m.contains("Kept your changes to a")).count(), 1, "{msgs:?}");
        assert_eq!(labels(&t), ["Create branch y"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A split with backups redoes (its parts come back) and undoes again.
    #[test]
    fn a_split_with_backups_redoes() {
        let (dir, ctx, mut t, tip) = split_repo("split-redo");
        std::fs::write(dir.join("b"), "FIVE").unwrap();
        step(&ctx, &mut t, discard("b"));
        std::fs::write(dir.join("c"), "SIX").unwrap();
        commit_part(&ctx, &mut t, "c");
        commit_part(&ctx, &mut t, ".");
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        let parts = sh(&dir, &["rev-parse", "HEAD"]);
        step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip);
        assert_eq!(std::fs::read_to_string(dir.join("b")).unwrap(), "FIVE");
        let msgs = step(&ctx, &mut t, Action::Redo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), parts, "{msgs:?}");
        assert_eq!((std::fs::read_to_string(dir.join("b")).unwrap(), sh(&dir, &["status", "--porcelain"])), ("1".into(), String::new()), "{msgs:?}");
        step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip);
        assert_eq!(std::fs::read_to_string(dir.join("b")).unwrap(), "FIVE");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn new_repo(name: &str) -> (std::path::PathBuf, egui::Context) {
        let dir = scratch(name);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        (dir, egui::Context::default())
    }

    fn open(ctx: &egui::Context, dir: &Path) -> RepoTab {
        let mut t = RepoTab::new(ctx, dir.to_path_buf(), 100, View::default());
        settle(ctx, &mut t);
        t
    }

    /// Starting an older split checks out a commit that tracks an ignored file: it's backed
    /// up first, and ⌘Z through the split brings it back.
    #[test]
    fn older_split_start_backs_up_an_ignored_file() {
        let (dir, ctx) = new_repo("split-b1");
        commit(&dir, ".env", "old");
        commit(&dir, "a", "1");
        commit(&dir, "a", "2");
        sh(&dir, &["rm", "-q", "--cached", ".env"]);
        std::fs::write(dir.join(".gitignore"), ".env\n").unwrap();
        sh(&dir, &["add", ".gitignore"]);
        sh(&dir, &["commit", "-q", "-m", "untrack env"]);
        std::fs::write(dir.join(".env"), "PRECIOUS").unwrap();
        let mut t = open(&ctx, &dir);
        step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", "HEAD~1"]).trim().into()));
        assert!(t.split.is_some());
        for _ in 0..2 {
            step(&ctx, &mut t, Action::Undo);
        }
        assert!(somewhere(&dir, "PRECIOUS"));
        assert_eq!(std::fs::read_to_string(dir.join(".env")).ok().as_deref(), Some("PRECIOUS"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed split Continue, resolved, then the banner's Continue rebase replays a commit
    /// that tracks an ignored file made meanwhile: it's backed up.
    #[test]
    fn continue_after_a_failed_split_finish_backs_up_an_ignored_file() {
        let (dir, ctx) = new_repo("split-b2");
        commit(&dir, ".gitignore", ".env\n");
        for f in ["a", "b"] {
            std::fs::write(dir.join(f), "1").unwrap();
        }
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-q", "-m", "base"]);
        for f in ["a", "b"] {
            std::fs::write(dir.join(f), "2").unwrap();
        }
        sh(&dir, &["commit", "-q", "-am", "C"]);
        commit(&dir, "b", "3");
        std::fs::write(dir.join(".env"), "committed").unwrap();
        sh(&dir, &["add", "-f", ".env"]);
        sh(&dir, &["commit", "-q", "-m", "E"]);
        let mut t = open(&ctx, &dir);
        step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", "HEAD~2"]).trim().into()));
        commit_part(&ctx, &mut t, "a");
        std::fs::write(dir.join("b"), "X").unwrap();
        commit_part(&ctx, &mut t, "b");
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        assert_eq!(git::state(&t.data.as_ref().unwrap().git_dir), git::State::Rebasing);
        std::fs::write(dir.join(".env"), "PRECIOUS").unwrap();
        std::fs::write(dir.join("b"), "3").unwrap();
        step(&ctx, &mut t, Action::Git(Op::new("Stage", &["add", "b"]).light()));
        step(&ctx, &mut t, Action::Git(Op::new("Continue rebase", &["-c", "core.editor=true", "rebase", "--continue"]).undo(Undo::Keep)));
        assert_eq!(std::fs::read_to_string(dir.join(".env")).unwrap(), "committed");
        for _ in 0..3 {
            step(&ctx, &mut t, Action::Undo);
        }
        assert!(somewhere(&dir, "PRECIOUS"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tag made mid-split (a terminal, auto-fetch) never cuts the fold short:
    /// ⌘Z of the split brings back the original commit, keeps the tag and the step before.
    #[test]
    fn a_tag_made_mid_split_keeps_the_fold_whole() {
        for older in [false, true] {
            let (dir, ctx) = new_repo(if older { "split-b3o" } else { "split-b3" });
            commit(&dir, "a", "1");
            for (f, v) in [("a", "A"), ("b", "B"), ("c", "C")] {
                std::fs::write(dir.join(f), v).unwrap();
            }
            sh(&dir, &["add", "."]);
            sh(&dir, &["commit", "-q", "-m", "C"]);
            if older {
                commit(&dir, "z", "z");
            }
            let tip = sh(&dir, &["rev-parse", "HEAD"]);
            let mut t = open(&ctx, &dir);
            step(&ctx, &mut t, Action::Git(Op::new("Create branch y", &["branch", "y"]).undo(Undo::Keep)));
            step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", if older { "HEAD~1" } else { "HEAD" }]).trim().into()));
            commit_part(&ctx, &mut t, "a");
            sh(&dir, &["tag", "v1", "HEAD"]);
            commit_part(&ctx, &mut t, "b");
            commit_part(&ctx, &mut t, "c");
            step(&ctx, &mut t, Action::SplitEnd { abort: false });
            assert_eq!(labels(&t).len(), 2, "{:?}", labels(&t));
            step(&ctx, &mut t, Action::Undo);
            assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "older={older}");
            assert_eq!(sh(&dir, &["symbolic-ref", "--short", "HEAD"]), "main\n");
            assert_eq!(sh(&dir, &["status", "--porcelain"]), "");
            assert!(git::run(&dir, &["rev-parse", "--verify", "v1"]).is_ok());
            assert_eq!(labels(&t), ["Create branch y"]);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A branch deleted during a split keeps its own undo step after the split ends.
    #[test]
    fn a_branch_deleted_mid_split_stays_undoable() {
        let (dir, ctx, mut t, tip) = split_repo("split-b4");
        let tree = sh(&dir, &["rev-parse", "HEAD^{tree}"]).trim().to_owned();
        let lone = sh(&dir, &["commit-tree", &tree, "-m", "only on x"]).trim().to_owned();
        sh(&dir, &["branch", "x", &lone]);
        step(&ctx, &mut t, Action::Reload);
        let del = crate::repo::delete_branches(t.data.as_ref().unwrap(), "Delete x".into(), &["x"], true);
        step(&ctx, &mut t, Action::Git(del));
        for p in ["a", "b", "c", "x"] {
            commit_part(&ctx, &mut t, p);
        }
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        assert!(labels(&t).contains(&"Delete x".to_owned()), "{:?}", labels(&t));
        while !t.undo.is_empty() {
            step(&ctx, &mut t, Action::Undo);
        }
        assert_eq!(sh(&dir, &["rev-parse", "x"]).trim(), lone);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An older split continued in a terminal stops on a conflict; ⌘Z past the split
    /// start moves nothing then (HEAD moved outside), so the terminal's rebase keeps running.
    #[test]
    fn undo_past_the_start_keeps_a_terminal_rebase() {
        let (dir, ctx) = new_repo("split-terminal-rebase");
        for f in ["a", "b"] {
            std::fs::write(dir.join(f), "1").unwrap();
        }
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-q", "-m", "base"]);
        for f in ["a", "b"] {
            std::fs::write(dir.join(f), "2").unwrap();
        }
        sh(&dir, &["commit", "-q", "-am", "C"]);
        commit(&dir, "b", "3");
        let mut t = open(&ctx, &dir);
        step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", "HEAD~1"]).trim().into()));
        commit_part(&ctx, &mut t, "a");
        std::fs::write(dir.join("b"), "X").unwrap();
        sh(&dir, &["commit", "-qam", "b part"]);
        assert!(git::run(&dir, &["-c", "core.editor=true", "rebase", "--continue"]).is_err());
        step(&ctx, &mut t, Action::Reload);
        while !t.undo.is_empty() {
            step(&ctx, &mut t, Action::Undo);
        }
        assert_eq!(git::state(&t.data.as_ref().unwrap().git_dir), git::State::Rebasing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A branch or tag made mid-split is no part of it: each stays its own step, under the
    /// folded one, so ⌘Z of the split keeps them.
    #[test]
    fn a_branch_made_mid_split_stays_its_own_step() {
        let (dir, ctx) = new_repo("split-own-steps");
        commit(&dir, "p", "p0\n");
        std::fs::write(dir.join("q"), "q\n").unwrap();
        std::fs::write(dir.join("p"), "p1\n").unwrap();
        sh(&dir, &["add", "-A"]);
        sh(&dir, &["commit", "-q", "-m", "two files"]);
        let head = sh(&dir, &["rev-parse", "HEAD"]);
        let mut t = open(&ctx, &dir);
        step(&ctx, &mut t, Action::Git(Op::new("Create branch before", &["branch", "before"]).undo(Undo::Keep)));
        step(&ctx, &mut t, Action::Split(head.trim().into()));
        commit_part(&ctx, &mut t, "p");
        step(&ctx, &mut t, Action::Git(Op::new("Create branch keepme", &["branch", "keepme"]).undo(Undo::Keep)));
        step(&ctx, &mut t, Action::Git(Op::new("Create tag v1", &["tag", "v1"]).undo(Undo::Keep)));
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        let c = t.choice.take().expect("left over");
        step(&ctx, &mut t, Action::Git(c.ops[0].1.clone()));
        assert_eq!(labels(&t), ["Create branch before", "Create branch keepme", "Create tag v1", &format!("Split {}", &head[..7])]);
        step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), head);
        assert!(git::run(&dir, &["rev-parse", "--verify", "keepme"]).is_ok() && git::run(&dir, &["rev-parse", "--verify", "v1"]).is_ok());
        assert_eq!(labels(&t), ["Create branch before", "Create branch keepme", "Create tag v1"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A HEAD split's "Commit the rest" refused by a hook keeps the split going, and its ⌘Z
    /// brings back the staged version the add replaced.
    #[test]
    fn a_refused_commit_the_rest_keeps_the_split_and_the_staged_version() {
        let (dir, ctx) = new_repo("split-hook");
        for f in ["a", "b"] {
            std::fs::write(dir.join(f), "1").unwrap();
        }
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-qm", "base"]);
        for f in ["a", "b"] {
            std::fs::write(dir.join(f), "2").unwrap();
        }
        sh(&dir, &["commit", "-qam", "C"]);
        let mut t = open(&ctx, &dir);
        step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", "HEAD"]).trim().into()));
        commit_part(&ctx, &mut t, "a");
        std::fs::write(dir.join("b"), "STAGED").unwrap();
        step(&ctx, &mut t, Action::Git(Op::new("Stage", &["add", "b"]).light()));
        std::fs::write(dir.join("b"), "WT").unwrap();
        let hook = dir.join(".git/hooks/pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        step(&ctx, &mut t, Action::SplitEnd { abort: false });
        let c = t.choice.take().expect("left over");
        let m = step(&ctx, &mut t, Action::Git(c.ops[0].1.clone()));
        assert!(t.split.is_some(), "{m:?}");
        let m = step(&ctx, &mut t, Action::Undo);
        assert_eq!((sh(&dir, &["show", ":b"]).as_str(), std::fs::read_to_string(dir.join("b")).unwrap().as_str()), ("STAGED", "WT"), "{m:?}");
        assert!(t.split.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
