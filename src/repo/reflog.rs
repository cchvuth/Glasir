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
            // New entries arrive on top: keep the same entry selected.
            let grew = match (&self.entries, &e) {
                (Some(Ok(old)), Ok(new)) => new.len().saturating_sub(old.len()),
                _ => 0,
            };
            self.sel = self.sel.map(|i| i + grew);
            self.entries = Some(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::git;
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
