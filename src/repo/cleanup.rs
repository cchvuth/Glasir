//! "Clean up branches": local branches that are merged or whose upstream is gone.

use super::data::is_trunk;
use super::RepoTab;
use crate::git::{self, RefKind};

impl RepoTab {
    pub(super) fn open_cleanup(&mut self) {
        let Some(d) = &self.data else { return };
        let def = d.default_branch();
        let base = if d.refs.iter().any(|r| r.name == format!("origin/{def}")) { format!("origin/{def}") } else { def.clone() };
        let merged = git::run(&self.dir, &["branch", "--format=%(refname:short)", "--merged", &base]).unwrap_or_default();
        let merged: std::collections::HashSet<&str> = merged.lines().collect();
        // A branch being rebased or bisected has no HEAD marker, but git refuses to delete it.
        let busy: Vec<String> = ["rebase-merge/head-name", "rebase-apply/head-name", "BISECT_START"]
            .iter()
            .filter_map(|f| std::fs::read_to_string(d.git_dir.join(f)).ok())
            .map(|s| s.trim().trim_start_matches("refs/heads/").to_owned())
            .collect();
        let list: Vec<(String, String, bool)> = d
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Local && !r.head && r.worktree.is_none() && !is_trunk(&r.name) && r.name != def && !busy.contains(&r.name))
            .filter_map(|r| {
                if merged.contains(r.name.as_str()) {
                    Some((r.name.clone(), format!("merged into {base}"), true))
                } else if r.gone {
                    // Squash-merged branches count too: unticked but labelled, the safe default.
                    let n = git::run(&self.dir, &["rev-list", "--count", &r.full, "--not", "--remotes", &base])
                        .ok()
                        .and_then(|s| s.trim().parse::<usize>().ok())
                        .unwrap_or(1);
                    let why = match n {
                        0 => "upstream branch was deleted".into(),
                        _ => format!("upstream deleted · {n} commit{} not on any remote", if n == 1 { "" } else { "s" }),
                    };
                    Some((r.name.clone(), why, n == 0))
                } else {
                    None
                }
            })
            .collect();
        self.cleanup = Some(list);
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{commit, identity, scratch, settle, sh};
    use super::super::View;
    use super::*;

    /// The remote's default branch is never offered, whatever its name.
    #[test]
    fn keeps_the_default_branch() {
        let root = scratch("cleanup");
        sh(&root, &["init", "-q", "--bare", "-b", "stable", "remote.git"]);
        sh(&root, &["clone", "-q", "remote.git", "a"]);
        let a = root.join("a");
        identity(&a);
        commit(&a, "f.txt", "one\n");
        sh(&a, &["push", "-q", "-u", "origin", "stable"]);
        sh(&a, &["remote", "set-head", "origin", "stable"]);
        sh(&a, &["branch", "done"]);
        sh(&a, &["switch", "-q", "-c", "feature"]);
        commit(&a, "g.txt", "two\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, a.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.open_cleanup();
        assert_eq!(t.cleanup.unwrap(), [("done".to_owned(), "merged into origin/stable".to_owned(), true)]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A branch whose upstream was deleted is only pre-selected when no commit of it
    /// would be lost.
    #[test]
    fn gone_branch_with_unpushed_work_is_not_preselected() {
        let root = scratch("cleanup-gone");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        sh(&root, &["clone", "-q", "remote.git", "a"]);
        let a = root.join("a");
        identity(&a);
        commit(&a, "f.txt", "one\n");
        sh(&a, &["push", "-q", "-u", "origin", "main"]);
        for b in ["feature", "pushed"] {
            sh(&a, &["switch", "-q", "-c", b]);
            commit(&a, &format!("{b}.txt"), "pushed\n");
            // Also kept under another name, so its pushed commit stays on a remote.
            sh(&a, &["push", "-q", "-u", "origin", b]);
            sh(&a, &["push", "-q", "origin", &format!("{b}:{b}-copy")]);
        }
        sh(&a, &["switch", "-q", "feature"]);
        commit(&a, "feature.txt", "unpushed\n");
        sh(&a, &["switch", "-q", "main"]);
        sh(&a, &["push", "-q", "origin", "--delete", "feature", "pushed"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, a.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.open_cleanup();
        let list = t.cleanup.unwrap();
        assert_eq!(
            list,
            [
                ("feature".to_owned(), "upstream deleted · 1 commit not on any remote".to_owned(), false),
                ("pushed".to_owned(), "upstream branch was deleted".to_owned(), true),
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
