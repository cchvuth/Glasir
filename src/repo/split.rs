//! Guided commit split: the commit is uncommitted (`rebase::split`), the user
//! stages and commits the parts, then Continue replays the rest of the branch.
//! The whole split becomes one undo step.

use super::undo::{UndoEntry, UndoKind};
use super::{Backup, Choice, Msg, Op, RepoTab, Undo};
use crate::git;
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub struct Split {
    pub id: String,
    /// An older commit: a rebase is stopped at it (a HEAD split needs none).
    pub rebase: bool,
    /// Paths the commit touched: what "left over" means at Continue.
    files: Vec<String>,
    /// The original message, for the last part.
    message: String,
    /// Undo entries before the split; the ones after are merged when it ends.
    undo_len: usize,
    /// `symbolic-ref HEAD` at the start (empty when detached): Abort resets only this branch.
    branch: String,
}

/// Label of the ops that end a split (matched in `poll_op`).
pub(super) const FINISH: &str = "Finish split";
pub(super) const ABORT: &str = "Abort split";

/// What `reset --hard <rev>` loses: paths whose worktree bytes differ from `rev`.
/// Not the split commit's own content, which the abort brings back anyway.
fn changed_since(dir: &Path, rev: &str) -> Backup {
    let z = |args: &[&str]| git::run(dir, args).map(|o| o.split('\0').filter(|p| !p.is_empty()).map(String::from).collect::<Vec<_>>());
    let (Ok(mut paths), Ok(untracked)) = (z(&["diff", "--name-only", "-z", "--no-renames", rev]), z(&["ls-files", "-z", "--others", "--exclude-standard"])) else {
        return Backup::All;
    };
    paths.extend(untracked);
    paths.sort_unstable();
    paths.dedup();
    if paths.is_empty() {
        return Backup::Paths(paths);
    }
    // The commit's own files show up as untracked (or deleted from the index) with `rev`'s bytes.
    let mut ls = vec!["--literal-pathspecs", "ls-tree", "-r", "-z", rev, "--"];
    ls.extend(paths.iter().map(String::as_str));
    let Ok(tree) = z(&ls) else { return Backup::All };
    let blobs: HashMap<&str, &str> = tree.iter().filter_map(|r| r.split_once('\t').and_then(|(m, p)| Some((p, m.rsplit(' ').next()?)))).collect();
    let cand: Vec<String> = paths.iter().filter(|p| blobs.contains_key(p.as_str()) && dir.join(p).is_file()).cloned().collect();
    if !cand.is_empty() {
        let mut hash = vec!["hash-object", "--"];
        hash.extend(cand.iter().map(String::as_str));
        let Ok(out) = git::run(dir, &hash) else { return Backup::All };
        let same: HashSet<&str> = cand.iter().zip(out.lines()).filter(|(p, h)| blobs[p.as_str()] == *h).map(|(p, _)| p.as_str()).collect();
        paths.retain(|p| !same.contains(p.as_str()));
    }
    Backup::Paths(paths)
}

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
                self.split = Some(Split { id: commit, rebase: !is_head, files, message, undo_len: self.undo.len(), branch });
                self.run(ctx, op);
            }
            Err(e) => self.notes.push(Msg { text: e, error: true }),
        }
    }

    /// Continue: replay the rest when nothing of the commit is left uncommitted,
    /// otherwise offer to commit the rest (or Cancel to stash / discard it first). Abort: back to the original commit.
    pub(super) fn end_split(&mut self, ctx: &egui::Context, abort: bool) {
        let (Some(sp), Some(d)) = (&self.split, &self.data) else { return };
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
        let cont = || Op::new(FINISH, &["-c", "core.editor=true", "rebase", "--continue"]).undo(Undo::Keep);
        if abort {
            let op = match sp.rebase {
                // Track the commit's files again first: `rebase --abort` won't overwrite its now-untracked added files.
                true => Op::new(ABORT, &["reset", "-q", "--hard", &sp.id]).then(&["rebase", "--abort"]).backup(changed_since(&self.dir, &sp.id)),
                // Undoable: it also drops whatever was committed on top since, in the app or not.
                false => Op::new(ABORT, &["reset", "-q", &sp.id, "--"]).undo(Undo::Mixed),
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
        if left.is_empty() {
            return if sp.rebase { self.run(ctx, cont()) } else { self.finish_split() };
        }
        let message = if self.summary.trim().is_empty() { sp.message.clone() } else { format!("{}\n\n{}", self.summary.trim(), self.body.trim()).trim().to_owned() };
        let mut add = vec!["--literal-pathspecs", "add", "-A", "-f", "--"];
        add.extend(&left);
        let mut rest = Op::new(FINISH, &add).then(&["commit", "-q", "-F", "-"]).input(message).undo(Undo::Keep);
        // No "continue anyway": git refuses `rebase --continue` with uncommitted tracked changes.
        if sp.rebase {
            rest = rest.then(&["-c", "core.editor=true", "rebase", "--continue"]);
        }
        let ops = vec![("Commit the rest as the last part".to_owned(), rest)];
        let n = left.len();
        let text = format!("{n} file{} from {} not committed yet: {}.", if n == 1 { " is" } else { "s are" }, &sp.id[..7.min(sp.id.len())], left.join(", "));
        self.choice = Some(Choice { title: "Changes left over".into(), text, ops });
    }

    /// Aborted: back to where the split started, so the steps in between are moot
    /// (the abort's own entry stays).
    pub(super) fn split_aborted(&mut self) {
        let Some(sp) = self.split.take() else { return };
        let steps = self.undo.split_off(sp.undo_len.min(self.undo.len()));
        self.undo.extend(steps.into_iter().filter(|e| matches!(e.kind, UndoKind::Discard(_)) || e.label == ABORT));
    }

    /// The split is done: merge its undo entries into one.
    pub(super) fn finish_split(&mut self) {
        let Some(sp) = self.split.take() else { return };
        (self.summary, self.body) = (String::new(), String::new());
        let steps = self.undo.split_off(sp.undo_len.min(self.undo.len()));
        let refs: Vec<_> = steps.iter().filter_map(|e| if let UndoKind::Refs { before, after, .. } = &e.kind { Some((before, after)) } else { None }).collect();
        if let (Some(first), Some(last)) = (refs.first(), refs.last()) {
            let kind = UndoKind::Refs { before: first.0.clone(), after: last.1.clone(), mode: Undo::Keep, autostash: None, saved: None };
            self.undo.push(UndoEntry::new(format!("Split {}", &sp.id[..7.min(sp.id.len())]), kind));
        }
        // Backed-up discards stay undoable on their own.
        self.undo.extend(steps.into_iter().filter(|e| matches!(e.kind, UndoKind::Discard(_))));
        self.notes.push(Msg { text: "Split done ✔".into(), error: false });
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{commit, identity, scratch, settle, sh};
    use super::super::{Action, View};
    use super::*;

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
}
