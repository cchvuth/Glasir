//! "Clean up branches": local branches that are merged or whose upstream is gone.

use super::data::is_trunk;
use super::RepoTab;
use crate::git::{self, RefKind};

impl RepoTab {
    pub(super) fn open_cleanup(&mut self) {
        let Some(d) = &self.data else { return };
        let def = d.default_branch();
        let base = if d.refs.iter().any(|r| r.name == format!("origin/{def}")) { format!("origin/{def}") } else { def };
        let merged = git::run(&self.dir, &["branch", "--format=%(refname:short)", "--merged", &base]).unwrap_or_default();
        let merged: std::collections::HashSet<&str> = merged.lines().collect();
        let list: Vec<(String, String, bool)> = d
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Local && !r.head && r.worktree.is_none() && !is_trunk(&r.name))
            .filter_map(|r| {
                if merged.contains(r.name.as_str()) {
                    Some((r.name.clone(), format!("merged into {base}"), true))
                } else if r.gone {
                    Some((r.name.clone(), "upstream branch was deleted".into(), true))
                } else {
                    None
                }
            })
            .collect();
        self.cleanup = Some(list);
    }
}
