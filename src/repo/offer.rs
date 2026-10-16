//! One-click ways out of common failures (blocked checkout, diverged pull,
//! rejected push), offered as a `Choice`.

use super::action::Choice;
use super::data::RepoData;
use super::op::Op;

fn pull_choices() -> Vec<(String, Op)> {
    use super::op::{PullMode, pull_op};
    let mut merge = pull_op(PullMode::Merge);
    merge.label = "Pull (merge)".into();
    vec![("Pull (rebase)".to_owned(), pull_op(PullMode::Rebase)), ("Pull (merge)".to_owned(), merge)]
}

fn cherry_pick_anyway(op: &Op, guard_at: usize, err: &str) -> Choice {
    let mut anyway = op.clone();
    anyway.cmds.remove(guard_at);
    Choice {
        title: "Already applied".into(),
        text: format!("{err}\n\nCherry-picking anyway may stop on an empty commit or conflicts, which you can skip or abort."),
        ops: vec![("Cherry-pick anyway".into(), anyway)],
    }
}

fn autostash_checkout(op: &Op, err: &str) -> Choice {
    let target = op.label.strip_prefix("Checkout ").unwrap_or(&op.label);
    let files: Vec<String> = err.lines().filter(|l| l.starts_with('\t')).map(|l| format!("  •  {}", l.trim())).collect();
    let mut o = op.clone();
    o.label = format!("{} (autostash)", op.label);
    o.autostash = Some(format!("autostash before switching to {target}"));
    Choice {
        title: "Uncommitted changes in the way".into(),
        text: format!(
            "Switching to {target} would overwrite your uncommitted changes in:\n{}\n\nGlasir can stash them, switch, and reapply them there.",
            files.join("\n")
        ),
        ops: vec![("Stash, switch and reapply".into(), o)],
    }
}

fn push_rejected(err: &str, d: Option<&RepoData>) -> Choice {
    let mut ops = pull_choices();
    // A force push leases on the remote commits we've seen: only possible once they're fetched.
    let text = if err.contains("(fetch first)") {
        ops.push(("Fetch".to_owned(), super::op::fetch_op()));
        "The remote branch has commits you haven't fetched. Pull them in first, then push again — or fetch to see them, and push again to get the option to replace them."
    } else {
        ops.extend(d.and_then(|d| d.push_op(true)).map(|o| ("Force push…".to_owned(), o)));
        "The remote branch has commits that aren't in yours. Pull them in first, then push again — or force push to replace them."
    };
    Choice { title: "Push rejected".into(), text: text.into(), ops }
}

fn signing_failed(op: &Op, i: usize, verb: &str, err: &str) -> Choice {
    let (on, off) = if verb == "commit" { ("-S", "--no-gpg-sign") } else { ("--sign", "--no-sign") };
    let mut o = op.clone();
    o.cmds[i].retain(|a| a != on);
    o.cmds[i].insert(1, off.into());
    Choice {
        title: "Signing failed".into(),
        text: format!(
            "Git couldn't sign this {verb}:\n\n{}\n\nCheck your signing setup (user.signingkey, gpg.format, the GPG agent / pinentry or your SSH agent); Glasir uses git's configuration and doesn't manage keys.",
            err.trim()
        ),
        ops: vec![(format!("{} without signing", if verb == "commit" { "Commit" } else { "Create tag" }), o)],
    }
}

/// When `op` failed with `err` in a way that has a one-click fix, the choices to offer.
pub(super) fn offer(op: &Op, err: &str, d: Option<&RepoData>) -> Option<Choice> {
    let i = op.cmds.iter().take_while(|c| c.first().is_some_and(|a| a == super::rebase::HEAD_IS)).count();
    let args = op.cmds.get(i)?;
    let has = |a: &str| args.iter().any(|x| x == a);
    // The guard's own refusal, wherever it sits (the drag cherry-pick checks HEAD first).
    let guard = op.cmds.iter().position(|c| c.first().is_some_and(|a| a == super::merge_opts::PICK_NEW));
    if let Some(i) = guard.filter(|_| err.starts_with(super::merge_opts::ALREADY)) {
        return Some(cherry_pick_anyway(op, i, err));
    }
    match args.first()?.as_str() {
        // Not for untracked files: the target's copy would be checked out and the stash couldn't pop.
        "checkout" if err.contains("would be overwritten by checkout") && !err.contains("untracked working tree files") => Some(autostash_checkout(op, err)),
        "pull" if has("--ff-only") && err.contains("Not possible to fast-forward") => Some(Choice {
            title: "Branches have diverged".into(),
            text: "Your branch and its upstream both have new commits, so a fast-forward isn't possible. Combine them by rebasing your commits on top, or with a merge commit.".into(),
            ops: pull_choices(),
        }),
        // Only the toolbar push (current branch, `--progress`): pulling fixes nothing for another branch.
        "push" if err.contains("[rejected]") && has("--progress") && !args.iter().any(|a| a.starts_with("--force")) => Some(push_rejected(err, d)),
        // gpg / ssh-keygen failed (no key, pinentry cancelled, locked agent): say so, offer to go unsigned.
        // A lone command only (bar the stacks' restack after it, which reports a failure as a note):
        // in a commit-then-rebase op, a later step may be what failed to sign.
        verb @ ("commit" | "tag") if op.cmds[i + 1..].iter().all(|c| c.first().is_some_and(|a| a == super::stack::RESTACK)) && ["failed to sign", "unable to sign", "user.signingkey", "Couldn't load public key", "Couldn't sign"].iter().any(|s| err.contains(s)) => {
            Some(signing_failed(op, i, verb, err))
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

    /// Branches whose upstream has another name: one started from origin/main gets its own
    /// remote branch, a deliberate upstream is updated. Deleting and renaming keep the upstream.
    #[test]
    fn push_and_undo_with_differently_named_upstream() {
        let root = scratch("upnames");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        sh(&root, &["clone", "-q", "remote.git", "a"]);
        let (a, remote) = (root.join("a"), root.join("remote.git"));
        identity(&a);
        commit(&a, "f.txt", "one\n");
        sh(&a, &["push", "-q", "-u", "origin", "main"]);
        sh(&a, &["push", "-q", "origin", "main:chan/fix"]);
        let load = || load_all(&a, 100, View::default()).unwrap();
        let tip = |r: &str| sh(&remote, &["rev-parse", r]);

        sh(&a, &["checkout", "-q", "-b", "feature", "origin/main"]);
        commit(&a, "g.txt", "two\n");
        exec(&a, &load().push_op(false).unwrap()).unwrap();
        assert_eq!(tip("feature"), sh(&a, &["rev-parse", "HEAD"]));
        assert_ne!(tip("main"), tip("feature"));
        assert_eq!(sh(&a, &["rev-parse", "--abbrev-ref", "@{u}"]).trim(), "origin/feature");

        sh(&a, &["checkout", "-q", "-b", "fix", "--track", "origin/chan/fix"]);
        commit(&a, "h.txt", "three\n");
        exec(&a, &load().push_op(false).unwrap()).unwrap();
        assert_eq!(tip("chan/fix"), sh(&a, &["rev-parse", "HEAD"]));
        assert!(git::run(&remote, &["rev-parse", "--verify", "-q", "refs/heads/fix"]).is_err());

        // Undo of a delete and a rename brings the upstream back too.
        sh(&a, &["checkout", "-q", "main"]);
        let del = crate::repo::op::delete_branches(&load(), "Delete fix".into(), &["fix"], true);
        exec(&a, &del).unwrap();
        exec(&a, del.revert.as_ref().unwrap()).unwrap();
        assert_eq!(sh(&a, &["rev-parse", "--abbrev-ref", "fix@{u}"]).trim(), "origin/chan/fix");
        let mut ren = Op::new("Rename branch", &["branch", "-m", "fix", "{}"]).revert(Op::new("Rename branch back", &["branch", "-m", "{}", "fix"]));
        let revert = ren.revert.as_deref_mut().into_iter().flat_map(|r| r.cmds.iter_mut().flatten());
        for x in ren.cmds.iter_mut().flatten().chain(revert) {
            *x = x.replace("{}", "fix2"); // what the prompt does
        }
        exec(&a, &ren).unwrap();
        exec(&a, ren.revert.as_ref().unwrap()).unwrap();
        assert_eq!(sh(&a, &["rev-parse", "--abbrev-ref", "fix@{u}"]).trim(), "origin/chan/fix");
        assert!(git::run(&a, &["config", "--get", "branch.fix2.merge"]).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn push_and_force_push_a_branch_named_like_a_tag() {
        let root = scratch("push-tagged");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        sh(&root, &["clone", "-q", "remote.git", "a"]);
        let (a, remote) = (root.join("a"), root.join("remote.git"));
        identity(&a);
        commit(&a, "f.txt", "one\n");
        sh(&a, &["push", "-q", "-u", "origin", "main"]);
        sh(&a, &["push", "-q", "origin", "main:chan/fix"]);
        let load = || load_all(&a, 100, View::default()).unwrap();
        let tip = |r: &str| sh(&remote, &["rev-parse", r]);
        for (branch, from, remote_branch) in [("rel", None, "rel"), ("feature", Some("origin/main"), "feature"), ("fix", Some("origin/chan/fix"), "chan/fix")] {
            let mut start = vec!["checkout", "-q", "-b", branch];
            start.extend(from);
            sh(&a, &start);
            commit(&a, branch, "one\n");
            sh(&a, &["tag", branch]);
            exec(&a, &load().push_op(false).unwrap()).unwrap();
            assert_eq!(tip(remote_branch), sh(&a, &["rev-parse", "HEAD"]), "{branch}");
            sh(&a, &["commit", "-q", "--amend", "-m", "again"]);
            exec(&a, &load().push_op(true).unwrap()).unwrap();
            assert_eq!(tip(remote_branch), sh(&a, &["rev-parse", "HEAD"]), "{branch} forced");
        }
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

        // Undo and redo move the branch, never merging: the edited a.txt stays as it is, and is said.
        let undo = |from: &git::Snapshot, to: &git::Snapshot| crate::repo::undo::transition(&dir, "Undo", Some((from.clone(), to.clone(), Undo::Keep)), &[]).unwrap().0;
        assert!(undo(&after, &before).is_some_and(|n| n.contains("a.txt")));
        carried("main", "1 feat\n2\n3\n4\n5 mine\n");
        undo(&before, &after);
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

        // An untracked file the target branch has: stashing can't help, so no offer.
        sh(&dir, &["checkout", "-q", "-f", "main"]);
        sh(&dir, &["stash", "clear"]);
        sh(&dir, &["checkout", "-q", "-b", "adds"]);
        commit(&dir, "added.txt", "theirs\n");
        sh(&dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("added.txt"), "mine\n").unwrap();
        let op = checkout("adds");
        assert!(offer(&op, &exec(&dir, &op).unwrap_err(), None).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SSH signing end to end: signed/unsigned commits and tags, the badge codes, and a failing key.
    #[test]
    fn ssh_signing() {
        let dir = scratch("signing");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let key = dir.join("key");
        let made = std::process::Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-C", "t@t", "-f"]).arg(&key).status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("ssh-keygen unavailable: skipping");
            return;
        }
        std::fs::write(dir.join(".gitignore"), "key*\nallowed\n").unwrap();
        let pubkey = std::fs::read_to_string(dir.join("key.pub")).unwrap();
        sh(&dir, &["config", "gpg.format", "ssh"]);
        sh(&dir, &["config", "user.signingkey", &dir.join("key.pub").to_string_lossy()]);
        let ctx = egui::Context::default();
        let mut t = crate::repo::RepoTab::new(&ctx, dir.clone(), 100, View::default());
        let info = || git::commit_info(&dir, "HEAD").unwrap();

        // Signed via the toggle; without an allowed-signers file git can't check it.
        sh(&dir, &["add", ".gitignore"]);
        t.summary = "signed".into();
        t.sign = Some(true);
        exec(&dir, &t.commit_op()).unwrap();
        assert_eq!(info().sig, 'E');
        std::fs::write(dir.join("allowed"), format!("t@t {pubkey}")).unwrap();
        sh(&dir, &["config", "gpg.ssh.allowedSignersFile", &dir.join("allowed").to_string_lossy()]);
        let i = info();
        assert_eq!((i.sig, i.signer.as_str()), ('G', "t@t"));
        assert!(i.key.starts_with("SHA256:"), "{}", i.key);

        // Unsigned despite commit.gpgSign.
        sh(&dir, &["config", "commit.gpgsign", "true"]);
        t.sign = Some(false);
        t.summary = "plain".into();
        std::fs::write(dir.join("a"), "a").unwrap();
        sh(&dir, &["add", "a"]);
        exec(&dir, &t.commit_op()).unwrap();
        assert_eq!(info().sig, 'N');

        // Tags as the tag dialog builds them; verify-tag tells signed from not.
        for (name, flag) in [("v1", "--sign"), ("v0", "--no-sign")] {
            exec(&dir, &Op::new("Create tag", &["tag", flag, "-a", name, "-m", "m", "HEAD"])).unwrap();
        }
        assert!(git::verify_tag(&dir, "refs/tags/v1").0);
        assert!(!git::verify_tag(&dir, "refs/tags/v0").0);

        // A missing key fails loudly and offers to go unsigned.
        sh(&dir, &["config", "user.signingkey", &dir.join("nope.pub").to_string_lossy()]);
        let head = sh(&dir, &["rev-parse", "HEAD"]);
        t.sign = Some(true);
        std::fs::write(dir.join("a"), "b").unwrap();
        sh(&dir, &["add", "a"]);
        let two = t.commit_op().then(&["status"]);
        let err = exec(&dir, &two).unwrap_err();
        assert!(offer(&two, &err, None).is_none());
        op_unsigned(&dir, &t.commit_op(), "Commit without signing");
        assert_ne!(sh(&dir, &["rev-parse", "HEAD"]), head);
        assert_eq!(info().sig, 'N');
        op_unsigned(&dir, &Op::new("Create tag", &["tag", "--sign", "-a", "v2", "-m", "m", "HEAD"]), "Create tag without signing");
        let head = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        let reword = crate::repo::rebase::reword(&dir, &git::git_dir(&dir).unwrap(), &head, true, None, "reworded".into()).unwrap();
        op_unsigned(&dir, &reword, "Commit without signing");
        assert_eq!(sh(&dir, &["log", "-1", "--format=%s"]).trim(), "reworded");
        assert!(!git::verify_tag(&dir, "refs/tags/v2").0);
        let head = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        std::fs::write(dir.join("a"), "c").unwrap();
        sh(&dir, &["add", "a"]);
        let amend = crate::repo::rebase::amend_into(&dir, &head, true).unwrap();
        assert_eq!(amend.cmds[0][0], crate::repo::rebase::HEAD_IS);
        op_unsigned(&dir, &amend, "Commit without signing");
        assert_eq!((sh(&dir, &["show", "HEAD:a"]).as_str(), info().sig), ("c", 'N'));

        // With stacks a restack follows the commit: still offered, and the retry restacks (once).
        sh(&dir, &["config", "--unset", "commit.gpgsign"]);
        for b in ["s1", "s2"] {
            sh(&dir, &["checkout", "-q", "-b", b]);
            commit(&dir, b, b);
        }
        sh(&dir, &["checkout", "-q", "s1"]);
        std::fs::write(dir.join("s1"), "s1 more").unwrap();
        sh(&dir, &["add", "s1"]);
        t.summary = "s1 more".into();
        let d = load_all(&dir, 100, View::default()).unwrap();
        let op = crate::repo::stack::follow(t.commit_op(), Some(&d));
        assert_eq!(op.cmds.last().unwrap()[0], crate::repo::stack::RESTACK);
        let err = exec(&dir, &op).unwrap_err();
        let retry = offer(&op, &err, None).expect("offered with the restack after the commit").ops.remove(0).1;
        assert_eq!(crate::repo::stack::follow(retry.clone(), Some(&d)).cmds, retry.cmds);
        exec(&dir, &retry).unwrap();
        assert_eq!(sh(&dir, &["rev-parse", "s2~1"]), sh(&dir, &["rev-parse", "s1"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// "Already applied" answers only the guard's own refusal, wherever the guard sits.
    #[test]
    fn already_applied_offer_only_for_the_guard() {
        use crate::repo::merge_opts::{PICK_NEW, pick_op};
        use crate::repo::rebase::{HEAD_IS, head_state};
        let dir = scratch("offer-picked");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "f", "feat\n");
        let pick = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "f", "main\n");
        let op = pick_op("Cherry-pick", &[&pick], false, false);
        let err = exec(&dir, &op).unwrap_err();
        assert!(offer(&op, &err, None).is_none(), "a conflict isn't 'already applied': {err}");
        sh(&dir, &["cherry-pick", "--abort"]);
        // The drag cherry-pick checks HEAD first.
        sh(&dir, &["checkout", "-q", "feat"]);
        let mut op = pick_op("Cherry-pick", &[&pick], false, false);
        op.cmds.insert(0, vec![HEAD_IS.into(), head_state(&dir)]);
        let c = offer(&op, &exec(&dir, &op).unwrap_err(), None).expect("already applied is offered");
        assert_eq!(c.title, "Already applied");
        assert!(c.ops[0].1.cmds[0][0] == HEAD_IS && !c.ops[0].1.cmds.iter().any(|c| c[0] == PICK_NEW));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `op` fails to sign; the offered fix runs.
    fn op_unsigned(dir: &std::path::Path, op: &Op, label: &str) {
        let err = exec(dir, op).unwrap_err();
        let c = offer(op, &err, None).unwrap_or_else(|| panic!("no offer for {err}"));
        assert_eq!(c.title, "Signing failed");
        assert_eq!(c.ops[0].0, label);
        exec(dir, &c.ops[0].1).unwrap();
    }
}
