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
    if let Some(r) = used_elsewhere(dir)?.into_iter().find(|r| *r != cur.head && cur.refs.get(r) != to.refs.get(r)) {
        return Err(format!("{} is in use in another worktree (checked out, or being rebased or bisected there); finish there or switch that worktree to another branch first, then undo.", r.strip_prefix("refs/heads/").unwrap_or(&r)));
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

/// Branches other worktrees have checked out, or are rebasing or bisecting (those show as detached).
fn used_elsewhere(dir: &Path) -> Res<Vec<String>> {
    let wts = super::read::parse_worktrees(&run(dir, &["worktree", "list", "--porcelain", "-z"])?);
    let mut refs: Vec<String> = wts.iter().filter(|w| !w.prunable).filter_map(|w| Some(format!("refs/heads/{}", w.branch.as_ref()?))).collect();
    let out = run(dir, &["rev-parse", "--path-format=absolute", "--git-dir", "--git-common-dir"])?;
    let mut l = out.lines().map(|l| std::fs::canonicalize(l).unwrap_or_else(|_| l.into()));
    let (own, common) = (l.next().unwrap_or_default(), l.next().unwrap_or_default());
    let linked = std::fs::read_dir(common.join("worktrees")).into_iter().flatten().flatten().map(|e| e.path());
    for g in std::iter::once(common.clone()).chain(linked).filter(|g| std::fs::canonicalize(g).ok().as_ref() != Some(&own)) {
        let read = |f: &str| std::fs::read_to_string(g.join(f)).ok().map(|s| s.trim().to_owned());
        refs.extend(read("rebase-merge/head-name").into_iter().chain(read("rebase-apply/head-name")));
        refs.extend(read("BISECT_START").map(|b| format!("refs/heads/{b}")));
    }
    Ok(refs)
}
