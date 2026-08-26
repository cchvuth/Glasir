//! Guided commit split: the commit is uncommitted (`rebase::split`), the user
//! stages and commits the parts, then Continue replays the rest of the branch.
//! The whole split becomes one undo step.

use super::undo::{UndoEntry, UndoKind, keep_backups};
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
    pub(super) undo_len: usize,
    /// `symbolic-ref HEAD` at the start (empty when detached): Abort resets only this branch.
    branch: String,
    /// Continue stopped on a conflict in a later commit: no banner, the split ends with the rebase.
    pub replaying: bool,
}

/// Label of the ops that end a split (matched in `poll_op`).
pub(super) const FINISH: &str = "Finish split";
pub(super) const ABORT: &str = "Abort split";

/// What `reset --hard <rev>` and then `rebase --abort` lose: paths whose worktree bytes differ
/// from `rev`, and ignored files in either one's way. Not the split commit's own content, which
/// the abort brings back anyway.
fn changed_since(dir: &Path, rev: &str) -> Backup {
    let z = |args: &[&str]| git::run(dir, args).map(|o| o.split('\0').filter(|p| !p.is_empty()).map(String::from).collect::<Vec<_>>());
    let orig = super::undo::rebase_orig(dir).map(|o| o.trim().to_owned());
    let revs: Vec<&str> = [Some(rev), orig.as_deref()].into_iter().flatten().collect();
    let (Ok(mut paths), Ok(untracked), Ok(ignored)) = (z(&["diff", "--name-only", "-z", "--no-renames", rev]), z(&["ls-files", "-z", "--others", "--exclude-standard"]), super::undo::in_the_way(dir, &revs)) else {
        return Backup::All;
    };
    paths.extend(untracked.into_iter().chain(ignored));
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

/// Untracked files, ignored ones too, in the way of the commits after `id` that Continue replays.
fn replayed_way(dir: &Path, id: &str) -> Backup {
    let Some(orig) = super::undo::rebase_orig(dir) else { return Backup::All };
    // ponytail: one `ls-tree` per later commit, run only when untracked files exist; fine for a split's few.
    let later = git::run(dir, &["rev-list", &format!("{id}..{}", orig.trim())]).unwrap_or_default();
    super::undo::in_the_way(dir, &later.lines().collect::<Vec<_>>()).map_or(Backup::All, Backup::Paths)
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
                true => Op::new(ABORT, &["reset", "-q", "--hard", &sp.id]).then(&["rebase", "--abort"]).backup(changed_since(&self.dir, &sp.id)).undo(Undo::Keep),
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
        // Replaying the later commits overwrites ignored files where one of them puts a file.
        let way = sp.rebase.then(|| replayed_way(&self.dir, &sp.id));
        if left.is_empty() {
            return match way {
                Some(b) => self.run(ctx, Op::new(FINISH, &["-c", "core.editor=true", "rebase", "--continue"]).backup(b).undo(Undo::Keep)),
                None => self.finish_split(),
            };
        }
        let message = if self.summary.trim().is_empty() { sp.message.clone() } else { format!("{}\n\n{}", self.summary.trim(), self.body.trim()).trim().to_owned() };
        let mut add = vec!["--literal-pathspecs", "add", "-A", "-f", "--"];
        add.extend(&left);
        let mut rest = Op::new(FINISH, &add).then(&["commit", "-q", "-F", "-"]).input(message).undo(Undo::Keep);
        // No "continue anyway": git refuses `rebase --continue` with uncommitted tracked changes.
        if let Some(b) = way {
            rest = rest.then(&["-c", "core.editor=true", "rebase", "--continue"]).backup(b);
        }
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

    /// The split's undo entries as one step: its refs from the split's start (an abort: from the
    /// last change made outside since, which its ⌘Z must bring back) to now, and every backup.
    fn fold_split(&mut self, sp: &Split, abort: bool) {
        let steps = self.undo.split_off(sp.undo_len.min(self.undo.len()));
        let refs: Vec<_> = steps.iter().filter_map(|e| if let UndoKind::Refs { before, after, mode, .. } = &e.kind { Some((before.clone(), after.clone(), *mode)) } else { None }).collect();
        // Every backup comes back in the split's step, newest first after its refs (like a hard
        // reset's): a discard on top would be undone first and block the `reset --keep` back,
        // and one below would find its files moved by it.
        let saved: Vec<_> = keep_backups(steps).filter_map(|e| if let UndoKind::Discard(s) = e.kind { Some(s) } else { None }).collect();
        let start = if abort { refs.windows(2).rposition(|w| w[0].1 != w[1].0).map_or(0, |i| i + 1) } else { 0 };
        let label = if abort { ABORT.to_owned() } else { format!("Split {}", &sp.id[..7.min(sp.id.len())]) };
        match refs.get(start).zip(refs.last()) {
            Some((first, last)) if first.0 != last.1 || !saved.is_empty() => {
                // A single step keeps its own mode (a HEAD split's abort is a mixed reset).
                let mode = if start + 1 == refs.len() { last.2 } else { Undo::Keep };
                let kind = UndoKind::Refs { before: first.0.clone(), after: last.1.clone(), mode, autostash: None, saved, on_remote: 0 };
                self.undo.push(UndoEntry::new(label, kind));
            }
            Some(_) => {}
            None => self.undo.extend(saved.into_iter().map(|s| UndoEntry::new(label.clone(), UndoKind::Discard(s)))),
        }
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

    /// Every backup made during a split comes back with its ⌘Z, newest first (a newer copy of
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
            assert_eq!((read(&dir, "a"), read(&dir, "b"), read(&dir, "c")), ("2".into(), "FIVE-unique".into(), "2".into()), "{msgs:?}");
            if hard {
                assert!(sh(&dir, &["stash", "list"]).lines().count() == 1, "{msgs:?}");
                assert_eq!(sh(&dir, &["show", "stash@{0}:b"]), "SIX");
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
            let stashed = git::run(&dir, &["show", &format!("stash@{{0}}:{p}")]).is_ok_and(|s| s == "PRECIOUS");
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
        assert_eq!(sh(&dir, &["show", "stash@{0}:d/f"]), "DF");
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
    /// rebase is aborted: ⌘Z brings the discarded file back, then the steps before the split.
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
        assert_eq!(std::fs::read_to_string(dir.join("b")).unwrap(), "FIVE", "{msgs:?}");
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

    /// Some stash entry holds `want` at `p` (in its files or, `^2`, its index).
    fn stashed(dir: &Path, p: &str, want: &str) -> bool {
        sh(dir, &["stash", "list", "--format=%H"]).lines().any(|h| [format!("{h}:{p}"), format!("{h}^2:{p}")].iter().any(|r| git::run(dir, &["show", r]).is_ok_and(|s| s == want)))
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

    /// r2-1: a discarded file `d` where the split's tip has a folder `d` (and the reverse) goes to
    /// the stash list; ⌘Z never sticks on it.
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
            assert!(stashed(&dir, p, "PRECIOUS"), "{msgs:?}");
            assert_eq!(msgs.iter().filter(|m| m.contains("stash")).count(), 1, "{msgs:?}");
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
    /// commit that tracks an ignored file made since: that file goes to the stash list, said once.
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
        assert!(stashed(&dir, ".env", "PRECIOUS"), "{msgs:?}");
        assert_eq!(msgs.iter().filter(|m| m.contains("stash")).count(), 1, "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);

        let (dir, ctx, mut t, tip) = ignored_repo("split-undo-start-ignored");
        step(&ctx, &mut t, Action::Split(sh(&dir, &["rev-parse", "HEAD~1"]).trim().into()));
        // The commit's changes dropped outside, so the checkout back goes through.
        sh(&dir, &["checkout", "--", "a"]);
        std::fs::remove_file(dir.join("x")).unwrap();
        std::fs::write(dir.join(".env"), "PRECIOUS").unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip, "{msgs:?}");
        assert!(t.split.is_none());
        assert_eq!(t.data.as_ref().unwrap().state, git::State::Clean);
        assert!(stashed(&dir, ".env", "PRECIOUS"), "{msgs:?}");
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
    #[test]
    fn a_name_git_cannot_store_is_not_discarded() {
        let (dir, ctx, mut t, _) = split_repo("split-unstorable");
        std::fs::write(dir.join("git~1"), "PRECIOUS").unwrap();
        let msgs = step(&ctx, &mut t, clean("git~1"));
        assert_eq!(std::fs::read_to_string(dir.join("git~1")).ok().as_deref(), Some("PRECIOUS"), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("Nothing was discarded")), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
