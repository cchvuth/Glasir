//! Undo snapshots: the local branches, tags and HEAD, and restoring them.

use super::{Res, run};
use std::path::Path;

/// Local branches, tags and HEAD: what undo/redo restores.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// `refs/heads/x`, or a commit id when detached.
    pub head: String,
    pub refs: std::collections::BTreeMap<String, String>,
}

pub fn snapshot(dir: &Path) -> Res<Snapshot> {
    let head = match run(dir, &["symbolic-ref", "-q", "HEAD"]) {
        Ok(h) => h,
        Err(_) => run(dir, &["rev-parse", "HEAD"])?,
    };
    let refs = run(dir, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads", "refs/tags"])?;
    Ok(Snapshot {
        head: head.trim().into(),
        refs: refs.lines().filter_map(|l| l.split_once(' ')).map(|(r, o)| (r.into(), o.into())).collect(),
    })
}

/// Move HEAD and every local branch/tag back to `to`, resetting the checked-out
/// branch with `mode` (`--soft` keeps an undone commit staged, `--keep` protects local edits).
pub fn restore(dir: &Path, to: &Snapshot, mode: &str) -> Res<()> {
    let cur = snapshot(dir)?;
    // `update-ref` skips git's checked-out-elsewhere check: moving another worktree's
    // branch would leave its index and files out of step with its HEAD.
    let other = super::read::parse_worktrees(&run(dir, &["worktree", "list", "--porcelain", "-z"])?);
    if let Some(r) = other.iter().filter(|w| !w.prunable).filter_map(|w| Some(format!("refs/heads/{}", w.branch.as_ref()?))).find(|r| *r != cur.head && cur.refs.get(r) != to.refs.get(r)) {
        return Err(format!("{} is checked out in another worktree; switch that worktree to another branch first, then undo.", &r["refs/heads/".len()..]));
    }
    for (r, oid) in &to.refs {
        if !cur.refs.contains_key(r) {
            run(dir, &["update-ref", r, oid, ""])?;
        }
    }
    let detached = |h: &str| !h.starts_with("refs/");
    if cur.head != to.head {
        if detached(&cur.head) && detached(&to.head) {
            // Detached on both sides (e.g. a commit made on a detached HEAD): move HEAD itself.
            run(dir, &["reset", "-q", mode, &to.head, "--"])?;
        } else {
            // Move the target branch first: its stale tip could clash with local edits on checkout.
            if let (true, Some(c), Some(oid)) = (mode == "--keep", cur.refs.get(&to.head), to.refs.get(&to.head))
                && c != oid
            {
                run(dir, &["update-ref", &to.head, oid, c])?;
            }
            run(dir, &["checkout", "-q", to.head.strip_prefix("refs/heads/").unwrap_or(&to.head), "--"])?;
        }
    }
    for (r, oid) in &to.refs {
        match cur.refs.get(r) {
            Some(c) if c != oid && *r == to.head => {
                run(dir, &["reset", "-q", mode, oid, "--"])?;
            }
            Some(c) if c != oid => {
                run(dir, &["update-ref", r, oid, c])?;
            }
            _ => {}
        }
    }
    for (r, c) in &cur.refs {
        if !to.refs.contains_key(r) {
            run(dir, &["update-ref", "-d", r, c])?;
        }
    }
    Ok(())
}
