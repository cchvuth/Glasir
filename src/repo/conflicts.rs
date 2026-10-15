//! Conflict prediction against the branch you would integrate with next.

use super::task::Task;
use super::RepoTab;
use crate::git;

impl RepoTab {
    /// Dry-run merge of the branch you'd integrate with next: the default
    /// branch, or your upstream when you're on the default branch.
    pub(super) fn check_conflicts(&mut self, ctx: &egui::Context) {
        let Some(d) = &self.data else { return };
        let Some(branch) = d.status.branch.clone() else {
            (self.conflicts, self.conflict_task, self.conflict_key) = (None, None, None);
            return;
        };
        let def = d.default_branch();
        let remote_def = format!("origin/{def}");
        let has = |n: &str| d.refs.iter().any(|r| r.name == n);
        let target = if branch != def && has(&remote_def) {
            remote_def
        } else if let Some(up) = d.status.upstream.clone().filter(|_| d.status.behind > 0) {
            up
        } else {
            (self.conflicts, self.conflict_task, self.conflict_key) = (None, None, None);
            return;
        };
        // The answer depends only on the two commits: status-only reloads keep it.
        let key = (d.status.head.clone().unwrap_or_default(), d.refs.iter().find(|r| r.name == target).map_or_else(|| target.clone(), |r| r.target.clone()));
        if self.conflict_key.as_ref() == Some(&key) {
            return;
        }
        self.conflict_key = Some(key);
        let dir = self.dir.clone();
        self.conflict_task = Some(Task::spawn(ctx, move || git::predict_conflicts(&dir, &target).map(|f| (target, f))));
    }
}

#[cfg(test)]
mod tests {
    use crate::repo::testutil::{commit, identity, scratch, settle, sh, wait_for};
    use crate::repo::{RepoTab, Task, View};

    fn conflicting(name: &str) -> std::path::PathBuf {
        let dir = scratch(name);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "feature"]);
        commit(&dir, "f", "feature\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "f", "main\n");
        sh(&dir, &["update-ref", "refs/remotes/origin/main", "main"]);
        sh(&dir, &["checkout", "-q", "feature"]);
        dir
    }

    /// The chip follows HEAD: gone once detached, not recomputed by a status-only reload.
    #[test]
    fn prediction_follows_head() {
        let dir = conflicting("predict-detach");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        let mut msgs = vec![];
        wait_for("the prediction", || {
            t.poll(&ctx, &mut msgs);
            t.conflicts.is_some()
        });
        assert_eq!(t.conflicts, Some(("origin/main".into(), vec!["f".into()])));
        let key = t.conflict_key.clone();
        t.apply(&ctx, crate::repo::op("Detach", &["checkout", "-q", "--detach", "main"]));
        settle(&ctx, &mut t);
        assert_eq!((t.conflicts.clone(), t.conflict_key.clone()), (None, None));
        assert!(key.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_running_prediction_does_not_outlive_its_target() {
        let dir = conflicting("predict-no-target");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        let mut msgs = vec![];
        wait_for("the prediction", || {
            t.poll(&ctx, &mut msgs);
            t.conflicts.is_some()
        });
        let (tx, rx) = std::sync::mpsc::channel();
        t.conflict_task = Some(Task(rx));
        t.apply(&ctx, crate::repo::op("Checkout", &["checkout", "-q", "main"]));
        settle(&ctx, &mut t);
        assert_eq!(t.conflicts, None);
        let _ = tx.send(Some(("origin/main".into(), vec!["f".into()])));
        t.poll(&ctx, &mut msgs);
        assert_eq!(t.conflicts, None, "the chip came back on main");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
