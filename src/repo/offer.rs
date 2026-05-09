//! One-click ways out of common failures (blocked checkout, diverged pull,
//! rejected push), offered as a `Choice`.

use super::action::Choice;
use super::data::RepoData;
use super::op::Op;

/// When `op` failed with `err` in a way that has a one-click fix, the choices to offer.
pub(super) fn offer(op: &Op, err: &str, d: Option<&RepoData>) -> Option<Choice> {
    use super::op::{PullMode, pull_op};
    let args = op.cmds.first()?;
    let has = |a: &str| args.iter().any(|x| x == a);
    let pulls = || {
        let mut merge = pull_op(PullMode::Merge);
        merge.label = "Pull (merge)".into();
        vec![("Pull (rebase)".to_owned(), pull_op(PullMode::Rebase)), ("Pull (merge)".to_owned(), merge)]
    };
    match args.first()?.as_str() {
        "checkout" if err.contains("would be overwritten by checkout") => {
            let target = op.label.strip_prefix("Checkout ").unwrap_or(&op.label);
            let files: Vec<String> = err.lines().filter(|l| l.starts_with('\t')).map(|l| format!("  •  {}", l.trim())).collect();
            let mut o = op.clone();
            o.label = format!("{} (autostash)", op.label);
            o.autostash = Some(format!("autostash before switching to {target}"));
            Some(Choice {
                title: "Uncommitted changes in the way".into(),
                text: format!(
                    "Switching to {target} would overwrite your uncommitted changes in:\n{}\n\nGlasir can stash them, switch, and reapply them there.",
                    files.join("\n")
                ),
                ops: vec![("Stash, switch and reapply".into(), o)],
            })
        }
        "pull" if has("--ff-only") && err.contains("Not possible to fast-forward") => Some(Choice {
            title: "Branches have diverged".into(),
            text: "Your branch and its upstream both have new commits, so a fast-forward isn't possible. Combine them by rebasing your commits on top, or with a merge commit.".into(),
            ops: pulls(),
        }),
        // Only the toolbar push (current branch, `--progress`): pulling fixes nothing for another branch.
        "push" if err.contains("[rejected]") && has("--progress") && !args.iter().any(|a| a.starts_with("--force")) => {
            let mut ops = pulls();
            // A force push leases on the remote commits we've seen: only possible once they're fetched.
            let text = if err.contains("(fetch first)") {
                ops.push(("Fetch".to_owned(), super::op::fetch_op()));
                "The remote branch has commits you haven't fetched. Pull them in first, then push again — or fetch to see them, and push again to get the option to replace them."
            } else {
                ops.extend(d.and_then(|d| d.push_op(true)).map(|o| ("Force push…".to_owned(), o)));
                "The remote branch has commits that aren't in yours. Pull them in first, then push again — or force push to replace them."
            };
            Some(Choice { title: "Push rejected".into(), text: text.into(), ops })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git;
    use crate::repo::data::{View, load_all};
    use crate::repo::op::{Undo, exec};
    use crate::repo::testutil::{commit, identity, scratch, sh};
    use crate::repo::undo::move_refs;

    #[test]
    fn push_pull_and_lease_against_bare_remote() {
        let root = scratch("net");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        for c in ["a", "b"] {
            sh(&root, &["clone", "-q", "remote.git", c]);
            identity(&root.join(c));
        }
        let (a, b) = (root.join("a"), root.join("b"));
        commit(&a, "f.txt", "one\n");

        // --progress output is streamed while the push runs.
        let push = load_all(&a, 100, View::default()).unwrap().push_op(false).unwrap();
        let cancel = git::Cancel::default();
        cancel.scope(|| exec(&a, &push)).unwrap();
        assert!(!cancel.progress().is_empty());
        let log = git::LOG.lock().unwrap().iter().rev().find(|l| l.cwd == a && l.args[0] == "push").unwrap().stderr.clone();
        assert!(log.contains("Writing objects: 100%") && !log.contains('\r'), "{log:?}");

        // Diverge: b pushes, a commits locally.
        sh(&b, &["pull", "-q"]);
        commit(&b, "b.txt", "b1\n");
        sh(&b, &["push", "-q"]);
        commit(&a, "a.txt", "a1\n");

        // A plain push is rejected. Unfetched remote commits: a lease can't name them yet, so fetch first.
        let labels = |c: Choice| c.ops.into_iter().map(|o| o.0).collect::<Vec<_>>();
        let rejected = || {
            let d = load_all(&a, 100, View::default()).unwrap();
            let err = exec(&a, &d.push_op(false).unwrap()).unwrap_err();
            let c = offer(&d.push_op(false).unwrap(), &err, Some(&d)).expect("rejected push offers a way out");
            (d, labels(c))
        };
        assert_eq!(rejected().1, ["Pull (rebase)", "Pull (merge)", "Fetch"]);

        let ff = crate::repo::op::pull_op(crate::repo::op::PullMode::FfOnly);
        let err = exec(&a, &ff).unwrap_err();
        let c = offer(&ff, &err, None).expect("diverged pull offers rebase/merge");
        assert_eq!(labels(c), ["Pull (rebase)", "Pull (merge)"]);

        // Fetched now: offer pull, or a force push leased on what we've seen.
        let (d, l) = rejected();
        assert_eq!(l, ["Pull (rebase)", "Pull (merge)", "Force push…"]);
        let force = d.push_op(true).unwrap();
        let seen = sh(&a, &["rev-parse", "origin/main"]);
        assert!(force.cmds[0].contains(&format!("--force-with-lease=refs/heads/main:{}", seen.trim())));
        assert!(force.confirm.as_ref().unwrap().contains("\n$ git push --progress --force-with-lease="));

        // Someone pushes again and a background fetch picks it up: the lease must not be defeated.
        // The fetch leaves tags alone, so it can't invalidate the undo history.
        commit(&b, "b.txt", "b2\n");
        sh(&b, &["tag", "v9"]);
        sh(&b, &["push", "-q", "origin", "main", "v9"]);
        let refs = git::snapshot(&a).unwrap();
        git::fetch_quiet(&a).unwrap();
        assert_eq!(git::snapshot(&a).unwrap(), refs);
        let err = exec(&a, &force).unwrap_err();
        assert!(err.contains("stale info"), "{err}");

        // With the fresh state shown, the force push goes through.
        let force = load_all(&a, 100, View::default()).unwrap().push_op(true).unwrap();
        exec(&a, &force).unwrap();
        assert_eq!(sh(&root.join("remote.git"), &["rev-parse", "main"]), sh(&a, &["rev-parse", "HEAD"]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn checkout_autostash() {
        let dir = scratch("autostash");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "1\n2\n3\n4\n5\n");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "a.txt", "1 feat\n2\n3\n4\n5\n");
        sh(&dir, &["checkout", "-q", "main"]);
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
        let stashes = || sh(&dir, &["stash", "list"]).lines().count();
        let checkout = |b: &str| Op::new(format!("Checkout {b}"), &["checkout", b, "--"]).undo(Undo::Keep);
        let autostash = |b: &str| {
            let op = checkout(b);
            let err = exec(&dir, &op).unwrap_err();
            let c = offer(&op, &err, None).expect("blocked checkout offers autostash");
            assert!(c.text.contains("a.txt"), "{}", c.text);
            c.ops.into_iter().next().unwrap().1
        };

        // A dirty file the target branch touches carries over cleanly, staged and untracked files too.
        std::fs::write(dir.join("a.txt"), "1\n2\n3\n4\n5 mine\n").unwrap();
        std::fs::write(dir.join("staged.txt"), "s\n").unwrap();
        sh(&dir, &["add", "staged.txt"]);
        std::fs::write(dir.join("new.txt"), "n\n").unwrap();
        let op = autostash("feat");
        let before = git::snapshot(&dir).unwrap();
        exec(&dir, &op).unwrap();
        let after = git::snapshot(&dir).unwrap();
        let carried = |branch: &str, a: &str| {
            assert_eq!(sh(&dir, &["branch", "--show-current"]).trim(), branch);
            assert_eq!(read("a.txt"), a);
            assert_eq!(read("new.txt"), "n\n");
            assert_eq!(sh(&dir, &["diff", "--cached", "--name-only"]).trim(), "staged.txt");
            assert_eq!(stashes(), 0);
        };
        carried("feat", "1 feat\n2\n3\n4\n5 mine\n");

        // Undo and redo carry the changes back and forth too.
        let msg = op.autostash.as_deref();
        move_refs(&dir, &after, &before, Undo::Keep.mode(), msg).unwrap();
        carried("main", "1\n2\n3\n4\n5 mine\n");
        move_refs(&dir, &before, &after, Undo::Keep.mode(), msg).unwrap();
        carried("feat", "1 feat\n2\n3\n4\n5 mine\n");

        // Conflicting: markers in the file, and the stash is kept.
        sh(&dir, &["stash", "push", "-q", "-u"]);
        sh(&dir, &["stash", "drop", "-q"]);
        sh(&dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("a.txt"), "1 main\n2\n3\n4\n5\n").unwrap();
        let err = exec(&dir, &autostash("feat")).unwrap_err();
        assert!(err.contains("kept in the stash"), "{err}");
        assert_eq!(sh(&dir, &["branch", "--show-current"]).trim(), "feat");
        assert!(read("a.txt").contains("<<<<<<<"));
        assert_eq!(stashes(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
