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
    /// `Backup::All`: paths whose index differs from HEAD (some only there: "MM" back to HEAD, "AD").
    pub(super) staged: Vec<String>,
    /// `Backup::Paths`: paths that were conflicted; undo brings the conflict back.
    pub(super) unmerged: Vec<String>,
    /// Untracked files' bytes, and tracked files' exact bytes (line endings) when they fit.
    pub(super) files: Vec<(String, Vec<u8>)>,
    /// `Backup::Overwrite`: put `files` back even if the path exists.
    pub(super) overwrite: bool,
    /// `Backup::All` with a real `stash create` commit: undo also restores its index (`^2`).
    pub(super) index: bool,
    /// What each path held right after the op (`seal`): undo refuses once one changed.
    /// Shared with the undo job, which re-seals what it restored so a half-done undo can be retried.
    pub(super) after: std::sync::Arc<std::sync::Mutex<Vec<(String, Option<u64>)>>>,
}

impl Saved {
    /// Records the paths' state after the discard ran.
    pub(super) fn seal(&mut self, dir: &Path) {
        let mut paths: Vec<&String> = self.tracked.iter().chain(&self.staged).chain(self.files.iter().map(|(p, _)| p)).collect();
        paths.sort_unstable();
        paths.dedup();
        *self.after.lock().unwrap() = paths.into_iter().map(|p| (p.clone(), fingerprint(&dir.join(p)))).collect();
    }
}

/// A hash of what's at `path` (None = nothing), to tell whether it changed.
fn fingerprint(path: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path).ok()?;
    let mut h = std::hash::DefaultHasher::new();
    (meta.is_symlink(), std::fs::read_link(path).ok(), meta.is_file()).hash(&mut h);
    // Streamed: a multi-GB file must not be read into memory.
    if let (true, Ok(mut f)) = (meta.is_file(), std::fs::File::open(path)) {
        let mut buf = vec![0; 1 << 16];
        loop {
            match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => h.write(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Some(0),
            }
        }
    }
    Some(h.finish())
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
    /// Unique, increasing: a toast's Undo button stays live while its step is on top.
    id: u64,
}

impl UndoEntry {
    pub(super) fn new(label: String, kind: UndoKind) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        UndoEntry { label, kind, id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) }
    }
}

fn split_z(s: String) -> Vec<String> {
    s.split('\0').filter(|p| !p.is_empty()).map(String::from).collect()
}

/// Back up everything a discard will destroy. Any failure aborts the discard.
pub(super) fn save_worktree(dir: &Path, b: &Backup) -> Res<Saved> {
    let refuse = |why: String| format!("Nothing was discarded: couldn't back it up first ({why}).");
    let (tracked, untracked): (Vec<String>, Vec<String>) = match b {
        Backup::All => (
            // No HEAD yet (unborn branch): every index entry is a staged new file.
            split_z(git::run(dir, &["diff", "--name-only", "-z", "HEAD"]).or_else(|_| git::run(dir, &["ls-files", "-z"])).map_err(refuse)?),
            split_z(git::run(dir, &["ls-files", "-z", "--others", "--exclude-standard"]).map_err(refuse)?),
        ),
        Backup::Paths(ps) | Backup::Overwrite(ps) => ps.iter().cloned().partition(|p| {
            git::run(dir, &["--literal-pathspecs", "ls-files", "-z", "--", p]).is_ok_and(|o| !o.is_empty())
        }),
    };
    // Unborn branch: `tracked` already lists every index entry.
    let staged = match b {
        Backup::All => git::run(dir, &["diff", "--cached", "--name-only", "-z", "--no-renames", "HEAD"]).map(split_z).unwrap_or_default(),
        _ => vec![],
    };
    let unmerged = match b {
        Backup::All => vec![],
        _ => split_z(git::run(dir, &["diff", "--name-only", "-z", "--diff-filter=U"]).map_err(refuse)?).into_iter().filter(|p| tracked.contains(p)).collect(),
    };
    // git prints raw bytes; a name that isn't UTF-8 decodes lossily and would match no file.
    if let Some(p) = tracked.iter().chain(&staged).chain(&untracked).find(|p| p.contains('\u{FFFD}')) {
        return Err(refuse(format!("{p}: file name isn't valid UTF-8")));
    }
    // `stash create` fails before the first commit and while the index has conflicts:
    // then the tracked files' bytes are copied like untracked ones (`stash` = None).
    let created = (!tracked.is_empty() || !staged.is_empty()).then(|| git::run(dir, &["stash", "create"]).ok()).flatten();
    let index = matches!(b, Backup::All) && created.as_ref().is_some_and(|s| !s.trim().is_empty());
    let stash = created.and_then(|s| {
        // No tracked file changed yet: HEAD already holds their contents (e.g. a
        // restore from history about to overwrite a clean file).
        Some(s.trim().to_owned()).filter(|s| !s.is_empty()).or_else(|| git::run(dir, &["rev-parse", "-q", "--verify", "HEAD"]).ok().map(|h| h.trim().to_owned()))
    });
    // Copies live in memory, capped so a stray huge file can't exhaust RAM.
    let mut budget: u64 = 256 << 20;
    let mut files = vec![];
    let copied: std::collections::HashSet<&String> = untracked.iter().collect();
    // With a stash, tracked copies are optional: they only keep the exact bytes, which the
    // stash's clean/smudge round trip may change (autocrlf, eol, filters).
    let optional = tracked.iter().filter(|p| !copied.contains(p)).map(|p| (p, stash.is_some()));
    for (p, optional) in untracked.iter().map(|p| (p, false)).chain(optional) {
        let path = dir.join(p);
        let meta = match std::fs::symlink_metadata(&path) {
            // Not there yet (a file restored from history): nothing to lose.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            m => m.map_err(|e| refuse(format!("{p}: {e}")))?,
        };
        if optional && (!meta.is_file() || meta.len() > budget) {
            continue;
        }
        if !meta.is_file() {
            return Err(refuse(format!("{p} is not a regular file")));
        }
        budget = budget.checked_sub(meta.len()).ok_or_else(|| refuse(format!("{p} is too large")))?;
        files.push((p.clone(), std::fs::read(&path).map_err(|e| refuse(format!("{p}: {e}")))?));
    }
    Ok(Saved { stash, tracked, staged, unmerged, files, overwrite: matches!(b, Backup::Overwrite(_)), index, after: Default::default() })
}

pub(super) fn restore_worktree(dir: &Path, s: &Saved) -> Res<()> {
    let mut after = s.after.lock().unwrap();
    if let Some((p, _)) = after.iter().find(|(p, f)| fingerprint(&dir.join(p)) != *f) {
        return Err(format!("{p} changed since the discard, and undoing it would overwrite that. Move the file aside, then undo again."));
    }
    // What this undo itself changed is no "change since" for a retry after a later step fails.
    let mut reseal = |ps: &[&String]| after.iter_mut().filter(|(p, _)| ps.contains(&p)).for_each(|(p, f)| *f = fingerprint(&dir.join(&*p)));
    let tracked: std::collections::HashSet<&String> = s.tracked.iter().collect();
    // Only the discarded paths, so other edits made since don't block the restore.
    if let Some(sha) = &s.stash {
        let restore = |source: &str, what: &str, paths: &[String]| {
            git::run_input(dir, &["--literal-pathspecs", "restore", "--source", source, what, "--pathspec-from-file=-", "--pathspec-file-nul"], Some(paths.join("\0").as_bytes()))
        };
        if !s.tracked.is_empty() {
            let res = restore(sha, "--worktree", &s.tracked);
            reseal(&s.tracked.iter().collect::<Vec<_>>());
            res?;
        }
        // The index too, unless it changed since for these paths (that staging would be lost).
        let now = if s.index { split_z(git::run(dir, &["diff", "--cached", "--name-only", "-z", "--no-renames", &format!("{sha}^1")])?) } else { vec![] };
        if s.index && !s.staged.is_empty() && !now.iter().any(|p| tracked.contains(p) || s.staged.contains(p)) {
            restore(&format!("{sha}^2"), "--staged", &s.staged)?;
        }
    }
    for (p, bytes) in &s.files {
        let path = dir.join(p);
        let meta = std::fs::symlink_metadata(&path);
        // Copied tracked files (and `Backup::Overwrite` copies) overwrite what's there now.
        if s.overwrite || meta.is_err() || tracked.contains(p) {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            // Never write through a symlink the op left there: replace the link itself.
            if meta.is_ok_and(|m| m.is_symlink()) {
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            }
            let res = std::fs::write(&path, bytes);
            reseal(&[p]);
            res.map_err(|e| e.to_string())?;
        }
    }
    // A resolution the op made (take a side, delete) is undone too: git keeps the stages (resolve-undo).
    // Only while that merge (rebase, pick) is still going: the stages outlive its commit.
    if !s.unmerged.is_empty() && !matches!(git::state(&git::git_dir(dir)?), git::State::Clean | git::State::Bisecting) {
        let mut a = vec!["update-index", "--unresolve", "--"];
        a.extend(s.unmerged.iter().map(String::as_str));
        git::run(dir, &a)?;
    }
    // Without a stash every tracked path present then was copied: one without a copy was
    // absent before the op (a discarded rename's old path), so it goes again.
    if s.stash.is_none() {
        for p in s.tracked.iter().filter(|p| !s.files.iter().any(|(f, _)| f == *p)) {
            let res = std::fs::remove_file(dir.join(p));
            reseal(&[p]);
            match res {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("{p}: {e}")),
                _ => {}
            }
        }
    }
    Ok(())
}

/// Undo/redo: move refs from `expect` back to `target`, carrying uncommitted
/// changes along when the op did (`autostash`). Err's flag: refused before anything moved
/// (a branch in use in another worktree), so the step can be retried.
pub(super) fn move_refs(dir: &Path, expect: &Snapshot, target: &Snapshot, mode: &str, autostash: Option<&str>) -> Result<(), (String, bool)> {
    let cur = git::snapshot(dir).map_err(|e| (e, false))?;
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
        return Err(("The repository changed outside Glasir since this action; undo history cleared.".into(), false));
    }
    let restore = || git::restore(dir, &target, mode).map(|_| String::new());
    let res = match autostash {
        Some(msg) => git::with_autostash(dir, msg, restore).map(drop),
        None => restore().map(drop),
    };
    res.map_err(|e| (e, git::snapshot(dir).is_ok_and(|s| s == cur)))
}

impl RepoTab {
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|u| u.label.as_str())
    }
    /// The newest undo step's id; a new step (or a redo) makes it larger.
    pub fn undo_top(&self) -> Option<u64> {
        self.undo.last().map(|u| u.id)
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
        // Moving refs mid merge/rebase would corrupt it; a guided split's own steps are fine.
        let busy = self.data.as_ref().is_some_and(|d| !matches!(git::state(&d.git_dir), git::State::Clean | git::State::Bisecting));
        if busy && self.split.is_none() && matches!(e.kind, UndoKind::Refs { .. }) {
            return self.notes.push(super::Msg { text: "Finish or abort the operation in progress first, then undo.".into(), error: true });
        }
        let dir = self.dir.clone();
        let label = format!("{} {}", if undo { "Undo" } else { "Redo" }, e.label);
        // Err's flag: the refs didn't move, so the ref steps still hold.
        let job: Box<dyn FnOnce() -> Result<(), (String, bool)> + Send> = match &e.kind {
            UndoKind::Refs { before, after, mode, autostash } => {
                let (expect, target) = if undo { (after.clone(), before.clone()) } else { (before.clone(), after.clone()) };
                let (mode, autostash) = (mode.mode(), autostash.clone());
                Box::new(move || move_refs(&dir, &expect, &target, mode, autostash.as_deref()))
            }
            UndoKind::Op { undo: u, redo } => {
                let op = if undo { u.clone() } else { redo.clone() };
                Box::new(move || super::op::exec(&dir, &op).map(drop).map_err(|e| (e, false)))
            }
            // Restoring discarded work has no meaningful redo.
            UndoKind::Discard(_) if !undo => return,
            UndoKind::Discard(saved) => {
                let saved = saved.clone();
                Box::new(move || restore_worktree(&dir, &saved).map_err(|e| (e, false)))
            }
        };
        self.running = Some((
            label,
            Task::spawn(ctx, move || match job() {
                Ok(()) => Done::Undo(undo, Ok(()), false),
                Err((e, kept)) => Done::Undo(undo, Err(e), !kept),
            }),
        ));
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
        std::fs::write(dir.join("s"), "staged\n").unwrap();
        sh(&dir, &["add", "s"]);
        let saved = save_worktree(&dir, &Backup::All).unwrap();
        sh(&dir, &["reset", "--hard", "-q"]);
        sh(&dir, &["clean", "-fdq"]);
        assert!(!dir.join("s").exists() && !dir.join("u").exists());
        restore_worktree(&dir, &saved).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("s")).unwrap(), "staged\n");
        assert_eq!(std::fs::read_to_string(dir.join("u")).unwrap(), "u\n");
        std::fs::remove_file(dir.join("s")).unwrap();

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
    /// A rebase stopped on a conflict keeps its entry, but undo waits until it's finished.
    #[test]
    fn undo_survives_new_tags_and_waits_for_stopped_rebases() {
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
        let feat_tip = sh(&dir, &["rev-parse", "feat"]);
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "a.txt", "main\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Rebase feat onto main", &["rebase", "main", "feat"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        assert_eq!(t.data.as_ref().unwrap().state, git::State::Rebasing);
        assert_eq!(t.undo_label(), Some("Rebase feat onto main"));
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(t.data.as_ref().unwrap().state, git::State::Rebasing);
        assert_eq!(t.undo_label(), Some("Rebase feat onto main"));
        assert!(t.notes.pop().is_some_and(|m| m.error));

        std::fs::write(dir.join("a.txt"), "both\n").unwrap();
        sh(&dir, &["add", "a.txt"]);
        t.apply(&ctx, Action::Git(Op::new("Continue rebase", &["-c", "core.editor=true", "rebase", "--continue"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        for _ in 0..2 {
            t.apply(&ctx, Action::Undo);
            settle(&ctx, &mut t);
        }
        assert_eq!(t.redo_label(), Some("Rebase feat onto main"));
        assert_eq!(sh(&dir, &["rev-parse", "feat"]), feat_tip);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_step_at_the_same_depth_has_a_new_id() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, View};
        let dir = scratch("undo-top");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let step = |t: &mut RepoTab, a: Action| {
            t.apply(&ctx, a);
            settle(&ctx, t);
            t.undo_top()
        };
        let first = step(&mut t, Action::Git(Op::new("Branch", &["branch", "x"]).undo(Undo::Keep)));
        assert!(first.is_some());
        assert_eq!(step(&mut t, Action::Undo), None);
        let second = step(&mut t, Action::Git(Op::new("Branch", &["branch", "y"]).undo(Undo::Keep)));
        assert!(second.is_some() && second != first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo of a discard refuses (and keeps its backup) when the path changed since.
    #[test]
    fn undo_discard_refuses_over_newer_edits() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, View};
        let dir = scratch("undo-newer");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let step = |t: &mut RepoTab, a: Action| {
            t.apply(&ctx, a);
            settle(&ctx, t)
        };
        let discard = |args: &[&str], p: &str| Action::Git(Op::new("Discard", args).light().backup(Backup::Paths(vec![p.into()])));
        let read = |p: &str| std::fs::read_to_string(dir.join(p)).unwrap();

        std::fs::write(dir.join("a"), "E1\n").unwrap();
        step(&mut t, discard(&["checkout", "--", "a"], "a"));
        std::fs::write(dir.join("a"), "E2\n").unwrap();
        let msgs = step(&mut t, Action::Undo);
        assert_eq!(read("a"), "E2\n");
        assert!(msgs.iter().any(|m| m.error && m.text.contains("changed since")) && t.undo_label() == Some("Discard"));
        std::fs::write(dir.join("a"), "base\n").unwrap();
        step(&mut t, Action::Undo);
        assert_eq!(read("a"), "E1\n");
        sh(&dir, &["checkout", "--", "a"]);

        std::fs::write(dir.join("notes.txt"), "A\n").unwrap();
        step(&mut t, discard(&["clean", "-fq", "--", "notes.txt"], "notes.txt"));
        std::fs::write(dir.join("notes.txt"), "B\n").unwrap();
        let msgs = step(&mut t, Action::Undo);
        assert_eq!(read("notes.txt"), "B\n");
        assert!(msgs.iter().any(|m| m.error && m.text.contains("changed since")) && t.undo_label() == Some("Discard"));
        std::fs::remove_file(dir.join("notes.txt")).unwrap();
        step(&mut t, Action::Undo);
        assert_eq!(read("notes.txt"), "A\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed ref undo drops ref snapshots only (and only once refs moved or changed outside):
    /// a force-deleted branch stays restorable.
    #[test]
    fn failed_ref_undo_keeps_op_entries() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, Undo, View};
        let dir = scratch("undo-keep-op");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "main\n");
        sh(&dir, &["checkout", "-q", "-b", "x"]);
        commit(&dir, "x", "only on x\n");
        let x = sh(&dir, &["rev-parse", "x"]).trim().to_owned();
        sh(&dir, &["checkout", "-q", "-b", "y", "main"]);
        commit(&dir, "a", "y\n");
        sh(&dir, &["checkout", "-q", "main"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let step = |t: &mut RepoTab, a: Action| {
            t.apply(&ctx, a);
            settle(&ctx, t);
        };
        step(&mut t, Action::Git(Op::new("Delete x", &["branch", "-D", "x"]).revert(Op::new("Restore x", &["branch", "x", &x]))));
        step(&mut t, Action::Git(Op::new("Checkout y", &["checkout", "-q", "y"]).undo(Undo::Keep)));
        // Refused before anything moved (a local edit in the way): the step stays.
        std::fs::write(dir.join("a"), "local edit\n").unwrap();
        step(&mut t, Action::Undo);
        assert_eq!(t.undo_label(), Some("Checkout y"));
        sh(&dir, &["checkout", "--", "a"]);
        sh(&dir, &["branch", "outside"]);
        step(&mut t, Action::Undo);
        assert_eq!(t.undo_label(), Some("Delete x"));
        step(&mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "x"]).trim(), x);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Thousands of long paths don't fit on a command line: they go through stdin.
    #[test]
    fn undo_discard_of_many_files() {
        use crate::repo::testutil::*;
        let dir = scratch("undo-many");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let sub = dir.join("d".repeat(190));
        std::fs::create_dir_all(&sub).unwrap();
        let write = |text: &str| (0..12000).for_each(|i| std::fs::write(sub.join(format!("f{i}")), text).unwrap());
        write("v1\n");
        sh(&dir, &["add", "-A"]);
        sh(&dir, &["commit", "-q", "-m", "init"]);
        write("v2\n");
        let saved = save_worktree(&dir, &Backup::All).unwrap();
        sh(&dir, &["reset", "--hard", "-q"]);
        restore_worktree(&dir, &saved).unwrap();
        assert_eq!(std::fs::read_to_string(sub.join("f11999")).unwrap(), "v2\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo replaces a symlink the restore created instead of writing through it.
    #[cfg(unix)]
    #[test]
    fn undo_restore_does_not_write_through_a_symlink() {
        use crate::repo::testutil::*;
        let dir = scratch("undo-symlink");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "real.env", "TRACKED=1\n");
        std::os::unix::fs::symlink("real.env", dir.join(".env")).unwrap();
        sh(&dir, &["add", ".env"]);
        sh(&dir, &["commit", "-q", "-m", "link"]);
        let c = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        sh(&dir, &["rm", "-q", "--cached", ".env"]);
        std::fs::write(dir.join(".gitignore"), ".env\n").unwrap();
        sh(&dir, &["add", ".gitignore"]);
        sh(&dir, &["commit", "-q", "-m", "ignore"]);
        std::fs::remove_file(dir.join(".env")).unwrap();
        std::fs::write(dir.join(".env"), "SECRET=mine\n").unwrap();

        let saved = save_worktree(&dir, &Backup::Overwrite(vec![".env".into()])).unwrap();
        sh(&dir, &["restore", "--source", &c, "--worktree", "--", ".env"]);
        assert!(std::fs::symlink_metadata(dir.join(".env")).unwrap().is_symlink());
        restore_worktree(&dir, &saved).unwrap();
        assert!(!std::fs::symlink_metadata(dir.join(".env")).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(dir.join(".env")).unwrap(), "SECRET=mine\n");
        assert_eq!(std::fs::read_to_string(dir.join("real.env")).unwrap(), "TRACKED=1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo never moves or deletes a branch another worktree has checked out.
    #[test]
    fn undo_leaves_other_worktrees_branches_alone() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, Undo, View};
        let dir = scratch("undo-worktree");
        let wt = scratch("undo-worktree-w");
        let _ = std::fs::remove_dir_all(&wt);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Create branch", &["branch", "x"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        sh(&dir, &["worktree", "add", "-q", wt.to_str().unwrap(), "x"]);
        t.apply(&ctx, Action::Undo);
        assert!(settle(&ctx, &mut t).iter().any(|m| m.error && m.text.contains("another worktree")));
        assert!(git::run(&dir, &["rev-parse", "--verify", "x"]).is_ok());
        // Nothing moved: the step stays for a retry once that worktree lets go.
        assert_eq!(t.undo_label(), Some("Create branch"));
        sh(&dir, &["worktree", "remove", wt.to_str().unwrap()]);
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert!(git::run(&dir, &["rev-parse", "--verify", "x"]).is_err());
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A name that isn't valid UTF-8 comes back from git with U+FFFD: refuse to discard it.
    #[test]
    fn discard_refuses_undecodable_names() {
        use crate::repo::testutil::*;
        let dir = scratch("undo-fffd");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a\n");
        std::fs::write(dir.join("caf\u{FFFD}.txt"), "x\n").unwrap();
        assert!(save_worktree(&dir, &Backup::All).err().is_some_and(|e| e.contains("UTF-8")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo of "Discard all" brings back the staged changes and the exact bytes.
    #[test]
    fn undo_discard_all_restores_index_and_line_endings() {
        use crate::repo::testutil::*;
        let dir = scratch("undo-index");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        sh(&dir, &["config", "core.autocrlf", "true"]);
        commit(&dir, "b", "one\n");
        commit(&dir, "c", "c0\n");
        std::fs::write(dir.join("b"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("c"), "c1\n").unwrap();
        sh(&dir, &["add", "c"]);
        std::fs::write(dir.join("c"), "c1\nc2\n").unwrap();
        std::fs::write(dir.join("new"), "n\n").unwrap();
        sh(&dir, &["add", "new"]);
        let before = sh(&dir, &["status", "--porcelain"]);
        let saved = save_worktree(&dir, &Backup::All).unwrap();
        sh(&dir, &["reset", "--hard", "-q"]);
        sh(&dir, &["clean", "-fdq"]);
        restore_worktree(&dir, &saved).unwrap();
        assert_eq!(std::fs::read(dir.join("b")).unwrap(), b"one\ntwo\n");
        assert_eq!(sh(&dir, &["status", "--porcelain"]), before);
        assert_eq!(sh(&dir, &["show", ":c"]).replace('\r', ""), "c1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ⌘Z on a discard after the file was edited again: refuses instead of overwriting the edit.
    #[test]
    fn undo_of_a_discard_keeps_edits_made_since() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, View};
        let dir = scratch("undo-discard-newer");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "base\n");
        std::fs::write(dir.join("f"), "first edit\n").unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Discard all", &["reset", "--hard", "-q"]).then(&["clean", "-fdq"]).backup(Backup::All)));
        settle(&ctx, &mut t);
        std::fs::write(dir.join("f"), "second edit\n").unwrap();
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "second edit\n");
        assert_eq!(t.undo_label(), Some("Discard all"));
        // Back to what the discard left: the undo goes through.
        std::fs::write(dir.join("f"), "base\n").unwrap();
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "first edit\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Changes that exist only in the index ("MM" back to HEAD, "AD") come back too.
    #[test]
    fn undo_discard_all_restores_index_only_changes() {
        use crate::repo::testutil::*;
        let dir = scratch("undo-index-only");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "v1\n");
        std::fs::write(dir.join("f"), "v2\n").unwrap();
        std::fs::write(dir.join("n"), "new\n").unwrap();
        sh(&dir, &["add", "f", "n"]);
        std::fs::write(dir.join("f"), "v1\n").unwrap();
        std::fs::remove_file(dir.join("n")).unwrap();
        let before = sh(&dir, &["status", "--porcelain"]);
        let mut saved = save_worktree(&dir, &Backup::All).unwrap();
        sh(&dir, &["reset", "--hard", "-q"]);
        saved.seal(&dir);
        restore_worktree(&dir, &saved).unwrap();
        assert_eq!(sh(&dir, &["status", "--porcelain"]), before);
        assert_eq!(sh(&dir, &["show", ":f"]), "v2\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An undo that failed halfway (here: a file where an untracked file's folder goes) can be retried.
    #[test]
    fn undo_discard_retries_after_a_half_applied_attempt() {
        use crate::repo::testutil::*;
        let dir = scratch("undo-half");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "b", "b0\n");
        commit(&dir, "c", "c0\n");
        std::fs::write(dir.join("b"), "b1\n").unwrap();
        std::fs::write(dir.join("c"), "c1\n").unwrap();
        sh(&dir, &["add", "c"]);
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/u"), "u\n").unwrap();
        let before = sh(&dir, &["status", "--porcelain", "-uall"]);
        let mut saved = save_worktree(&dir, &Backup::All).unwrap();
        sh(&dir, &["reset", "--hard", "-q"]);
        sh(&dir, &["clean", "-fdq"]);
        saved.seal(&dir);
        std::fs::write(dir.join("d"), "in the way\n").unwrap();
        assert!(restore_worktree(&dir, &saved.clone()).is_err());
        std::fs::remove_file(dir.join("d")).unwrap();
        restore_worktree(&dir, &saved).unwrap();
        assert_eq!(sh(&dir, &["status", "--porcelain", "-uall"]), before);
        assert_eq!(std::fs::read_to_string(dir.join("d/u")).unwrap(), "u\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A removal that fails partway reseals what it removed, so the retry isn't refused.
    #[cfg(unix)]
    #[test]
    fn undo_removal_retries_after_a_partial_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::repo::testutil::scratch("undo-rm-half");
        std::fs::create_dir(dir.join("z")).unwrap();
        std::fs::write(dir.join("a"), "a\n").unwrap();
        std::fs::write(dir.join("z/b"), "b\n").unwrap();
        let mut saved = Saved { stash: None, tracked: vec!["a".into(), "z/b".into()], staged: vec![], unmerged: vec![], files: vec![], overwrite: false, index: false, after: Default::default() };
        saved.seal(&dir);
        let mode = |m| std::fs::set_permissions(dir.join("z"), std::fs::Permissions::from_mode(m)).unwrap();
        mode(0o555);
        assert!(restore_worktree(&dir, &saved).is_err());
        mode(0o755);
        restore_worktree(&dir, &saved).unwrap();
        assert!(!dir.join("a").exists() && !dir.join("z/b").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The streamed fingerprint still sees a change past the first read chunk.
    #[test]
    fn fingerprint_covers_the_whole_file() {
        let dir = crate::repo::testutil::scratch("undo-fp");
        let (mut a, p) = (vec![b'x'; 200_000], dir.join("f"));
        std::fs::write(&p, &a).unwrap();
        let before = fingerprint(&p);
        a[150_000] = b'y';
        std::fs::write(&p, &a).unwrap();
        assert_ne!(fingerprint(&p), before);
        std::fs::remove_file(&p).unwrap();
        assert_eq!(fingerprint(&p), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo never moves a branch another worktree is rebasing (shown there as detached).
    #[test]
    fn undo_leaves_branches_rebased_elsewhere() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, View};
        let (dir, wt) = (scratch("undo-wt-rebase"), scratch("undo-wt-rebase-other"));
        let _ = std::fs::remove_dir_all(&wt);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "x"]);
        commit(&dir, "a", "x1\n");
        commit(&dir, "a", "x2\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "a", "main\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Move x", &["branch", "-f", "x", "x~1"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        let x1 = sh(&dir, &["rev-parse", "x"]);
        sh(&dir, &["worktree", "add", "-q", wt.to_str().unwrap(), "x"]);
        assert!(git::run(&wt, &["rebase", "main"]).is_err());
        t.apply(&ctx, Action::Undo);
        assert!(settle(&ctx, &mut t).iter().any(|m| m.error && m.text.contains("another worktree")));
        assert_eq!(sh(&dir, &["rev-parse", "x"]), x1);
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo never moves a branch that another worktree has checked out.
    #[test]
    fn undo_leaves_branches_checked_out_elsewhere() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, View};
        let (dir, wt) = (scratch("undo-wt"), scratch("undo-wt-other"));
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a");
        sh(&dir, &["branch", "feat"]);
        commit(&dir, "b", "b");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Move feat", &["branch", "-f", "feat", "main"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        sh(&dir, &["worktree", "add", "-q", wt.to_str().unwrap(), "feat"]);
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["rev-parse", "feat"]), sh(&dir, &["rev-parse", "main"]));
        assert_eq!(sh(&wt, &["status", "--porcelain"]), "");
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A branch another worktree is rebasing shows there as detached: still never move it,
    /// since finishing that rebase would overwrite what the undo restored.
    #[test]
    fn undo_leaves_branches_rebasing_elsewhere() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, View};
        let (dir, wt) = (scratch("undo-wt-rebasing"), scratch("undo-wt-rebasing-other"));
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a");
        sh(&dir, &["branch", "feat"]);
        commit(&dir, "b", "b");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Move feat", &["branch", "-f", "feat", "main"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        sh(&dir, &["worktree", "add", "-q", wt.to_str().unwrap(), "feat"]);
        assert!(git::run(&wt, &["rebase", "-q", "--exec", "false", "HEAD~1"]).is_err());
        t.apply(&ctx, Action::Undo);
        assert!(settle(&ctx, &mut t).iter().any(|m| m.error && m.text.contains("another worktree")));
        assert_eq!(sh(&dir, &["rev-parse", "feat"]), sh(&dir, &["rev-parse", "main"]));
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
