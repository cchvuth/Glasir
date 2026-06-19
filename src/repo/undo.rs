//! Undo/redo: ref snapshots for ops that move refs, backups of discarded
//! work so a discard can be undone, and reverting ops for config changes.

use super::op::{Backup, Done, Undo};
use super::task::Task;
use super::RepoTab;
use crate::git::{self, Res, Snapshot};
use std::path::Path;

/// Discarded work: a `git stash create` commit plus copies of deleted untracked files.
#[derive(Clone)]
pub(super) struct Saved {
    /// `git stash create` commit holding the tracked files' contents.
    pub(super) stash: Option<String>,
    pub(super) tracked: Vec<String>,
    /// Untracked files' bytes.
    pub(super) files: Vec<(String, Vec<u8>)>,
    /// `Backup::Overwrite`: put `files` back even if the path exists.
    pub(super) overwrite: bool,
}

pub(super) enum UndoKind {
    /// `autostash`: the op carried uncommitted changes along; undo/redo carries them back.
    Refs { before: Snapshot, after: Snapshot, mode: Undo, autostash: Option<String> },
    Discard(Saved),
    /// A config-only op (remotes) and the op that reverts it.
    Op { undo: Box<super::Op>, redo: Box<super::Op> },
}

pub(super) struct UndoEntry {
    pub(super) label: String,
    pub(super) kind: UndoKind,
}

fn split_z(s: String) -> Vec<String> {
    s.split('\0').filter(|p| !p.is_empty()).map(String::from).collect()
}

/// Back up everything a discard will destroy. Any failure aborts the discard.
pub(super) fn save_worktree(dir: &Path, b: &Backup) -> Res<Saved> {
    let refuse = |why: String| format!("Nothing was discarded: couldn't back it up first ({why}).");
    let (tracked, untracked): (Vec<String>, Vec<String>) = match b {
        Backup::All => (
            split_z(git::run(dir, &["diff", "--name-only", "-z", "HEAD"]).unwrap_or_default()),
            split_z(git::run(dir, &["ls-files", "-z", "--others", "--exclude-standard"]).map_err(refuse)?),
        ),
        Backup::Paths(ps) | Backup::Overwrite(ps) => ps.iter().cloned().partition(|p| {
            git::run(dir, &["--literal-pathspecs", "ls-files", "-z", "--", p]).is_ok_and(|o| !o.is_empty())
        }),
    };
    // `stash create` fails before the first commit and while the index has conflicts:
    // then the tracked files' bytes are copied like untracked ones (`stash` = None).
    let stash = if tracked.is_empty() {
        None
    } else {
        git::run(dir, &["stash", "create"]).ok().and_then(|s| {
            // No tracked file changed yet: HEAD already holds their contents (e.g. a
            // restore from history about to overwrite a clean file).
            Some(s.trim().to_owned()).filter(|s| !s.is_empty()).or_else(|| git::run(dir, &["rev-parse", "-q", "--verify", "HEAD"]).ok().map(|h| h.trim().to_owned()))
        })
    };
    // Copies live in memory, capped so a stray huge file can't exhaust RAM.
    let mut budget: u64 = 256 << 20;
    let mut files = vec![];
    for p in untracked.into_iter().chain(stash.is_none().then(|| tracked.clone()).into_iter().flatten()) {
        let path = dir.join(&p);
        let meta = match std::fs::symlink_metadata(&path) {
            // Not there yet (a file restored from history): nothing to lose.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            m => m.map_err(|e| refuse(format!("{p}: {e}")))?,
        };
        if !meta.is_file() {
            return Err(refuse(format!("{p} is not a regular file")));
        }
        budget = budget.checked_sub(meta.len()).ok_or_else(|| refuse(format!("{p} is too large")))?;
        files.push((p.clone(), std::fs::read(&path).map_err(|e| refuse(format!("{p}: {e}")))?));
    }
    Ok(Saved { stash, tracked, files, overwrite: matches!(b, Backup::Overwrite(_)) })
}

pub(super) fn restore_worktree(dir: &Path, s: &Saved) -> Res<()> {
    for (p, bytes) in &s.files {
        let path = dir.join(p);
        // Copied tracked files (and `Backup::Overwrite` copies) overwrite what's there now.
        if s.overwrite || !path.exists() || (s.stash.is_none() && s.tracked.contains(p)) {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        }
    }
    // Only the discarded paths, so other edits made since don't block the restore.
    if let Some(sha) = s.stash.as_ref().filter(|_| !s.tracked.is_empty()) {
        let mut args = vec!["--literal-pathspecs", "restore", "--source", sha, "--worktree", "--"];
        args.extend(s.tracked.iter().map(String::as_str));
        git::run(dir, &args)?;
    }
    Ok(())
}

/// Undo/redo: move refs from `expect` back to `target`, carrying uncommitted
/// changes along when the op did (`autostash`).
pub(super) fn move_refs(dir: &Path, expect: &Snapshot, target: &Snapshot, mode: &str, autostash: Option<&str>) -> Res<()> {
    let cur = git::snapshot(dir)?;
    let (mut expect, mut target) = (expect.clone(), target.clone());
    // Tags a Fetch brought in since are unrelated to this entry: keep them, they're no change.
    for (r, o) in &cur.refs {
        if !r.starts_with("refs/tags/") || expect.refs.contains_key(r) || target.refs.contains_key(r) {
            continue;
        }
        expect.refs.insert(r.clone(), o.clone());
        target.refs.insert(r.clone(), o.clone());
    }
    if cur != expect {
        return Err("The repository changed outside Glasir since this action; undo history cleared.".into());
    }
    let restore = || git::restore(dir, &target, mode).map(|_| String::new());
    match autostash {
        Some(msg) => git::with_autostash(dir, msg, restore).map(drop),
        None => restore().map(drop),
    }
}

impl RepoTab {
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|u| u.label.as_str())
    }
    /// Undo stack height: a toast's Undo button stays live while it is unchanged.
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|u| u.label.as_str())
    }

    pub(super) fn undo_redo(&mut self, ctx: &egui::Context, undo: bool) {
        let stack = if undo { &self.undo } else { &self.redo };
        let Some(e) = stack.last() else { return };
        if self.running.is_some() {
            return;
        }
        let dir = self.dir.clone();
        let label = format!("{} {}", if undo { "Undo" } else { "Redo" }, e.label);
        let job: Box<dyn FnOnce() -> Res<()> + Send> = match &e.kind {
            UndoKind::Refs { before, after, mode, autostash } => {
                let (expect, target) = if undo { (after.clone(), before.clone()) } else { (before.clone(), after.clone()) };
                let (mode, autostash) = (mode.mode(), autostash.clone());
                Box::new(move || move_refs(&dir, &expect, &target, mode, autostash.as_deref()))
            }
            UndoKind::Op { undo: u, redo } => {
                let op = if undo { u.clone() } else { redo.clone() };
                Box::new(move || super::op::exec(&dir, &op).map(drop))
            }
            // Restoring discarded work has no meaningful redo.
            UndoKind::Discard(_) if !undo => return,
            UndoKind::Discard(saved) => {
                let saved = saved.clone();
                Box::new(move || restore_worktree(&dir, &saved))
            }
        };
        self.running = Some((label, Task::spawn(ctx, move || Done::Undo(undo, job()))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discard_is_restorable() {
        let dir = std::env::temp_dir().join(format!("glasir-discard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| git::run(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "T"]);
        g(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        g(&["add", "a.txt"]);
        g(&["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("a.txt"), "edited\n").unwrap();
        std::fs::create_dir_all(dir.join("new")).unwrap();
        std::fs::write(dir.join("new/b.txt"), "fresh\n").unwrap();

        let saved = save_worktree(&dir, &Backup::All).unwrap();
        assert!(saved.stash.is_some());
        g(&["reset", "--hard", "-q"]);
        g(&["clean", "-fdq"]);
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
        assert!(!dir.join("new/b.txt").exists());

        restore_worktree(&dir, &saved).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "edited\n");
        assert_eq!(std::fs::read_to_string(dir.join("new/b.txt")).unwrap(), "fresh\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `stash create` fails before the first commit and with an unmerged index.
    #[test]
    fn discard_backs_up_without_stash() {
        use crate::repo::testutil::{commit, identity, scratch, sh};
        let dir = scratch("discard-nostash");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        std::fs::write(dir.join("u"), "u\n").unwrap();
        assert_eq!(save_worktree(&dir, &Backup::Paths(vec!["u".into()])).unwrap().files.len(), 1);

        commit(&dir, "f", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "f", "theirs\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "f", "ours\n");
        assert!(git::run(&dir, &["merge", "-q", "feat"]).is_err());
        let conflicted = std::fs::read_to_string(dir.join("f")).unwrap();
        let saved = save_worktree(&dir, &Backup::All).unwrap();
        sh(&dir, &["reset", "--hard", "-q"]);
        sh(&dir, &["clean", "-fdq"]);
        assert!(!dir.join("u").exists());
        restore_worktree(&dir, &saved).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), conflicted);
        assert_eq!(std::fs::read_to_string(dir.join("u")).unwrap(), "u\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tag fetched since an undo entry doesn't block undoing it, and stays.
    /// An op that stops on a conflict mid-rebase records no entry (Abort is the way back).
    #[test]
    fn undo_survives_new_tags_and_skips_stopped_rebases() {
        use super::super::testutil::{commit, identity, scratch, settle, sh};
        use super::super::{Action, Op, RepoTab, Undo, View};
        let dir = scratch("undo-tags");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "one\n");
        let before = git::snapshot(&dir).unwrap();
        commit(&dir, "a.txt", "two\n");
        let after = git::snapshot(&dir).unwrap();
        sh(&dir, &["update-ref", "refs/tags/v2", "HEAD"]);
        move_refs(&dir, &after, &before, "--keep", None).unwrap();
        assert!(sh(&dir, &["tag"]).contains("v2"));
        assert_eq!(git::snapshot(&dir).unwrap().refs.get("refs/heads/main"), before.refs.get("refs/heads/main"));

        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "a.txt", "feat\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "a.txt", "main\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Rebase feat onto main", &["rebase", "main", "feat"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        assert_eq!(t.data.as_ref().unwrap().state, git::State::Rebasing);
        assert_eq!(t.undo_label(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
