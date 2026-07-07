//! File watcher: live refresh when the working tree or `.git` changes elsewhere.

use std::path::Path;
use std::sync::mpsc::{Receiver, channel};

/// Watches the working tree and the git dir so edits made elsewhere show up live.
/// Sends `true` when refs/HEAD changed (full reload), `false` for status-only.
pub(super) fn watch(ctx: &egui::Context, dir: &Path) -> Option<(notify::RecommendedWatcher, Receiver<bool>)> {
    use notify::Watcher;
    let (tx, rx) = channel();
    let (ctx, root) = (ctx.clone(), dir.to_path_buf());
    // A linked worktree's HEAD/index (`.git/worktrees/x`) and the shared refs and stashes
    // (the common dir) live outside it: watch the common dir too. Own git dir first.
    // `--path-format=absolute` resolves symlinks like `--show-toplevel` (the tab's dir) does.
    let git = |what| crate::git::run(dir, &["rev-parse", "--path-format=absolute", what]).ok().map(|p| std::path::PathBuf::from(p.trim()));
    let (own, common) = (git("--git-dir")?, git("--git-common-dir")?);
    let gits = [own, common.clone()];
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(e) = res else { return };
        if matches!(e.kind, notify::EventKind::Access(_)) {
            return; // our own git reads (inotify IN_OPEN/CLOSE) are not changes
        }
        for p in &e.paths {
            let kind = match gits.iter().enumerate().find_map(|(i, g)| Some((i, p.strip_prefix(g).ok()?))) {
                Some((i, rel)) => match rel.components().next().map(|c| c.as_os_str().to_string_lossy()).as_deref() {
                    Some("refs" | "packed-refs") => Some(true),
                    // Per-worktree files count only in our own git dir, not the main worktree's.
                    Some("HEAD" | "MERGE_HEAD" | "rebase-merge" | "rebase-apply" | "CHERRY_PICK_HEAD") if i == 0 => Some(true),
                    Some("index") if i == 0 => Some(false),
                    _ => None, // objects, logs, locks, other worktrees: noise
                },
                None => p.strip_prefix(&root).ok().and_then(|rel| rel.components().next()).filter(|c| c.as_os_str() != ".git").map(|_| false),
            };
            if let Some(full) = kind {
                let _ = tx.send(full);
                ctx.request_repaint_after(std::time::Duration::from_millis(450));
            }
        }
    })
    .ok()?;
    w.watch(dir, notify::RecursiveMode::Recursive).ok()?;
    if !common.starts_with(dir) {
        w.watch(&common, notify::RecursiveMode::Recursive).ok()?;
    }
    Some((w, rx))
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{commit, identity, scratch, sh};

    /// A linked worktree's refs, stashes and HEAD live in the main repo's `.git`.
    #[test]
    fn a_linked_worktree_sees_ref_changes_in_the_main_repo() {
        let canon = |p: std::path::PathBuf| std::fs::canonicalize(p).unwrap();
        let (dir, wt) = (canon(scratch("watch-main")), canon(scratch("watch-wt")));
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a");
        sh(&dir, &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()]);
        let ctx = egui::Context::default();
        let (_w, rx) = super::watch(&ctx, &wt).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500)); // FSEvents start-up latency
        std::fs::write(dir.join("a"), "b").unwrap();
        sh(&dir, &["stash", "push", "-q"]);
        let full = (0..40).any(|_| rx.recv_timeout(std::time::Duration::from_millis(250)).unwrap_or(false));
        assert!(full, "no full reload for a stash made in the main worktree");
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
