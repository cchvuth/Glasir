//! File watcher: live refresh when the working tree or `.git` changes elsewhere.

use std::path::Path;
use std::sync::mpsc::{Receiver, channel};

/// What a watcher event changed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Change {
    /// Refs, HEAD or a sequencer state: full reload.
    Refs,
    /// The index (often our own git writing it): status reload.
    Index,
    /// A worktree file: status reload.
    Tree,
}

/// Watches the working tree and the git dir so edits made elsewhere show up live.
pub(super) fn watch(ctx: &egui::Context, dir: &Path) -> Option<(notify::RecommendedWatcher, Receiver<Change>)> {
    use notify::Watcher;
    let (tx, rx) = channel();
    let (ctx, root) = (ctx.clone(), dir.to_path_buf());
    // A linked worktree's HEAD/index (`.git/worktrees/x`) and the shared refs and stashes
    // (the common dir) live outside it: watch the common dir too. Own git dir first.
    // `--path-format=absolute` resolves symlinks like `--show-toplevel` (the tab's dir) does.
    let git = |what| crate::git::run(dir, &["rev-parse", "--path-format=absolute", what]).ok().map(|p| std::path::PathBuf::from(p.trim()));
    let (own, common) = (git("--git-dir")?, git("--git-common-dir")?);
    let gits = [own, common.clone()];
    let git_dirs = gits.clone();
    // `ignored` verdicts: a build dir's churn must not trigger status reloads.
    let (mut ignored, mut tree_at) = (std::collections::HashMap::new(), None);
    let (raw, events) = channel::<notify::Event>();
    std::thread::spawn(move || {
        for e in events {
            if e.need_rescan() {
                let _ = tx.send(Change::Refs);
                ctx.request_repaint_after(std::time::Duration::from_millis(450));
                continue;
            }
            for p in &e.paths {
                if let Some(c) = classify(p, &gits, &root, &mut ignored, &mut tree_at) {
                    let _ = tx.send(c);
                    ctx.request_repaint_after(std::time::Duration::from_millis(450));
                }
            }
        }
    });
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(e) = res else { return };
        if matches!(e.kind, notify::EventKind::Access(_)) {
            return; // our own git reads (inotify IN_OPEN/CLOSE) are not changes
        }
        let _ = raw.send(e);
    })
    .ok()?;
    if w.watch(dir, notify::RecursiveMode::Recursive).is_err() {
        let _ = w.unwatch(dir);
        git_dirs.iter().try_for_each(|g| w.watch(g, notify::RecursiveMode::Recursive)).ok()?;
    } else if !common.starts_with(dir) {
        w.watch(&common, notify::RecursiveMode::Recursive).ok()?;
    }
    Some((w, rx))
}

fn classify(p: &Path, gits: &[std::path::PathBuf; 2], root: &Path, ignored: &mut std::collections::HashMap<String, bool>, tree_at: &mut Option<std::time::Instant>) -> Option<Change> {
    let Some((i, rel)) = gits.iter().enumerate().find_map(|(i, g)| Some((i, p.strip_prefix(g).ok()?))) else {
        let rel = p.strip_prefix(root).ok()?;
        if rel.components().next()?.as_os_str() == ".git" {
            return None;
        }
        if rel.file_name().is_some_and(|n| n == ".gitignore") {
            ignored.clear();
        }
        let due = tree_at.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(400));
        if is_ignored(root, rel, ignored, !due) {
            return None;
        }
        if !due {
            *tree_at = Some(std::time::Instant::now());
        }
        return Some(Change::Tree);
    };
    let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect();
    match parts.iter().map(|c| c.as_ref()).collect::<Vec<&str>>()[..] {
        ["config", ..] => {
            ignored.clear(); // core.excludesFile may have changed
            Some(Change::Refs)
        }
        ["info", "exclude"] => {
            ignored.clear();
            Some(Change::Tree)
        }
        ["refs" | "packed-refs" | "reftable", ..] | ["HEAD"] | ["worktrees", _, "HEAD"] => Some(Change::Refs),
        // Other per-worktree files count only in our own git dir, not the main worktree's (any HEAD does).
        ["MERGE_HEAD" | "rebase-merge" | "rebase-apply" | "CHERRY_PICK_HEAD" | "config.worktree", ..] if i == 0 => Some(Change::Refs),
        ["index", ..] if i == 0 => {
            ignored.clear(); // a file was tracked or untracked: verdicts may flip
            Some(Change::Index)
        }
        _ => None, // objects, logs, locks, other worktrees' state: noise
    }
}

/// `rel` (under `root`) is gitignored, as `git check-ignore` sees it: a tracked file, or a
/// directory holding one, is not. Ancestors are checked first (an ignored `target/` answers for
/// everything under it; with `leaf` false only they are). Verdicts are cached in `cache`; a path
/// that is gone may have been a file or a directory (`rm -rf target`): it is checked both ways.
fn is_ignored(root: &Path, rel: &Path, cache: &mut std::collections::HashMap<String, bool>, leaf: bool) -> bool {
    let mut ignored = |key: String| *cache.entry(key).or_insert_with_key(|k| crate::git::run(root, &["check-ignore", "-q", "--", k]).is_ok());
    let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect();
    let Some((name, dirs)) = parts.split_last() else { return false };
    let mut key = String::new();
    for d in dirs {
        key = key + d + "/";
        if ignored(key.clone()) {
            return true;
        }
    }
    if !leaf {
        return false;
    }
    key += name;
    match std::fs::symlink_metadata(root.join(rel)) {
        Ok(m) if m.is_dir() => ignored(key + "/"),
        Ok(_) => ignored(key),
        Err(_) => ignored(key.clone()) || ignored(key + "/"),
    }
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
        let full = (0..40).any(|_| rx.recv_timeout(std::time::Duration::from_millis(250)) == Ok(super::Change::Refs));
        assert!(full, "no full reload for a stash made in the main worktree");
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reftable_writes_are_ref_changes() {
        use std::path::{Path, PathBuf};
        let (own, common) = (PathBuf::from("/r/.git/worktrees/wt"), PathBuf::from("/r/.git"));
        let gits = [own.clone(), common.clone()];
        let c = |p: PathBuf| super::classify(&p, &gits, Path::new("/wt"), &mut Default::default(), &mut None);
        assert_eq!(c(own.join("reftable/tables.list")), Some(super::Change::Refs));
        assert_eq!(c(common.join("reftable/0x000000000001-0x000000000002-1d179f9d.ref")), Some(super::Change::Refs));
        assert_eq!(c(common.join("worktrees/other/reftable/tables.list")), None);
        let canon = |p: PathBuf| if cfg!(windows) { p } else { std::fs::canonicalize(p).unwrap() };
        let dir = canon(scratch("watch-reftable"));
        if crate::git::run(&dir, &["init", "-q", "--ref-format=reftable", "-b", "main"]).is_err() {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        identity(&dir);
        commit(&dir, "a", "a");
        let ctx = egui::Context::default();
        let (_w, rx) = super::watch(&ctx, &dir).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        sh(&dir, &["branch", "x"]);
        let full = (0..40).any(|_| rx.recv_timeout(std::time::Duration::from_millis(250)) == Ok(super::Change::Refs));
        assert!(full, "no full reload for a branch made in a reftable repo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Config (remotes, author) and any worktree's HEAD are ref changes; another worktree's sequencer state is not.
    #[test]
    fn config_and_every_worktrees_head_are_ref_changes() {
        use std::path::{Path, PathBuf};
        let (own, common) = (PathBuf::from("/r/.git/worktrees/wt"), PathBuf::from("/r/.git"));
        let gits = [own.clone(), common.clone()];
        let c = |p: PathBuf| super::classify(&p, &gits, Path::new("/wt"), &mut Default::default(), &mut None);
        for p in [common.join("config"), own.join("config.worktree"), common.join("HEAD"), common.join("worktrees/other/HEAD"), own.join("HEAD")] {
            assert_eq!(c(p.clone()), Some(super::Change::Refs), "{p:?}");
        }
        for p in [common.join("config.worktree"), common.join("MERGE_HEAD"), common.join("worktrees/other/index"), common.join("FETCH_HEAD")] {
            assert_eq!(c(p.clone()), None, "{p:?}");
        }
    }

    #[test]
    fn exclude_and_config_edits_drop_ignore_verdicts() {
        use std::path::{Path, PathBuf};
        let common = PathBuf::from("/r/.git");
        let gits = [common.clone(), common.clone()];
        for (p, change) in [("info/exclude", super::Change::Tree), ("config", super::Change::Refs)] {
            let mut cache = std::collections::HashMap::from([("build".to_owned(), true)]);
            assert_eq!(super::classify(&common.join(p), &gits, Path::new("/r"), &mut cache, &mut None), Some(change));
            assert!(cache.is_empty(), "{p}");
        }
    }

    /// A checkout writing many files: once one counted, the rest within the debounce run no
    /// `check-ignore` of their own (an ignored dir still answers for what's under it).
    #[test]
    fn a_burst_of_new_files_runs_few_ignore_checks() {
        let dir = scratch("watch-burst");
        sh(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        let gits = [dir.join(".git"), dir.join(".git")];
        let (mut cache, mut at) = (Default::default(), None);
        let checks = || crate::git::LOG.lock().unwrap().iter().filter(|l| l.cwd == dir && l.args.iter().any(|a| a == "check-ignore")).count();
        let before = checks();
        for i in 0..30 {
            std::fs::write(dir.join(format!("src/f{i}")), "x").unwrap();
            assert_eq!(super::classify(&dir.join(format!("src/f{i}")), &gits, &dir, &mut cache, &mut at), Some(super::Change::Tree));
        }
        assert_eq!(super::classify(&dir.join("target/out.o"), &gits, &dir, &mut cache, &mut at), None);
        assert!(checks() - before <= 3, "{} checks", checks() - before);
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

    /// Tracked files under an ignored dir still count; a deletion seen first must not
    /// poison the verdict for the dir.
    #[test]
    fn ignore_verdicts_follow_check_ignore() {
        let dir = scratch("watch-ign-verdict");
        sh(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join(".gitignore"), "target/\nvendor/\n*.log\n").unwrap();
        for d in ["target/a", "vendor"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        for f in ["target/a/x.o", "vendor/kept.c", "vendor/junk.c", "x.log", "src.rs"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        sh(&dir, &["add", "-f", "vendor/kept.c"]);
        let mut cache = Default::default();
        let mut ign = |p: &str| super::is_ignored(&dir, std::path::Path::new(p), &mut cache, true);
        assert!(!ign("src.rs") && ign("x.log") && ign("target/a/x.o"));
        assert!(!ign("vendor/kept.c"), "tracked under an ignored dir");
        assert!(ign("vendor/junk.c"));
        std::fs::remove_dir_all(dir.join("target")).unwrap();
        assert!(ign("target"), "a deleted ignored dir");
        std::fs::create_dir_all(dir.join("target/a")).unwrap();
        assert!(ign("target") && ign("target/a/y.o"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
