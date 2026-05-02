//! Conflict prediction against the branch you would integrate with next.

use super::task::Task;
use super::RepoTab;
use crate::git;

impl RepoTab {
    /// Dry-run merge of the branch you'd integrate with next: the default
    /// branch, or your upstream when you're on the default branch.
    pub(super) fn check_conflicts(&mut self, ctx: &egui::Context) {
        let Some(d) = &self.data else { return };
        let Some(branch) = d.status.branch.clone() else { return };
        let def = d.default_branch();
        let remote_def = format!("origin/{def}");
        let has = |n: &str| d.refs.iter().any(|r| r.name == n);
        let target = if branch != def && has(&remote_def) {
            remote_def
        } else if let Some(up) = d.status.upstream.clone().filter(|_| d.status.behind > 0) {
            up
        } else {
            self.conflicts = None;
            return;
        };
        let dir = self.dir.clone();
        self.conflict_task = Some(Task::spawn(ctx, move || git::predict_conflicts(&dir, &target).map(|f| (target, f))));
    }
}
