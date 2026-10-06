//! Merge and cherry-pick variants. Squash merge and cherry-pick without committing
//! leave their result staged, so they only run on a clean tree (their undo puts back
//! the files they changed, like any other).

use super::op::{Op, Undo};
use crate::git::{self, Res};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// `exec` pseudo-command: fails unless nothing is uncommitted and no operation is in progress.
pub(super) const CLEAN: &str = "!tree-clean";
/// `exec` pseudo-command: `PICK_NEW id...` fails if a commit's change is already in HEAD.
pub(super) const PICK_NEW: &str = "!pick-new";
/// How [`pick_new`]'s refusal starts.
pub(super) const ALREADY: &str = "Already in the current branch:";

/// Settings → "Add '(cherry picked from …)'", mirrored every frame for the menus.
static PICK_X: AtomicBool = AtomicBool::new(false);

pub fn pick_x() -> bool {
    PICK_X.load(Ordering::Relaxed)
}

pub fn set_pick_x(on: bool) {
    PICK_X.store(on, Ordering::Relaxed);
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Merge {
    Squash,
    NoFf,
    FfOnly,
}

impl Merge {
    pub const ALL: [Merge; 3] = [Merge::Squash, Merge::NoFf, Merge::FfOnly];

    pub fn name(self) -> &'static str {
        match self {
            Merge::Squash => "squash",
            Merge::NoFf => "no fast-forward",
            Merge::FfOnly => "fast-forward only",
        }
    }
}

/// Merge `rev` (named `target`) into the current branch; `None` = git's default.
pub fn merge_op(target: &str, rev: &str, how: Option<Merge>) -> Op {
    let mut op = match how {
        None => Op::new(format!("Merge {target}"), &["merge", "--no-edit", rev]),
        Some(Merge::NoFf) => Op::new(format!("Merge {target} (no fast-forward)"), &["merge", "--no-ff", "--no-edit", rev]),
        Some(Merge::FfOnly) => Op::new(format!("Merge {target} (fast-forward only)"), &["merge", "--ff-only", rev]),
        Some(Merge::Squash) => {
            let mut op = Op::new(format!("Squash merge {target}"), &["merge", "--squash", rev]);
            op.cmds.insert(0, vec![CLEAN.into()]);
            op
        }
    };
    op.undo = Undo::Keep;
    op
}

/// Cherry-pick `ids` (oldest first). `merge`: one merge commit, picked against parent 1.
pub fn pick_op(label: impl Into<String>, ids: &[&str], merge: bool, no_commit: bool) -> Op {
    let mut a = vec!["cherry-pick"];
    if no_commit {
        a.push("-n");
    } else if PICK_X.load(Ordering::Relaxed) {
        a.push("-x");
    }
    if merge {
        a.extend(["-m", "1"]);
    }
    a.extend(ids);
    let mut op = Op::new(label, &a).undo(Undo::Keep);
    if no_commit {
        op.cmds.insert(0, vec![CLEAN.into()]);
    }
    if !merge {
        op.cmds.insert(0, std::iter::once(PICK_NEW).chain(ids.iter().copied()).map(String::from).collect());
    }
    op
}

pub(super) fn clean(dir: &Path) -> Res<String> {
    let busy = !matches!(git::state(&git::git_dir(dir)?), git::State::Clean);
    if busy || !git::run(dir, &["status", "--porcelain", "-z", "--untracked-files=no"])?.trim().is_empty() {
        return Err("Commit, stash or discard your uncommitted changes first (and finish any merge or rebase in progress): this leaves its result uncommitted, and undoing it must not touch other work.".into());
    }
    Ok(String::new())
}

pub(super) fn pick_new(dir: &Path, a: &[&str]) -> Res<String> {
    let mut seen = vec![];
    for id in &a[1..] {
        let parent = format!("{id}^");
        if git::run(dir, &["rev-parse", "-q", "--verify", &parent]).is_err() {
            continue; // a root commit
        }
        // `-` = a commit with the same patch is in HEAD; nothing at all = already reachable from HEAD.
        let Ok(out) = git::run(dir, &["cherry", "HEAD", id, &parent]) else { continue };
        if out.trim().is_empty() || out.trim_start().starts_with('-') {
            let line = git::run(dir, &["show", "-s", "--format=%h %s", id]).unwrap_or_default();
            seen.push(format!("  •  {}{}", line.trim(), if out.trim().is_empty() { " (already in its history)" } else { "" }));
        }
    }
    match seen.is_empty() {
        true => Ok(String::new()),
        false => Err(format!("{ALREADY}\n{}", seen.join("\n"))),
    }
}

/// The squash merge's prepared message as (summary, body).
pub(super) fn squash_message(git_dir: &Path) -> Option<(String, String)> {
    let msg = std::fs::read_to_string(git_dir.join("SQUASH_MSG")).ok()?;
    let (head, body) = msg.trim().split_once('\n').unwrap_or((msg.trim(), ""));
    Some((head.trim().to_owned(), body.trim().to_owned())).filter(|(h, _)| !h.is_empty())
}

#[cfg(test)]
mod tests {
    use super::super::op::exec;
    use super::super::testutil::{commit, identity, scratch, settle, sh};
    use super::super::{Action, RepoTab, View};
    use super::*;

    /// main: base, then feat: two commits touching other files.
    fn repo(name: &str) -> std::path::PathBuf {
        let dir = scratch(name);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "base", "b\n");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "one", "1\n");
        commit(&dir, "two", "2\n");
        sh(&dir, &["checkout", "-q", "main"]);
        dir
    }

    #[test]
    fn merge_variants() {
        let dir = repo("merge-variants");
        // fast-forward only works here; --no-ff makes a merge commit
        exec(&dir, &merge_op("feat", "feat", Some(Merge::NoFf))).unwrap();
        assert_eq!(sh(&dir, &["rev-list", "--parents", "-1", "HEAD"]).split_whitespace().count(), 3);
        sh(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
        commit(&dir, "diverge", "d\n");
        let e = exec(&dir, &merge_op("feat", "feat", Some(Merge::FfOnly))).unwrap_err();
        assert!(e.contains("fast-forward"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn squash_merge_stages_and_undoes() {
        let dir = repo("squash-undo");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let head = sh(&dir, &["rev-parse", "HEAD"]);
        t.apply(&ctx, Action::Git(merge_op("feat", "feat", Some(Merge::Squash))));
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), head);
        assert_eq!(sh(&dir, &["diff", "--cached", "--name-only"]).lines().count(), 2);
        // the commit box is prefilled from SQUASH_MSG
        assert!(t.summary.contains("Squashed commit"), "{}", t.summary);
        assert!(t.body.contains(sh(&dir, &["rev-parse", "feat"]).trim()), "{}", t.body);
        assert_eq!(t.undo_label(), Some("Squash merge feat"));
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert!(sh(&dir, &["status", "--porcelain"]).trim().is_empty());
        assert!(!dir.join("one").exists());
        t.apply(&ctx, Action::Redo);
        settle(&ctx, &mut t);
        assert!(dir.join("two").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Both refuse a dirty tree; an edit made after the op stays through its undo (the
    /// backup of that file goes to the stash list), as does a file where it deleted one.
    #[test]
    fn staged_ops_refuse_dirty_tree_and_undo_keeps_later_edits() {
        let dir = repo("staged-refuse");
        std::fs::write(dir.join("base"), "edited\n").unwrap();
        let e = exec(&dir, &merge_op("feat", "feat", Some(Merge::Squash))).unwrap_err();
        assert!(e.contains("uncommitted"), "{e}");
        assert!(sh(&dir, &["status", "--porcelain"]).contains("base"));
        sh(&dir, &["checkout", "-q", "--", "base"]);
        sh(&dir, &["checkout", "-q", "feat"]);
        sh(&dir, &["rm", "-q", "one"]);
        sh(&dir, &["commit", "-q", "-m", "rm one"]);
        sh(&dir, &["checkout", "-q", "main"]);
        sh(&dir, &["merge", "-q", "feat~2"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        for op in [pick_op("Cherry-pick", &["feat~1"], false, true), merge_op("feat", "feat", Some(Merge::Squash))] {
            t.apply(&ctx, Action::Git(op));
            settle(&ctx, &mut t);
            let (p, mine) = if dir.join("one").exists() { ("two", "mine\n") } else { ("one", "precious\n") };
            std::fs::write(dir.join(p), mine).unwrap();
            t.apply(&ctx, Action::Undo);
            let msgs = settle(&ctx, &mut t);
            assert_eq!(std::fs::read_to_string(dir.join(p)).unwrap(), mine);
            assert!(msgs.iter().any(|n| n.text.contains("stash list")), "{p}");
            let _ = std::fs::remove_file(dir.join(p));
            sh(&dir, &["reset", "-q", "--hard"]);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cherry_pick_flags_applied_commits_and_adds_x() {
        let dir = repo("pick-new");
        // an equivalent commit already on main (different parent: patch-equivalent only)
        commit(&dir, "one", "1\n");
        let one = sh(&dir, &["rev-parse", "feat~1"]);
        let one = one.trim();
        let e = exec(&dir, &pick_op("Cherry-pick", &[one], false, false)).unwrap_err();
        assert!(e.contains("Already in the current branch") && e.contains(" 1"), "{e}");
        let two = sh(&dir, &["rev-parse", "feat"]);
        exec(&dir, &pick_op("Cherry-pick", &[two.trim()], false, false)).unwrap();
        // an ancestor of HEAD is flagged too
        let e = exec(&dir, &pick_op("Cherry-pick", &["HEAD~1"], false, false)).unwrap_err();
        assert!(e.contains("already in its history"), "{e}");
        set_pick_x(true);
        let x = pick_op("Cherry-pick", &["feat"], false, false);
        set_pick_x(false);
        assert!(x.cmds[1].contains(&"-x".to_string()));
        assert!(pick_op("p", &["feat"], true, false).cmds[0].contains(&"-m".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cherry_pick_no_commit_leaves_staged() {
        let dir = repo("pick-nc");
        let two = sh(&dir, &["rev-parse", "feat"]);
        let head = sh(&dir, &["rev-parse", "HEAD"]);
        let op = pick_op("Cherry-pick", &[two.trim()], false, true);
        exec(&dir, &op).unwrap();
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), head);
        assert_eq!(sh(&dir, &["diff", "--cached", "--name-only"]).trim(), "two");
        assert_eq!(op.undo, Undo::Keep);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
