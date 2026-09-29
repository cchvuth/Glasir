//! Write one file's version from a commit or stash into the working tree
//! (restore from history, apply a file from a stash), backed up first so ⌘Z undoes it.

use super::{Backup, Op, RepoData};
use crate::git::{self, Res};
use std::path::Path;

pub const APPLY_STASH_FILE: &str = "!apply-stash-file";

/// Apply only the stash's own change to a tracked `path` (its diff against the stash's base), so
/// commits made since stay; like `git stash apply` for one file, but working tree only.
pub fn apply_stash_file(d: &RepoData, label: String, stash: &str, path: &str) -> Op {
    let mut o = restore_file(d, label, stash, path, "");
    o.cmds = vec![vec![APPLY_STASH_FILE.into(), stash.into(), path.into()]];
    o
}

pub(super) fn apply_stash(dir: &Path, stash: &str, path: &str) -> Res<String> {
    let patch = git::run(dir, &["--literal-pathspecs", "diff", "--binary", "-U0", &format!("{stash}^1"), stash, "--", path])?;
    git::run_input(dir, &["apply", "--unidiff-zero", "--whitespace=nowarn"], Some(patch.as_bytes()))
}

/// Overwrite `path` in the working tree with its version in `source`; `what`
/// names that version for the confirmation (e.g. "the stash's version").
pub fn restore_file(d: &RepoData, label: String, source: &str, path: &str, what: &str) -> Op {
    let o = Op::new(label, &["--literal-pathspecs", "restore", "--source", source, "--worktree", "--", path]).light();
    let s = &d.status;
    let untracked = s.unstaged.iter().any(|x| x.path == path && x.code == '?');
    if untracked || s.conflicted.iter().any(|x| x.path == path) {
        // An untracked or half-resolved file: undo can't bring it back, so ask.
        o.confirm(format!("Overwrite your local {path} with {what}? This can't be undone."))
    } else {
        o.backup(Backup::Paths(vec![path.to_owned()]))
    }
}

#[cfg(test)]
mod tests {
    use crate::repo::op::exec;
    use crate::repo::testutil::{run_backed, undo_files};
    use crate::repo::testutil::{commit, identity, scratch, sh};
    use crate::repo::{View, data::load_all};

    #[test]
    fn applying_a_stash_file_keeps_later_commits() {
        let dir = scratch("stashfile");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "1\n2\n3\n");
        std::fs::write(dir.join("a"), "1 stash\n2\n3\n").unwrap();
        sh(&dir, &["stash", "push"]);
        commit(&dir, "a", "1\n2\n3 later\n");
        let id = sh(&dir, &["rev-parse", "stash@{0}"]).trim().to_owned();
        let d = load_all(&dir, 100, View::default()).unwrap();
        exec(&dir, &super::apply_stash_file(&d, "Apply".into(), &id, "a")).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "1 stash\n2\n3 later\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restores_a_file_from_a_commit_and_its_parent() {
        let dir = scratch("restore");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "v1\n");
        commit(&dir, "a.txt", "v2\n");
        let c2 = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        commit(&dir, "a.txt", "v3\n");
        std::fs::write(dir.join("new.txt"), "mine\n").unwrap();
        let d = load_all(&dir, 100, View::default()).unwrap();
        let read = || std::fs::read_to_string(dir.join("a.txt")).unwrap();

        // A clean tracked file is backed up (undoable); an untracked one asks first.
        let o = super::restore_file(&d, "Restore".into(), &c2, "a.txt", "v2");
        assert!(o.backup.is_some() && o.confirm.is_none());
        assert!(super::restore_file(&d, "Restore".into(), &c2, "new.txt", "v2").confirm.is_some());
        let saved = run_backed(&dir, &o).unwrap().unwrap();
        assert_eq!(read(), "v2\n");
        undo_files(&dir, &saved).unwrap();
        assert_eq!(read(), "v3\n");
        exec(&dir, &o).unwrap();
        exec(&dir, &super::restore_file(&d, "Restore".into(), &format!("{c2}^"), "a.txt", "v1")).unwrap();
        assert_eq!(read(), "v1\n");
        // Only the working tree changes: nothing staged, HEAD untouched.
        assert_eq!(sh(&dir, &["diff", "--cached", "--name-only"]), "");
        assert_eq!(sh(&dir, &["show", "HEAD:a.txt"]), "v3\n");

        // A file deleted since: nothing local to back up, the restore still goes through.
        sh(&dir, &["rm", "-qf", "a.txt"]);
        sh(&dir, &["commit", "-q", "-m", "rm"]);
        let o = super::restore_file(&d, "Restore".into(), &c2, "a.txt", "v2");
        run_backed(&dir, &o).unwrap();
        assert_eq!(read(), "v2\n");

        // An ignored local file (once tracked) isn't in status; undo still puts it back.
        commit(&dir, ".env", "OLD=1\n");
        let c_env = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        sh(&dir, &["rm", "-q", "--cached", ".env"]);
        std::fs::write(dir.join(".gitignore"), ".env\n").unwrap();
        sh(&dir, &["add", ".gitignore"]);
        sh(&dir, &["commit", "-q", "-m", "ignore .env"]);
        std::fs::write(dir.join(".env"), "SECRET=mine\n").unwrap();
        let o = super::restore_file(&load_all(&dir, 100, View::default()).unwrap(), "Restore".into(), &c_env, ".env", "v1");
        let saved = run_backed(&dir, &o).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(".env")).unwrap(), "OLD=1\n");
        undo_files(&dir, &saved).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(".env")).unwrap(), "SECRET=mine\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
