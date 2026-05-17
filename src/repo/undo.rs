//! Undo/redo: ref snapshots for ops that move refs, and backups of discarded
//! work so a discard can be undone.

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
}

pub(super) enum UndoKind {
    /// `autostash`: the op carried uncommitted changes along; undo/redo carries them back.
    Refs { before: Snapshot, after: Snapshot, mode: Undo, autostash: Option<String> },
    Discard(Saved),
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
    let stash = git::run(dir, &["stash", "create"]).map_err(|e| refuse(e.trim().to_owned()))?;
    let stash = Some(stash.trim().to_owned()).filter(|s| !s.is_empty());
    let (tracked, untracked): (Vec<String>, Vec<String>) = match b {
        Backup::All => (
            split_z(git::run(dir, &["diff", "--name-only", "-z", "HEAD"]).unwrap_or_default()),
            split_z(git::run(dir, &["ls-files", "-z", "--others", "--exclude-standard"]).map_err(refuse)?),
        ),
        Backup::Paths(ps) => ps.iter().cloned().partition(|p| {
            git::run(dir, &["--literal-pathspecs", "ls-files", "-z", "--", p]).is_ok_and(|o| !o.is_empty())
        }),
    };
    // ponytail: untracked copies live in memory, capped so a stray huge file can't exhaust RAM.
    let mut budget: u64 = 256 << 20;
    let mut files = vec![];
    for p in untracked {
        let path = dir.join(&p);
        let meta = std::fs::symlink_metadata(&path).map_err(|e| refuse(format!("{p}: {e}")))?;
        if !meta.is_file() {
            return Err(refuse(format!("{p} is not a regular file")));
        }
        budget = budget.checked_sub(meta.len()).ok_or_else(|| refuse(format!("{p} is too large")))?;
        files.push((p.clone(), std::fs::read(&path).map_err(|e| refuse(format!("{p}: {e}")))?));
    }
    Ok(Saved { stash, tracked, files })
}

pub(super) fn restore_worktree(dir: &Path, s: &Saved) -> Res<()> {
    for (p, bytes) in &s.files {
        let path = dir.join(p);
        if !path.exists() {
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
    if git::snapshot(dir)? != *expect {
        return Err("The repository changed outside Glasir since this action; undo history cleared.".into());
    }
    let restore = || git::restore(dir, target, mode).map(|_| String::new());
    match autostash {
        Some(msg) => git::with_autostash(dir, msg, restore).map(drop),
        None => restore().map(drop),
    }
}

impl RepoTab {
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|u| u.label.as_str())
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
}
