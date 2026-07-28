//! Inline editing of a commit's message in the commit details: the edit state,
//! which commits may be reworded, and keeping the state in step with reloads.

use super::{Action, RepoData, RepoTab, Sel};
use crate::git;

/// A commit message being edited (summary line, body, and how they were joined).
pub struct MsgEdit {
    pub id: String,
    pub summary: String,
    pub body: String,
    /// Newlines between summary and body, so an unedited message round-trips exactly.
    sep: String,
    original: String,
    /// First-parent steps from HEAD, to find the commit again after the rebase renamed it.
    depth: usize,
    /// Save was sent: when the commit disappears, select its rewritten successor.
    saved: bool,
    /// Focus the summary field on the next frame.
    pub focus: bool,
}

impl MsgEdit {
    pub fn new(id: &str, message: &str, depth: usize) -> Self {
        let message = message.trim_end();
        let (summary, rest) = message.split_once('\n').unwrap_or((message, ""));
        let body = rest.trim_start_matches('\n');
        let sep = if body.is_empty() { "\n\n".to_owned() } else { "\n".repeat(1 + rest.len() - body.len()) };
        MsgEdit { id: id.into(), summary: summary.into(), body: body.into(), sep, original: message.into(), depth, saved: false, focus: true }
    }

    /// The edited message; trailing whitespace is git's to clean up.
    pub fn message(&self) -> String {
        let (s, b) = (self.summary.trim_end(), self.body.trim_end());
        if b.is_empty() { s.to_owned() } else { format!("{s}{}{b}", self.sep) }
    }

    pub fn changed(&self) -> bool {
        self.message() != self.original
    }
}

impl RepoData {
    /// How many first-parent steps `id` is below HEAD, or why its message can't be edited here:
    /// only non-merge commits on the current branch's first-parent history, with no merge above
    /// them (the rebase engine refuses merges), and not during another operation.
    pub fn reword_depth(&self, id: &str) -> Result<usize, &'static str> {
        if self.state != git::State::Clean {
            return Err("Finish the operation in progress (merge, rebase, cherry-pick...) first.");
        }
        let Some(head) = self.status.head.as_deref().filter(|_| self.status.branch.is_some()) else {
            return Err("Check out a branch first: this isn't on a branch (HEAD is detached).");
        };
        let c = self.row_of.get(id).and_then(|&r| self.commit(r)).ok_or("This commit isn't on the current branch.")?;
        if c.parents.len() > 1 {
            return Err("Merge commits can't be edited here.");
        }
        let (mut cur, mut depth) = (head, 0);
        loop {
            let at = self.row_of.get(cur).and_then(|&r| self.commit(r)).ok_or("This commit isn't on the current branch.")?;
            if cur == id {
                return if at.parents.is_empty() && depth > 0 { Err("The root commit can't be edited here; use the interactive rebase editor.") } else { Ok(depth) };
            }
            if at.parents.len() > 1 {
                return Err("A merge commit sits on top of this one, which can't be rewritten here.");
            }
            cur = at.parents.first().ok_or("This commit isn't on the current branch.")?;
            depth += 1;
        }
    }

    fn first_parent_nth(&self, n: usize) -> Option<String> {
        let mut cur = self.status.head.clone()?;
        for _ in 0..n {
            cur = self.row_of.get(&cur).and_then(|&r| self.commit(r))?.parents.first()?.clone();
        }
        Some(cur)
    }
}

impl RepoTab {
    /// Drops the edit when its commit is no longer selected or loaded; after a saved reword
    /// selects the rewritten commit instead.
    pub(super) fn sync_msg_edit(&mut self, ctx: &egui::Context) {
        let Some(e) = &self.edit_msg else { return };
        let loaded = self.data.as_ref().is_some_and(|d| d.row_of.contains_key(&e.id));
        if matches!(&self.sel, Some(Sel::Commit(id)) if *id == e.id) && loaded {
            return;
        }
        if e.saved && !loaded && self.load.is_some() {
            return; // the reload that shows the rewritten commit is on its way
        }
        let successor = (e.saved && !loaded).then(|| self.data.as_ref().and_then(|d| d.first_parent_nth(e.depth))).flatten();
        self.edit_msg = None;
        if let Some(id) = successor {
            self.apply(ctx, Action::Jump(id));
        }
    }

    /// Save the edited message (`Action::Reword`): a no-op close when nothing changed.
    pub(super) fn mark_saved(&mut self) {
        if let Some(e) = &mut self.edit_msg {
            e.saved = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{commit, identity, scratch, settle, sh};
    use super::super::{Action, RepoTab, Sel, View};
    use super::*;

    #[test]
    fn message_round_trips_and_edits() {
        for m in ["just a summary", "s\n\nbody\n\nSigned-off-by: A <a@a>", "s\nno blank line", "s\n\n\n\nwide gap\n"] {
            let e = MsgEdit::new("x", m, 0);
            assert_eq!(e.message(), m.trim_end());
            assert!(!e.changed());
        }
        let mut e = MsgEdit::new("x", "s", 0);
        e.body = "now a body\n\n".into();
        assert_eq!(e.message(), "s\n\nnow a body");
        e.body.clear();
        e.summary = "s2  ".into();
        assert_eq!(e.message(), "s2");
        assert!(e.changed());
    }

    fn trees(dir: &std::path::Path) -> Vec<String> {
        sh(dir, &["log", "--format=%T"]).lines().map(String::from).collect()
    }

    /// Reword HEAD and an older commit through the action: new message, same trees, one ⌘Z step each,
    /// uncommitted work kept, and a pushed commit asks before rewriting.
    #[test]
    fn reword_head_and_older_with_undo() {
        let dir = scratch("inline-reword");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "one");
        std::fs::write(dir.join("b"), "two").unwrap();
        sh(&dir, &["add", "b"]);
        sh(&dir, &["commit", "-q", "-m", "two", "-m", "body of two\n\nCo-authored-by: X <x@x>"]);
        commit(&dir, "c", "three");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let ids: Vec<String> = sh(&dir, &["rev-list", "HEAD"]).lines().map(String::from).collect(); // three, two, one
        let (before, trees0) = (ids.clone(), trees(&dir));
        let d = t.data.as_ref().unwrap();
        assert_eq!([d.reword_depth(&ids[0]), d.reword_depth(&ids[1])], [Ok(0), Ok(1)]);
        assert!(d.reword_depth(&ids[2]).is_err()); // root, not HEAD
        // Older commit, with uncommitted work in the way.
        std::fs::write(dir.join("a"), "dirty").unwrap();
        let msg = "two, better\n\nbody of two\n\nCo-authored-by: X <x@x>";
        t.select(&ctx, Sel::Commit(ids[1].clone()));
        t.edit_msg = Some(MsgEdit::new(&ids[1], "two", 1));
        t.apply(&ctx, Action::Reword { commit: ids[1].clone(), message: msg.into() });
        settle(&ctx, &mut t);
        // The edit is done and the rewritten commit is selected in place of the old one.
        let new = sh(&dir, &["rev-parse", "HEAD~1"]).trim().to_owned();
        assert!(t.edit_msg.is_none());
        assert_eq!(t.sel, Some(Sel::Commit(new)));
        assert_eq!(sh(&dir, &["log", "-1", "--format=%B", "HEAD~1"]).trim_end(), msg);
        assert_eq!(sh(&dir, &["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["three", "two, better", "one"]);
        assert_eq!(trees(&dir), trees0);
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "dirty");
        t.apply(&ctx, Action::Reword { commit: sh(&dir, &["rev-parse", "HEAD"]).trim().into(), message: "three!".into() });
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["log", "-1", "--format=%B"]).trim_end(), "three!");
        assert_eq!(trees(&dir), trees0);
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "dirty");
        // Selecting another commit drops an open edit.
        t.edit_msg = Some(MsgEdit::new(&ids[0], "x", 0));
        t.select(&ctx, Sel::Wip);
        settle(&ctx, &mut t);
        assert!(t.edit_msg.is_none());
        // One ⌘Z per reword, back to the original commits.
        for _ in 0..2 {
            t.apply(&ctx, Action::Undo);
            settle(&ctx, &mut t);
        }
        assert_eq!(sh(&dir, &["rev-list", "HEAD"]).lines().map(String::from).collect::<Vec<_>>(), before);
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "dirty");
        assert_eq!(sh(&dir, &["status", "--porcelain"]).trim(), "M a");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pushed_commits_ask_first_and_merges_are_refused() {
        let dir = scratch("inline-reword-pushed");
        let remote = scratch("inline-reword-remote");
        sh(&remote, &["init", "-q", "--bare", "-b", "main"]);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "one");
        commit(&dir, "b", "two");
        sh(&dir, &["remote", "add", "origin", remote.to_str().unwrap()]);
        sh(&dir, &["push", "-q", "-u", "origin", "main"]);
        commit(&dir, "c", "three"); // unpushed
        let gd = git::git_dir(&dir).unwrap();
        let ids: Vec<String> = sh(&dir, &["rev-list", "HEAD"]).lines().map(String::from).collect();
        use super::super::rebase::reword;
        assert!(reword(&dir, &gd, &ids[0], true, "3".into()).unwrap().confirm.is_none());
        let confirm = reword(&dir, &gd, &ids[1], false, "2".into()).unwrap().confirm.unwrap();
        assert!(confirm.contains("1 commit already pushed to origin/main"), "{confirm}");
        assert!(reword(&dir, &gd, &ids[2], false, "1".into()).is_err()); // the root can't be rebased onto
        // A merge on top blocks editing what's below it.
        sh(&dir, &["checkout", "-q", "-b", "side", &ids[1]]);
        commit(&dir, "s", "side");
        sh(&dir, &["checkout", "-q", "main"]);
        sh(&dir, &["merge", "-q", "--no-ff", "-m", "merge", "side"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let d = t.data.as_ref().unwrap();
        let head = d.status.head.clone().unwrap();
        assert!(d.reword_depth(&head).unwrap_err().contains("Merge"));
        assert!(d.reword_depth(&ids[0]).unwrap_err().contains("merge commit sits"));
        let side = sh(&dir, &["rev-parse", "side"]).trim().to_owned();
        assert!(d.reword_depth(&side).is_err()); // not on main's first-parent line
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&remote);
    }
}
