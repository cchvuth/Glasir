//! "Oops, I committed on main": move the current branch's unpushed commits to
//! a new branch and put the current branch back on its upstream.

use super::{Action, Op, RepoData, Undo, prompt};

/// Asks for the new branch's name; `None` when nothing is unpushed. The new
/// branch is created at HEAD and checked out (so local changes stay as they
/// are), then the old branch is moved to its upstream. One undo step.
pub fn move_unpushed(d: &RepoData) -> Option<(String, Action)> {
    let (branch, up, n) = (d.status.branch.as_deref()?, d.status.upstream.as_deref()?, d.status.ahead);
    if n == 0 {
        return None;
    }
    let what = format!("Move {n} unpushed commit{} to a new branch…", if n == 1 { "" } else { "s" });
    let op = Op::new(format!("Move {n} commits off {branch}"), &["checkout", "-b", "{}"]).then(&["branch", "-f", branch, up]).undo(Undo::Keep);
    Some((what, prompt(format!("New branch for {branch}'s {n} unpushed commit{}", if n == 1 { "" } else { "s" }), "", op)))
}

#[cfg(test)]
mod tests {
    use crate::git;
    use crate::repo::op::exec;
    use crate::repo::testutil::{commit, identity, scratch, sh};
    use crate::repo::{Action, View, data::load_all};

    #[test]
    fn moves_unpushed_commits_to_a_new_branch() {
        let dir = scratch("spinoff");
        let remote = scratch("spinoff-remote");
        sh(&remote, &["init", "-q", "--bare", "-b", "main"]);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "pushed\n");
        sh(&dir, &["remote", "add", "origin", &remote.to_string_lossy()]);
        sh(&dir, &["push", "-q", "-u", "origin", "main"]);
        assert!(super::move_unpushed(&load_all(&dir, 100, View::default()).unwrap()).is_none());
        commit(&dir, "b.txt", "oops 1\n");
        commit(&dir, "c.txt", "oops 2\n");
        std::fs::write(dir.join("a.txt"), "local edit\n").unwrap();
        std::fs::write(dir.join("b.txt"), "oops 1, edited\n").unwrap(); // a file the moved commits touch
        let tip = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();

        let d = load_all(&dir, 100, View::default()).unwrap();
        let (label, Action::Prompt(p)) = super::move_unpushed(&d).unwrap() else { panic!() };
        assert_eq!(label, "Move 2 unpushed commits to a new branch…");
        let mut op = p.op;
        op.cmds.iter_mut().flatten().for_each(|a| *a = a.replace("{}", "feature"));
        let before = git::snapshot(&dir).unwrap();
        exec(&dir, &op).unwrap();
        let rev = |r: &str| sh(&dir, &["rev-parse", r]).trim().to_owned();
        assert_eq!((rev("feature"), rev("main")), (tip.clone(), rev("origin/main")));
        assert_eq!(sh(&dir, &["symbolic-ref", "--short", "HEAD"]).trim(), "feature");
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "local edit\n");

        // Undo puts everything back: main at its old tip, checked out, no new branch.
        git::restore(&dir, &before, "--keep").unwrap();
        assert_eq!(git::snapshot(&dir).unwrap(), before);
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "local edit\n");
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "oops 1, edited\n");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&remote);
    }
}
