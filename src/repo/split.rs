//! Guided commit split: the commit is uncommitted (`rebase::split`), the user
//! stages and commits the parts, then Continue replays the rest of the branch.
//! The whole split becomes one undo step.

use super::undo::{UndoEntry, UndoKind};
use super::{Choice, Msg, Op, RepoTab, Undo};
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
    undo_len: usize,
}

/// Label of the ops that end a split (matched in `poll_op`).
pub(super) const FINISH: &str = "Finish split";

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
                self.split = Some(Split { id: commit, rebase: !is_head, files, message, undo_len: self.undo.len() });
                self.run(ctx, op);
            }
            Err(e) => self.notes.push(Msg { text: e, error: true }),
        }
    }

    /// Continue: replay the rest when nothing of the commit is left uncommitted,
    /// otherwise offer to commit the rest (or Cancel to stash / discard it first). Abort: back to the original commit.
    pub(super) fn end_split(&mut self, ctx: &egui::Context, abort: bool) {
        let (Some(sp), Some(d)) = (&self.split, &self.data) else { return };
        let cont = || Op::new(FINISH, &["-c", "core.editor=true", "rebase", "--continue"]).undo(Undo::Keep);
        if abort {
            let op = if sp.rebase { Op::new("Abort split", &["rebase", "--abort"]) } else { Op::new("Abort split", &["reset", "-q", &sp.id, "--"]) };
            // Back to where the split started: the steps in between are moot.
            self.undo.truncate(sp.undo_len);
            self.split = None;
            return self.run(ctx, op);
        }
        let s = &d.status;
        let mut left: Vec<&str> = s.staged.iter().chain(&s.unstaged).map(|c| c.path.as_str()).filter(|p| sp.files.iter().any(|f| f == p)).collect();
        left.sort_unstable();
        left.dedup();
        if left.is_empty() {
            return if sp.rebase { self.run(ctx, cont()) } else { self.finish_split() };
        }
        let message = if self.summary.trim().is_empty() { sp.message.clone() } else { format!("{}\n\n{}", self.summary.trim(), self.body.trim()).trim().to_owned() };
        let mut add = vec!["--literal-pathspecs", "add", "-A", "--"];
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

    /// The split is done: merge its undo entries into one.
    pub(super) fn finish_split(&mut self) {
        let Some(sp) = self.split.take() else { return };
        (self.summary, self.body) = (String::new(), String::new());
        let steps = self.undo.split_off(sp.undo_len.min(self.undo.len()));
        let refs: Vec<_> = steps.iter().filter_map(|e| if let UndoKind::Refs { before, after, .. } = &e.kind { Some((before, after)) } else { None }).collect();
        if let (Some(first), Some(last)) = (refs.first(), refs.last()) {
            let kind = UndoKind::Refs { before: first.0.clone(), after: last.1.clone(), mode: Undo::Keep, autostash: None };
            self.undo.push(UndoEntry { label: format!("Split {}", &sp.id[..7.min(sp.id.len())]), kind });
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
}
