//! State behind the reflog view (drawn by `crate::ui::reflog`): HEAD's reflog,
//! reloaded after every op so recovered commits stop showing as lost.

use super::Task;
use crate::git::{self, ReflogEntry, Res};
use std::path::Path;

pub struct Reflog {
    pub entries: Option<Res<Vec<ReflogEntry>>>,
    task: Option<Task<Res<Vec<ReflogEntry>>>>,
    pub sel: Option<usize>,
}

impl Reflog {
    pub fn open(ctx: &egui::Context, dir: &Path) -> Self {
        let mut r = Reflog { entries: None, task: None, sel: None };
        r.refresh(ctx, dir);
        r
    }

    pub(super) fn refresh(&mut self, ctx: &egui::Context, dir: &Path) {
        let dir = dir.to_path_buf();
        self.task = Some(Task::spawn(ctx, move || git::reflog(&dir)));
    }

    pub fn poll(&mut self) {
        if let Some(e) = Task::poll(&mut self.task) {
            self.sel = match (self.sel, &self.entries, &e) {
                (Some(i), Some(Ok(old)), Ok(new)) => old.get(i).and_then(|o| reselect(o, i, new)),
                _ => None,
            };
            self.entries = Some(e);
        }
    }
}

/// Where entry `o`, formerly at `i`, is now. New entries arrive on top and the
/// `-n2000` cap or expiry drops old ones, so it only moves down: look from `i`.
fn reselect(o: &ReflogEntry, i: usize, new: &[ReflogEntry]) -> Option<usize> {
    let same = |n: &ReflogEntry| n.id == o.id && n.time == o.time && n.action == o.action && n.detail == o.detail;
    new.iter().skip(i).position(same).map(|p| p + i).or_else(|| new.iter().position(same))
}

#[cfg(test)]
mod tests {
    use crate::git;

    #[test]
    fn selection_follows_the_entry_at_the_cap() {
        let e = |n: i64| git::ReflogEntry { id: n.to_string(), time: n, action: "commit".into(), detail: String::new(), summary: String::new(), lost: false };
        let old: Vec<_> = (0..5).rev().map(e).collect(); // 4 3 2 1 0
        let new: Vec<_> = (1..6).rev().map(e).collect(); // 5 4 3 2 1: same length
        assert_eq!(super::reselect(&old[2], 2, &new), Some(3));
        assert_eq!(super::reselect(&old[4], 4, &new), None);
    }
    use crate::repo::op::{Op, exec};
    use crate::repo::testutil::{commit, identity, scratch, sh};

    #[test]
    fn recovers_commits_lost_to_a_reset() {
        let dir = scratch("reflog");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "base\n");
        commit(&dir, "b.txt", "precious work\n");
        let lost = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        sh(&dir, &["reset", "-q", "--hard", "HEAD~1"]);

        let log = git::reflog(&dir).unwrap();
        assert_eq!((log[0].action.as_str(), log[0].detail.as_str(), log[0].lost), ("reset", "moving to HEAD~1", false));
        let e = log.iter().find(|e| e.id == lost).unwrap();
        assert!(e.lost && e.summary == "precious work" && e.action == "commit");

        // "Create branch here" brings it back without touching the working tree.
        exec(&dir, &Op::new("Create branch", &["branch", "rescued", &lost])).unwrap();
        assert!(!git::reflog(&dir).unwrap().iter().any(|e| e.lost));
        // "Reset main here (keep)" moves the current branch back onto it.
        exec(&dir, &Op::new("Reset", &["reset", "--keep", &lost])).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "precious work\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
