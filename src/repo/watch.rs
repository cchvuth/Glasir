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
    // Top-level entries known to be gitignored (a build dir's churn must not trigger status
    // reloads). Reset when a .gitignore changes. ponytail: whole-dir verdict, so tracked files
    // under an ignored dir are missed; per-path check-ignore if that matters.
    let mut ignored: std::collections::HashMap<std::ffi::OsString, bool> = Default::default();
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
                None => p.strip_prefix(&root).ok().and_then(|rel| {
                    let first = rel.components().next()?.as_os_str().to_owned();
                    if first == ".git" {
                        return None;
                    }
                    if rel.file_name().is_some_and(|n| n == ".gitignore") {
                        ignored.clear();
                    }
                    let is_dir = rel.components().count() > 1;
                    let skip = *ignored.entry(first.clone()).or_insert_with(|| {
                        let mut arg = first.to_string_lossy().into_owned();
                        if is_dir {
                            arg.push('/');
                        }
                        crate::git::run(&root, &["check-ignore", "-q", "--", &arg]).is_ok()
                    });
                    (!skip).then_some(false)
                }),
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
        // macOS reports /private/var for /var; Windows canonical paths are \\?\ ones git can't use.
        let canon = |p: std::path::PathBuf| if cfg!(windows) { p } else { std::fs::canonicalize(p).unwrap() };
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

    /// Writes under a gitignored dir are not worktree changes.
    #[test]
    fn ignored_dirs_send_no_events() {
        let canon = |p: std::path::PathBuf| if cfg!(windows) { p } else { std::fs::canonicalize(p).unwrap() };
        let dir = canon(scratch("watch-ign"));
        sh(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir(dir.join("target")).unwrap();
        let ctx = egui::Context::default();
        let (_w, rx) = super::watch(&ctx, &dir).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        while rx.try_recv().is_ok() {}
        for i in 0..5 {
            std::fs::write(dir.join("target/out.o"), i.to_string()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(rx.try_recv().is_err(), "event for an ignored path");
        std::fs::write(dir.join("src.rs"), "x").unwrap();
        assert!(rx.recv_timeout(std::time::Duration::from_secs(3)).is_ok(), "no event for a normal file");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
