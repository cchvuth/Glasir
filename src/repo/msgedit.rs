//! Inline editing of a commit's message in the commit details: the edit state,
//! which commits may be reworded, and keeping the state in step with reloads.

use super::{Action, RepoData, RepoTab, Sel};
use crate::git::RefKind;
use crate::git;
use std::collections::{HashMap, VecDeque};

/// A commit message being edited (summary line, body, and how they were joined).
pub struct MsgEdit {
    pub id: String,
    pub summary: String,
    pub body: String,
    /// Newlines between summary and body, so an unedited message round-trips exactly.
    sep: String,
    original: String,
    /// The branch it's rewritten on (`None`: HEAD's), and the parent steps from its tip (which
    /// parent each), to find the commit again after the rewrite renamed it.
    at: (Option<String>, Vec<usize>),
    /// Save was sent: when the commit disappears, select its rewritten successor.
    saved: bool,
    /// Focus the summary field on the next frame.
    pub focus: bool,
}

impl MsgEdit {
    pub fn new(id: &str, message: &str, at: (Option<String>, Vec<usize>)) -> Self {
        let message = message.trim_end();
        let (summary, rest) = message.split_once('\n').unwrap_or((message, ""));
        let body = rest.trim_start_matches('\n');
        let sep = if body.is_empty() { "\n\n".to_owned() } else { "\n".repeat(1 + rest.len() - body.len()) };
        MsgEdit { id: id.into(), summary: summary.into(), body: body.into(), sep, original: message.into(), at, saved: false, focus: true }
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
    /// The branch to rewrite `id` on (`None`: HEAD's, else a local branch that has it) and the
    /// parent steps from its tip to `id`, or why its message can't be edited here.
    pub fn reword_path(&self, id: &str) -> Result<(Option<String>, Vec<usize>), &'static str> {
        if self.state != git::State::Clean {
            return Err("Finish the operation in progress (merge, rebase, cherry-pick...) first.");
        }
        let head = self.status.head.as_deref().filter(|_| self.status.branch.is_some());
        if let Some(path) = head.and_then(|h| self.path_from(h, id)) {
            return Ok((None, path));
        }
        self.refs
            .iter()
            .filter(|r| r.kind == RefKind::Local && !r.head)
            .find_map(|r| self.path_from(&r.target, id).map(|p| (Some(r.name.clone()), p)))
            .ok_or("This commit isn't on a local branch.")
    }

    fn path_from(&self, tip: &str, id: &str) -> Option<Vec<usize>> {
        // Rows are children before parents: the way down from the tip never goes below `id`'s row.
        let row = *self.row_of.get(id)?;
        let mut from: HashMap<&str, (&str, usize)> = HashMap::new();
        let mut queue = VecDeque::from([tip]);
        while let Some(cur) = queue.pop_front() {
            if cur == id {
                let (mut path, mut at) = (vec![], cur);
                while let Some(&(child, i)) = from.get(at) {
                    path.push(i);
                    at = child;
                }
                path.reverse();
                return Some(path);
            }
            let Some(c) = self.row_of.get(cur).filter(|&&r| r <= row).and_then(|&r| self.commit(r)) else { continue };
            for (i, p) in c.parents.iter().enumerate() {
                if p != tip && !from.contains_key(p.as_str()) {
                    from.insert(p, (cur, i));
                    queue.push_back(p);
                }
            }
        }
        None
    }

    fn follow(&self, (branch, path): &(Option<String>, Vec<usize>)) -> Option<String> {
        let mut cur = match branch {
            Some(b) => self.local(b)?.target.clone(),
            None => self.status.head.clone()?,
        };
        for &i in path {
            cur = self.row_of.get(&cur).and_then(|&r| self.commit(r))?.parents.get(i)?.clone();
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
        // Its place from HEAD holds its rewritten self once saved (the old one may still show, from another ref).
        let successor = self.data.as_ref().filter(|_| e.saved).and_then(|d| d.follow(&e.at)).filter(|s| *s != e.id);
        if successor.is_none() && matches!(&self.sel, Some(Sel::Commit(id)) if *id == e.id) && loaded {
            return;
        }
        if successor.is_none() && e.saved && !loaded && self.load.is_some() {
            return; // the reload that shows the rewritten commit is on its way
        }
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
            let e = MsgEdit::new("x", m, (None, vec![]));
            assert_eq!(e.message(), m.trim_end());
            assert!(!e.changed());
        }
        let mut e = MsgEdit::new("x", "s", (None, vec![]));
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
    fn reword_on_a_branch_not_checked_out() {
        let dir = scratch("inline-reword-other");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "one");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "b", "two");
        commit(&dir, "c", "three");
        sh(&dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("a"), "dirty").unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let (head, feat) = (sh(&dir, &["rev-parse", "HEAD"]), sh(&dir, &["rev-parse", "feat"]));
        let two = sh(&dir, &["rev-parse", "feat~1"]).trim().to_owned();
        assert_eq!(t.data.as_ref().unwrap().reword_path(&two), Ok((Some("feat".into()), vec![0])));
        t.select(&ctx, Sel::Commit(two.clone()));
        t.edit_msg = Some(MsgEdit::new(&two, "two", (Some("feat".into()), vec![0])));
        t.apply(&ctx, Action::Reword { commit: two.clone(), message: "two, better".into() });
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["log", "--format=%s", "feat"]).lines().collect::<Vec<_>>(), ["three", "two, better", "one"]);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), head);
        assert_eq!(sh(&dir, &["symbolic-ref", "--short", "HEAD"]).trim(), "main");
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "dirty");
        assert_eq!(t.sel, Some(Sel::Commit(sh(&dir, &["rev-parse", "feat~1"]).trim().into())));
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["rev-parse", "feat"]), feat);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        assert_eq!([d.reword_path(&ids[0]), d.reword_path(&ids[1]), d.reword_path(&ids[2])], [Ok((None, vec![])), Ok((None, vec![0])), Ok((None, vec![0, 0]))]);
        // Older commit, with uncommitted work in the way.
        std::fs::write(dir.join("a"), "dirty").unwrap();
        let msg = "two, better\n\nbody of two\n\nCo-authored-by: X <x@x>";
        t.select(&ctx, Sel::Commit(ids[1].clone()));
        t.edit_msg = Some(MsgEdit::new(&ids[1], "two", (None, vec![0])));
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
        t.edit_msg = Some(MsgEdit::new(&ids[0], "x", (None, vec![])));
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
    fn pushed_commits_ask_first() {
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
        assert!(reword(&dir, &gd, &ids[0], true, None, "3".into()).unwrap().confirm.is_none());
        let confirm = reword(&dir, &gd, &ids[1], false, None, "2".into()).unwrap().confirm.unwrap();
        assert!(confirm.contains("1 commit already pushed to origin/main"), "{confirm}");
        let confirm = reword(&dir, &gd, &ids[2], false, None, "1".into()).unwrap().confirm.unwrap(); // the root
        assert!(confirm.contains("2 commits already pushed"), "{confirm}");
        // With a merge on top, only the rewritten commits count.
        sh(&dir, &["checkout", "-q", "-b", "side", &ids[1]]);
        commit(&dir, "s", "side");
        sh(&dir, &["checkout", "-q", "main"]);
        sh(&dir, &["merge", "-q", "--no-ff", "-m", "merge", "side"]);
        assert!(reword(&dir, &gd, &ids[0], false, None, "3".into()).unwrap().confirm.is_none());
        let confirm = reword(&dir, &gd, &ids[1], false, None, "2".into()).unwrap().confirm.unwrap();
        assert!(confirm.contains("1 commit already pushed"), "{confirm}");
        sh(&dir, &["checkout", "-q", "-b", "other", &ids[1]]);
        commit(&dir, "o", "other");
        sh(&dir, &["checkout", "-q", "main"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let d = t.data.as_ref().unwrap();
        let rev = |r: &str| sh(&dir, &["rev-parse", r]).trim().to_owned();
        assert_eq!([d.reword_path(&rev("HEAD")), d.reword_path(&ids[0]), d.reword_path(&rev("side"))], [Ok((None, vec![])), Ok((None, vec![0])), Ok((None, vec![1]))]);
        assert_eq!(d.reword_path(&rev("other")), Ok((Some("other".into()), vec![])));
        assert!(reword(&dir, &gd, &rev("other"), false, None, "x".into()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&remote);
    }

    /// Below a merge, the merge itself, a commit merged in, and the root: each keeps every tree,
    /// author and merge, selects its rewritten self, leaves uncommitted work alone; ⌘Z undoes it.
    #[test]
    fn reword_through_merges_with_undo() {
        let dir = scratch("inline-reword-merges");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "one");
        commit(&dir, "b", "two");
        sh(&dir, &["checkout", "-q", "-b", "side"]);
        commit(&dir, "s", "side");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "c", "three");
        sh(&dir, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
        commit(&dir, "d", "four");
        std::fs::write(dir.join("a"), "dirty").unwrap();
        std::fs::write(dir.join("n"), "new").unwrap();
        sh(&dir, &["add", "n"]);
        let revs = ["HEAD", "HEAD~1", "HEAD~1^2", "HEAD~2", "HEAD~3", "HEAD~4"];
        let show = |fmt: &str| -> Vec<String> { revs.iter().map(|r| sh(&dir, &["log", "-1", fmt, r])).collect() };
        let (trees, authors, head0, status) = (show("--format=%T %P"), show("--format=%an %ae %ad"), sh(&dir, &["rev-parse", "HEAD"]), sh(&dir, &["status", "--porcelain"]));
        let parents = |s: &[String]| -> Vec<usize> { s.iter().map(|l| l.split_whitespace().count() - 1).collect() };
        assert_eq!(parents(&trees), [1, 2, 1, 1, 1, 0]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        for (rev, path) in [("HEAD~3", vec![0, 0, 0]), ("HEAD~1", vec![0]), ("HEAD~1^2", vec![0, 1]), ("HEAD~4", vec![0, 0, 0, 0])] {
            let id = sh(&dir, &["rev-parse", rev]).trim().to_owned();
            assert_eq!(t.data.as_ref().unwrap().reword_path(&id), Ok((None, path.clone())), "{rev}");
            let msg = format!("{rev} reworded\n\nwith a body");
            t.select(&ctx, Sel::Commit(id.clone()));
            t.edit_msg = Some(MsgEdit::new(&id, "x", (None, path)));
            t.apply(&ctx, Action::Reword { commit: id, message: msg.clone() });
            settle(&ctx, &mut t);
            assert_eq!(sh(&dir, &["log", "-1", "--format=%B", rev]).trim_end(), msg);
            assert!(t.edit_msg.is_none());
            assert_eq!(t.sel, Some(Sel::Commit(sh(&dir, &["rev-parse", rev]).trim().to_owned())), "{rev}");
            assert_eq!(parents(&show("--format=%T %P")), parents(&trees));
            assert_eq!(show("--format=%T"), trees.iter().map(|l| l.split_whitespace().next().unwrap().to_owned() + "\n").collect::<Vec<_>>());
            assert_eq!(show("--format=%an %ae %ad"), authors);
            assert_eq!(sh(&dir, &["status", "--porcelain"]), status);
        }
        assert_ne!(sh(&dir, &["rev-parse", "HEAD"]), head0);
        assert_eq!(sh(&dir, &["symbolic-ref", "HEAD"]).trim(), "refs/heads/main");
        for _ in 0..4 {
            t.apply(&ctx, Action::Undo);
            settle(&ctx, &mut t);
        }
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), head0);
        assert_eq!(show("--format=%T %P"), trees);
        assert_eq!(sh(&dir, &["status", "--porcelain"]), status);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
