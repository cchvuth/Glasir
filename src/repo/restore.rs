//! Write one file's version from a commit or stash into the working tree
//! (restore from history, apply a file from a stash), backed up first so ⌘Z undoes it.

use super::{Backup, Op, RepoData};
use crate::git::{self, Res};
use std::path::Path;

pub const APPLY_STASH_FILE: &str = "!apply-stash-file";

/// Apply only the stash's own change to a tracked `path` (its diff against the stash's base), so
/// commits made since stay; like `git stash apply` for one file, but working tree only. Where the
/// file changed since, a 3-way merge into it (it may leave conflict markers); a file gone since
/// gets the stash's version.
pub fn apply_stash_file(d: &RepoData, label: String, stash: &str, path: &str) -> Op {
    let mut o = restore_file(d, label, stash, path, "the stash's change");
    o.cmds = vec![vec![APPLY_STASH_FILE.into(), stash.into(), path.into()]];
    o
}

pub(super) fn apply_stash(dir: &Path, stash: &str, path: &str) -> Res<String> {
    let patch = git::run_raw(dir, &["--literal-pathspecs", "diff", "--no-color", "--no-ext-diff", "--binary", &format!("{stash}^1"), stash, "--", path], None)?;
    let has = |l: &str| patch.split(|&b| b == b'\n').any(|x| x.starts_with(l.as_bytes()));
    let (deletes, adds) = (has("deleted file mode"), has("new file mode"));
    let blob = |rev: &str| git::run_raw(dir, &["cat-file", "--filters", &format!("{rev}:{path}")], None);
    let base = if adds && !deletes { vec![] } else { blob(&format!("{stash}^1"))? };
    let file = dir.join(path);
    let linked = git::ancestors(path).any(|a| std::fs::symlink_metadata(dir.join(a)).is_ok_and(|m| m.is_symlink()));
    let edited = !linked && has("@@ ") && std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file()) && std::fs::read(&file).is_ok_and(|now| now != base);
    if !edited {
        let Err(e) = git::run_input(dir, &["apply", "--whitespace=nowarn"], Some(&patch)) else { return Ok(String::new()) };
        match std::fs::symlink_metadata(&file) {
            _ if linked => return Err(e),
            Err(_) if deletes && !adds => return Ok(String::new()),
            Err(_) => return git::run(dir, &["--literal-pathspecs", "restore", "--source", stash, "--worktree", "--", path]),
            Ok(m) if !m.is_file() && !(deletes && adds) => return Err(e),
            Ok(_) => {}
        }
    }
    if deletes && adds {
        return Err(format!("The stash replaces {path} with another kind of file (such as a symlink), and it changed since, so it was left as it is."));
    }
    if deletes {
        return Err(format!("The stash deletes {path}, which changed since, so it was left as it is."));
    }
    if has("GIT binary patch") {
        return Err(format!("{path} is binary and changed since the stash, so the stash's change can't be merged into it; it was left as it is."));
    }
    let ours = std::fs::read(&file).map_err(|e| format!("{path}: {e}"))?;
    let theirs = blob(stash)?;
    let name = git::stashes(dir).ok().and_then(|ss| ss.into_iter().find(|s| s.id == stash)).map_or("stash".into(), |s| format!("{}: {}", s.name, s.message));
    let merged = git::scratch(dir, |b| {
        git::scratch(dir, |t| {
            std::fs::write(b, &base).and_then(|()| std::fs::write(t, &theirs)).map_err(|e| e.to_string())?;
            git::run(dir, &["merge-file", "-L", "working tree", "-L", "stash base", "-L", &name, "--", path, &b.to_string_lossy(), &t.to_string_lossy()])
        })
    });
    match merged {
        Err(_) if std::fs::read(&file).is_ok_and(|now| now != ours) => {
            Err(format!("{path} changed since the stash, so its change was merged in with conflict markers (<<<<<<< working tree … >>>>>>> {name}): resolve them in the file."))
        }
        r => r,
    }
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
        commit(&dir, "a", "1\n2\n3\n4\n5\n");
        std::fs::write(dir.join("a"), "1 stash\n2\n3\n4\n5\n").unwrap();
        sh(&dir, &["stash", "push"]);
        commit(&dir, "a", "1\n2\n3\n4\n5 later\n");
        let id = sh(&dir, &["rev-parse", "stash@{0}"]).trim().to_owned();
        let d = load_all(&dir, 100, View::default()).unwrap();
        exec(&dir, &super::apply_stash_file(&d, "Apply".into(), &id, "a")).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "1 stash\n2\n3\n4\n5 later\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stash_file_lands_by_its_context_or_a_merge() {
        let dir = scratch("stashfile-moved");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        sh(&dir, &["config", "color.diff", "always"]);
        sh(&dir, &["config", "diff.external", "echo"]);
        let lines = |r: std::ops::RangeInclusive<u32>| r.map(|i| format!("{i}\n")).collect::<String>();
        let top = "n1\nn2\nn3\n";
        commit(&dir, "a", &lines(1..=10));
        commit(&dir, "b", "x\ny\nx\n");
        std::fs::write(dir.join("a"), lines(1..=5) + "STASHED\n" + &lines(6..=10)).unwrap();
        std::fs::write(dir.join("b"), "x\ny\nX\n").unwrap();
        sh(&dir, &["stash", "push", "-q"]);
        commit(&dir, "a", &(top.to_owned() + &lines(1..=10)));
        commit(&dir, "b", "n1\nn2\nx\ny\nx\n");
        let id = sh(&dir, &["rev-parse", "stash@{0}"]).trim().to_owned();
        super::apply_stash(&dir, &id, "a").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), top.to_owned() + &lines(1..=5) + "STASHED\n" + &lines(6..=10));
        super::apply_stash(&dir, &id, "b").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("b")).unwrap(), "n1\nn2\nx\ny\nX\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stash_file_merges_over_edited_context() {
        let dir = scratch("stashfile-merge");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "1\n2\n3\n4\n5\n6\n7\n");
        std::fs::write(dir.join("a"), "1\n2\n3\n4 stash\n5\n6\n7\n").unwrap();
        std::fs::write(dir.join("n"), "new\n").unwrap();
        sh(&dir, &["add", "n"]);
        sh(&dir, &["stash", "push", "-q"]);
        let id = sh(&dir, &["rev-parse", "stash@{0}"]).trim().to_owned();
        let read = |p: &str| std::fs::read_to_string(dir.join(p)).unwrap();
        std::fs::write(dir.join("a"), "1\n2 mine\n3\n4\n5\n6 mine\n7\n").unwrap();
        super::apply_stash(&dir, &id, "a").unwrap();
        assert_eq!(read("a"), "1\n2 mine\n3\n4 stash\n5\n6 mine\n7\n");
        std::fs::remove_file(dir.join("a")).unwrap();
        let o = super::apply_stash_file(&load_all(&dir, 100, View::default()).unwrap(), "Apply".into(), &id, "a");
        let saved = run_backed(&dir, &o).unwrap().unwrap();
        assert_eq!(read("a"), "1\n2\n3\n4 stash\n5\n6\n7\n");
        undo_files(&dir, &saved).unwrap();
        assert!(!dir.join("a").exists());
        std::fs::write(dir.join("n"), "new\n").unwrap();
        super::apply_stash(&dir, &id, "n").unwrap();
        assert_eq!(read("n"), "new\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_conflicting_stash_file_gets_markers_and_undoes() {
        use crate::repo::testutil::settle;
        use crate::repo::{Action, RepoTab};
        let dir = scratch("stashfile-conflict");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "1\n2\n3\n");
        std::fs::write(dir.join("a"), "1\n2 stash\n3\n").unwrap();
        sh(&dir, &["stash", "push", "-q", "-m", "wip"]);
        let id = sh(&dir, &["rev-parse", "stash@{0}"]).trim().to_owned();
        std::fs::write(dir.join("a"), "1\n2 mine\n3\n").unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let op = super::apply_stash_file(t.data.as_ref().unwrap(), "Apply a from stash".into(), &id, "a");
        t.apply(&ctx, Action::Git(op));
        let msgs = settle(&ctx, &mut t);
        let read = || std::fs::read_to_string(dir.join("a")).unwrap();
        assert!(msgs.iter().any(|m| m.error && m.text.contains("conflict markers")), "{:?}", msgs.iter().map(|m| &m.text).collect::<Vec<_>>());
        assert!(read().starts_with("1\n<<<<<<< working tree\n2 mine\n") && read().ends_with("=======\n2 stash\n>>>>>>> stash@{0}: On main: wip\n3\n"), "{}", read());
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(read(), "1\n2 mine\n3\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_binary_or_deleted_stash_file_is_left_alone() {
        let dir = scratch("stashfile-refused");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        std::fs::write(dir.join("bin"), "\0one\n").unwrap();
        sh(&dir, &["add", "bin"]);
        commit(&dir, "d", "1\n");
        std::fs::write(dir.join("bin"), "\0stash\n").unwrap();
        std::fs::remove_file(dir.join("d")).unwrap();
        sh(&dir, &["stash", "push", "-q"]);
        let id = sh(&dir, &["rev-parse", "stash@{0}"]).trim().to_owned();
        std::fs::write(dir.join("bin"), "\0mine\n").unwrap();
        std::fs::write(dir.join("d"), "2\n").unwrap();
        assert!(super::apply_stash(&dir, &id, "bin").is_err_and(|e| e.contains("binary")));
        assert_eq!(std::fs::read(dir.join("bin")).unwrap(), b"\0mine\n");
        assert!(super::apply_stash(&dir, &id, "d").is_err_and(|e| e.contains("deletes")));
        assert_eq!(std::fs::read_to_string(dir.join("d")).unwrap(), "2\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stash_file_change_stays_in_its_own_block() {
        let dir = scratch("stashfile-twin");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let block = |b: &str, d: &str| format!("a\n{b}\nc\n{d}\ne\nf\ng\n");
        let file = |first: String| format!("top\n{first}{}{}end\n", "gap\n".repeat(10), block("b", "d"));
        commit(&dir, "f", &file(block("b", "d")));
        std::fs::write(dir.join("f"), file(block("b", "D"))).unwrap();
        sh(&dir, &["stash", "push", "-q"]);
        commit(&dir, "f", &file(block("B", "d")));
        let id = sh(&dir, &["rev-parse", "stash@{0}"]).trim().to_owned();
        super::apply_stash(&dir, &id, "f").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), file(block("B", "D")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_stash_file_beyond_a_symlinked_folder_is_refused() {
        let root = scratch("stashfile-beyond");
        let (dir, outside) = (root.join("r"), root.join("outside"));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "sub/a", "1\n2\n3\n");
        std::fs::write(dir.join("sub/a"), "1\n2 stash\n3\n").unwrap();
        sh(&dir, &["stash", "push", "-q"]);
        let id = sh(&dir, &["rev-parse", "stash@{0}"]).trim().to_owned();
        std::fs::remove_dir_all(dir.join("sub")).unwrap();
        std::os::unix::fs::symlink("../outside", dir.join("sub")).unwrap();
        std::fs::write(outside.join("a"), "1\n2 mine\n3\n").unwrap();
        assert!(super::apply_stash(&dir, &id, "sub/a").is_err());
        assert_eq!(std::fs::read_to_string(outside.join("a")).unwrap(), "1\n2 mine\n3\n");
        std::fs::remove_file(outside.join("a")).unwrap();
        assert!(super::apply_stash(&dir, &id, "sub/a").is_err());
        assert!(std::fs::symlink_metadata(dir.join("sub")).unwrap().is_symlink() && !outside.join("a").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_stash_file_turned_symlink_is_restored_or_refused() {
        let dir = scratch("stashfile-typechange");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "l", "x\n");
        std::fs::remove_file(dir.join("l")).unwrap();
        std::os::unix::fs::symlink("target", dir.join("l")).unwrap();
        sh(&dir, &["stash", "push", "-q"]);
        let id = sh(&dir, &["rev-parse", "stash@{0}"]).trim().to_owned();
        std::fs::write(dir.join("l"), "y\n").unwrap();
        assert!(super::apply_stash(&dir, &id, "l").is_err_and(|e| e.contains("another kind of file")));
        assert_eq!(std::fs::read_to_string(dir.join("l")).unwrap(), "y\n");
        std::fs::remove_file(dir.join("l")).unwrap();
        super::apply_stash(&dir, &id, "l").unwrap();
        assert_eq!(std::fs::read_link(dir.join("l")).unwrap(), std::path::Path::new("target"));
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
