//! Remote management: the add / edit form (rename, fetch and push URL), removal
//! that ⌘Z restores (config, upstreams, remote-tracking refs), checking out a
//! pull request by number and the opt-in pull-request refspec.

use super::{Op, RepoData, Undo};

/// The add / edit remote dialog. `old`: the edited remote's (name, url, push url).
#[derive(Clone, Debug, Default)]
pub struct Form {
    pub old: Option<(String, String, String)>,
    pub name: String,
    pub url: String,
    /// Separate push URL; empty = push to `url`.
    pub push: String,
}

/// An op running `cmds` in order (`cmds` is never empty).
fn seq(label: String, cmds: &[Vec<&str>]) -> Op {
    cmds[1..].iter().fold(Op::new(label, &cmds[0]), |op, c| op.then(c))
}

impl Form {
    pub fn add(d: &RepoData) -> Self {
        let name = ["origin", "upstream", "fork"].into_iter().find(|n| !d.remotes.iter().any(|r| r.0 == *n)).unwrap_or_default();
        Form { name: name.into(), ..Default::default() }
    }

    pub fn edit(d: &RepoData, name: &str) -> Self {
        let get = |k: &str| d.cfg(&format!("remote.{name}.{k}")).unwrap_or_default().to_owned();
        let (url, push) = (get("url"), get("pushurl"));
        Form { old: Some((name.into(), url.clone(), push.clone())), name: name.into(), url, push }
    }

    /// Why the form can't be saved as it is.
    pub fn problem(&self, d: &RepoData) -> Option<&'static str> {
        let name = self.name.trim();
        if name.is_empty() || self.url.trim().is_empty() {
            return Some("Enter a name and a URL");
        }
        if name.starts_with('-') || name.contains(|c: char| c.is_whitespace() || "~^:?*[\\".contains(c)) {
            return Some("Not a valid remote name");
        }
        if self.old.as_ref().is_none_or(|o| o.0 != name) && d.remotes.iter().any(|r| r.0 == name) {
            return Some("A remote with this name already exists");
        }
        None
    }

    /// Saves the form (a new remote is fetched right away); ⌘Z reverts it.
    /// `None` when nothing changed.
    pub fn op(&self) -> Option<Op> {
        let (name, url, push) = (self.name.trim(), self.url.trim(), self.push.trim());
        let Some((old, old_url, old_push)) = &self.old else {
            let mut cmds = vec![vec!["remote", "add", name, url]];
            if !push.is_empty() {
                cmds.push(vec!["remote", "set-url", "--push", name, push]);
            }
            cmds.push(vec!["fetch", "--progress", name]);
            return Some(seq(format!("Add remote {name}"), &cmds).revert(Op::new(format!("Remove remote {name}"), &["remote", "remove", name])));
        };
        let push_key = format!("remote.{name}.pushurl");
        fn set_push<'a>(name: &'a str, key: &'a str, p: &'a str) -> Vec<&'a str> {
            if p.is_empty() { vec!["config", "--unset-all", key] } else { vec!["remote", "set-url", "--push", name, p] }
        }
        let (mut fwd, mut back) = (vec![], vec![]);
        // Renamed first: a name git rejects then fails before anything changed. The URL
        // commands use the new name (and the way back renames last).
        if name != old {
            fwd.push(vec!["remote", "rename", old, name]);
        }
        if url != old_url {
            fwd.push(vec!["remote", "set-url", name, url]);
            back.push(vec!["remote", "set-url", name, old_url]);
        }
        if push != old_push {
            fwd.push(set_push(name, &push_key, push));
            back.push(set_push(name, &push_key, old_push));
        }
        if name != old {
            back.push(vec!["remote", "rename", name, old]);
        }
        (!fwd.is_empty()).then(|| seq(format!("Edit remote {old}"), &fwd).revert(seq(format!("Restore remote {old}"), &back)))
    }
}

/// `git remote remove`, confirmed. ⌘Z re-adds the remote's config, the upstreams of
/// branches tracking it and its remote-tracking refs (offline: the objects are still here).
pub fn remove(d: &RepoData, name: &str) -> Op {
    let mut back: Vec<Vec<String>> = vec![];
    let add = |k: &str, v: &str| vec!["config".to_owned(), "--add".into(), k.into(), v.into()];
    let prefix = format!("remote.{name}.");
    // Not the keys of a remote `<name>.x`, which stay.
    back.extend(d.config.iter().filter(|(k, _)| k.strip_prefix(&prefix).is_some_and(|var| !var.contains('.'))).map(|(k, v)| add(k, v)));
    // `remote remove` also drops the upstream of every branch tracking it.
    for (k, _) in d.config.iter().filter(|(k, v)| k.starts_with("branch.") && k.ends_with(".remote") && v == name) {
        let merge = format!("{}.merge", k.trim_end_matches(".remote"));
        back.push(add(k, name));
        back.extend(d.cfg(&merge).map(|m| add(&merge, m)));
    }
    // ...and every push remote naming it.
    back.extend(d.config.iter().filter(|(k, v)| k.starts_with("branch.") && k.ends_with(".pushremote") && v == name).map(|(k, v)| add(k, v)));
    if d.cfg("remote.pushdefault") == Some(name) {
        back.push(add("remote.pushdefault", name));
    }
    let tracking_prefix = format!("refs/remotes/{name}/");
    let tracking: Vec<_> = d.refs.iter().filter(|r| r.full.starts_with(&tracking_prefix)).collect();
    let refs: String = tracking.iter().map(|r| format!("create {} {}\n", r.full, r.target)).collect();
    if !refs.is_empty() {
        back.push(vec!["update-ref".into(), "--stdin".into()]);
    }
    if name == "origin" && !d.origin_head.is_empty() {
        back.push(["symbolic-ref", "refs/remotes/origin/HEAD", &format!("refs/remotes/origin/{}", d.origin_head)].map(String::from).to_vec());
    }
    let mut revert = Op::new(format!("Restore remote {name}"), &[]);
    revert.cmds = back;
    // Only `update-ref --stdin` reads the input; `git config` ignores it.
    let revert = if refs.is_empty() { revert } else { revert.input(refs) };
    let n = tracking.len();
    Op::new(format!("Remove remote {name}"), &["remote", "remove", name])
        .confirm(format!("Remove remote {name} and its {n} remote-tracking branches? Local branches stay. (Undo with ⌘Z.)"))
        .revert(revert)
}

/// Fetch pull request `n` (filled in by a prompt: `Prompt::VALUE`) from `remote` into `pr/<n>` and
/// check it out. Works on github.com and GitHub Enterprise, which publish `refs/pull/<n>/head`.
pub fn pr_checkout(remote: &str) -> Op {
    let n = super::Prompt::VALUE;
    // Via `-B`: running it again updates pr/<n>, even checked out or force-pushed.
    // `seen` remembers the head last checked out, so a force-push isn't taken for local work.
    // Into a ref of its own, not FETCH_HEAD: a background fetch may rewrite that meanwhile.
    let (seen, pr) = (format!("refs/glasir/pr/{remote}/{n}"), format!("pr/{n}"));
    let fetched = format!("{seen}-fetched");
    Op::new("Check out pull request", &["fetch", "--progress", remote, &format!("+refs/pull/{n}/head:{fetched}")])
        .then(&[PR_GUARD, &pr, &fetched, &seen])
        .then(&["checkout", "-B", &pr, &fetched, "--"])
        .then(&["update-ref", &seen, &fetched])
        .undo(Undo::Keep)
}

/// `exec` pseudo-command `PR_GUARD branch fetched seen`: fails when `branch` has commits that
/// are neither in `fetched` nor in ref `seen`, which resetting it would drop.
pub(super) const PR_GUARD: &str = "!pr-guard";

pub(super) fn pr_guard(dir: &std::path::Path, branch: &str, fetched: &str, seen: &str) -> crate::git::Res<String> {
    let has = |r: &str| crate::git::run(dir, &["rev-parse", "-q", "--verify", r]).is_ok();
    let full = format!("refs/heads/{branch}");
    if !has(&full) {
        return Ok(String::new());
    }
    let mut args = vec!["rev-list", "--count", &full, "--not", fetched];
    if has(seen) {
        args.push(seen);
    }
    match crate::git::run(dir, &args)?.trim() {
        "0" => Ok(String::new()),
        n => Err(format!("{branch} has {n} commit(s) that are not on the pull request, so it was not reset. Rename {branch} to keep them, then check the pull request out again.")),
    }
}

/// Delete `branch` on `remote`, only while it is still at `oid` (its remote-tracking ref): commits
/// pushed there since the last fetch are never deleted unseen. Qualified: once the branch is gone
/// there, a short name would delete a same-named tag instead.
pub fn delete_branch(remote: &str, branch: &str, oid: &str) -> Op {
    Op::new(format!("Delete {remote}/{branch}"), &[super::cleanup::REMOTE_DELETE, remote, branch, oid])
        .confirm(format!("Delete branch {branch} on remote {remote}?"))
}

/// Fast-forward local branch `name` to branch `rb` of `remote` without checking it out.
/// Qualified: git resolves a short source name as a tag before a branch.
pub fn pull_ff(name: &str, remote: &str, rb: &str) -> Op {
    Op::new(format!("Pull {name}"), &["fetch", remote, &format!("refs/heads/{rb}:refs/heads/{name}")]).undo(Undo::Keep)
}

fn pr_refspec(remote: &str) -> String {
    format!("+refs/pull/*/head:refs/remotes/{remote}/pr/*")
}

/// Whether `remote` fetches every pull request head as `<remote>/pr/<n>`.
pub fn pr_refs_on(d: &RepoData, remote: &str) -> bool {
    d.cfg_all(&format!("remote.{remote}.fetch")).any(|v| v == pr_refspec(remote))
}

/// A pull request head `<remote>/pr/<n>` (`n` is a number).
fn pr_head<'a>(r: &'a crate::git::Ref, remote: &str) -> Option<&'a str> {
    r.full.strip_prefix(&format!("refs/remotes/{remote}/pr/")).filter(|n| !n.is_empty())
}

/// A real branch `pr/<n>` or `pr/<n>/…` on `remote` that pull request heads would collide
/// with, making every fetch fail: then fetching them can't be turned on.
pub fn pr_refs_clash<'a>(d: &'a RepoData, remote: &str) -> Option<&'a str> {
    let numbered = |n: &str| n.split('/').next().is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    (!pr_refs_on(d, remote)).then(|| d.refs.iter().find(|r| pr_head(r, remote).is_some_and(numbered)).map(|r| r.name.as_str())).flatten()
}

/// Toggle fetching pull request heads; turning it off deletes the fetched `pr/<n>` refs
/// (not remote branches like `pr/feature`).
pub fn pr_refs_op(d: &RepoData, remote: &str) -> Op {
    let (key, spec) = (format!("remote.{remote}.fetch"), pr_refspec(remote));
    if !pr_refs_on(d, remote) {
        return Op::new(format!("Fetch pull requests from {remote}"), &["config", "--add", &key, &spec]).then(&["fetch", "--progress", remote]);
    }
    let stale: String = d.refs.iter().filter(|r| pr_head(r, remote).is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))).map(|r| format!("delete {}\n", r.full)).collect();
    let op = Op::new(format!("Stop fetching pull requests from {remote}"), &["config", "--unset-all", "--fixed-value", &key, &spec]);
    if stale.is_empty() { op } else { op.then(&["update-ref", "--stdin"]).input(stale) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git;
    use crate::repo::View;
    use crate::repo::data::load_all;
    use crate::repo::op::exec;
    use crate::repo::testutil::{commit, identity, scratch, sh};

    /// Add (and fetch), edit, remove and restore a remote the way the sidebar does it.
    #[test]
    fn manage_remotes_with_undo() {
        let up = scratch("remotes-up");
        sh(&up, &["init", "-q", "-b", "main"]);
        identity(&up);
        commit(&up, "a.txt", "one\n");
        sh(&up, &["checkout", "-q", "-b", "feat"]);
        commit(&up, "b.txt", "two\n");
        let dir = scratch("remotes");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let load = || load_all(&dir, 100, View::default()).unwrap();
        // Sorted: re-added keys may land in another order in the config file.
        let cfg = || {
            let mut l: Vec<String> = sh(&dir, &["config", "--local", "--get-regexp", r"^(remote|branch)\."]).lines().map(String::from).collect();
            l.sort();
            l
        };
        let tracking = || sh(&dir, &["for-each-ref", "--format=%(refname)", "refs/remotes"]);

        let d = load();
        let mut f = Form::add(&d);
        assert_eq!(f.name, "origin");
        assert_eq!(f.problem(&d), Some("Enter a name and a URL"));
        f.url = up.to_string_lossy().into();
        f.push = "/elsewhere.git".into();
        assert_eq!(f.problem(&d), None);
        exec(&dir, &f.op().unwrap()).unwrap();
        sh(&dir, &["checkout", "-q", "--track", "origin/feat"]);
        let d = load();
        assert_eq!(d.cfg("remote.origin.pushurl"), Some("/elsewhere.git"));
        // (git 2.48+ also records origin/HEAD on fetch.)
        assert_eq!(tracking().replace("refs/remotes/origin/HEAD\n", ""), "refs/remotes/origin/feat\nrefs/remotes/origin/main\n");
        assert_eq!(Form::add(&d).name, "upstream");
        assert_eq!(Form { name: "origin".into(), url: "x".into(), ..Default::default() }.problem(&d), Some("A remote with this name already exists"));

        // Edit: rename and drop the push URL in one go; the revert puts both back.
        let mut e = Form::edit(&d, "origin");
        assert_eq!((e.url.as_str(), e.push.as_str()), (f.url.as_str(), "/elsewhere.git"));
        assert!(e.op().is_none(), "nothing changed");
        (e.name, e.push) = ("fork".into(), String::new());
        let op = e.op().unwrap();
        let before = cfg();
        exec(&dir, &op).unwrap();
        assert!(load().cfg("remote.fork.pushurl").is_none());
        assert_eq!(sh(&dir, &["rev-parse", "--abbrev-ref", "@{u}"]).trim(), "fork/feat");
        exec(&dir, op.revert.as_ref().unwrap()).unwrap();
        assert_eq!(cfg(), before);
        // A name the form allows but git rejects changes nothing (no half-applied edit).
        let mut bad = Form::edit(&d, "origin");
        (bad.name, bad.url) = ("a..b".into(), "/new.git".into());
        assert_eq!(bad.problem(&d), None);
        assert!(exec(&dir, &bad.op().unwrap()).is_err());
        assert_eq!(cfg(), before);

        // Remove, then undo: config, the branch's upstream, push remotes and the tracking refs
        // come back; a remote whose name starts with `origin.` is left alone.
        sh(&dir, &["remote", "add", "origin.b", "/b.git"]);
        sh(&dir, &["config", "branch.feat.pushremote", "origin"]);
        sh(&dir, &["config", "remote.pushdefault", "origin"]);
        let (before, refs) = (cfg(), tracking());
        let op = remove(&load(), "origin");
        assert!(op.confirm.as_deref().unwrap().contains("its 2 remote-tracking"));
        exec(&dir, &op).unwrap();
        assert_eq!((tracking().as_str(), git::run(&dir, &["rev-parse", "--abbrev-ref", "@{u}"]).is_err()), ("", true));
        exec(&dir, op.revert.as_ref().unwrap()).unwrap();
        assert_eq!((cfg(), tracking()), (before, refs));
        assert_eq!(sh(&dir, &["rev-parse", "--abbrev-ref", "@{u}"]).trim(), "origin/feat");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&up);
    }

    /// A local-only repo gets its first remote (a freshly created, empty one) and
    /// publishes to it: the fetch of the empty remote succeeds, the push sets upstream.
    #[test]
    fn add_origin_then_first_push() {
        let up = scratch("first-push-up");
        sh(&up, &["init", "-q", "--bare"]);
        let dir = scratch("first-push");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "one\n");
        let load = || load_all(&dir, 100, View::default()).unwrap();
        let d = load();
        assert!(d.remotes.is_empty() && d.push_op(false).is_none());
        let mut f = Form::add(&d);
        f.url = up.to_string_lossy().into();
        exec(&dir, &f.op().unwrap()).unwrap();
        let op = load().push_op(false).unwrap();
        assert_eq!(op.cmds[0], ["push", "--progress", "-u", "origin", "main"]);
        exec(&dir, &op).unwrap();
        assert_eq!(sh(&dir, &["rev-parse", "--abbrev-ref", "@{u}"]).trim(), "origin/main");
        assert_eq!(sh(&up, &["rev-parse", "main"]), sh(&dir, &["rev-parse", "HEAD"]));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&up);
    }

    /// GitHub publishes pull requests as `refs/pull/<n>/head`: check one out, list them all.
    #[test]
    fn check_out_pull_requests() {
        let up = scratch("pr-up");
        sh(&up, &["init", "-q", "-b", "main"]);
        identity(&up);
        commit(&up, "a.txt", "one\n");
        commit(&up, "a.txt", "pr work\n");
        sh(&up, &["update-ref", "refs/pull/7/head", "HEAD"]);
        let pr = sh(&up, &["rev-parse", "HEAD"]);
        sh(&up, &["reset", "-q", "--hard", "HEAD~1"]);
        let dir = scratch("pr");
        sh(&dir, &["clone", "-q", &up.to_string_lossy(), "."]);
        identity(&dir);

        let mut op = pr_checkout("origin");
        for a in op.cmds.iter_mut().flatten() {
            *a = a.replace(crate::repo::Prompt::VALUE, "7"); // what the prompt does
        }
        exec(&dir, &op).unwrap();
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), pr);
        assert_eq!(sh(&dir, &["branch", "--show-current"]).trim(), "pr/7");
        // Force-pushed: running it again while on pr/7 moves it to the new head.
        commit(&up, "a.txt", "pr redone\n");
        sh(&up, &["update-ref", "refs/pull/7/head", "HEAD"]);
        let pr = sh(&up, &["rev-parse", "HEAD"]);
        exec(&dir, &op).unwrap();
        assert_eq!(sh(&dir, &["rev-parse", "pr/7"]), pr);
        // A local commit on pr/7 is not reset away by checking the PR out again.
        commit(&dir, "mine.txt", "review fixup\n");
        let mine = sh(&dir, &["rev-parse", "HEAD"]);
        assert!(exec(&dir, &op).unwrap_err().contains("not on the pull request"));
        assert_eq!(sh(&dir, &["rev-parse", "pr/7"]), mine);
        sh(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
        sh(&up, &["branch", "pr/feature"]);

        let load = || load_all(&dir, 100, View::default()).unwrap();
        sh(&dir, &["fetch", "-q", "origin"]);
        assert!(!pr_refs_on(&load(), "origin") && pr_refs_clash(&load(), "origin").is_none());
        exec(&dir, &pr_refs_op(&load(), "origin")).unwrap();
        let d = load();
        assert!(pr_refs_on(&d, "origin"));
        assert!(d.refs.iter().any(|r| r.name == "origin/pr/7" && r.target == pr.trim()));
        exec(&dir, &pr_refs_op(&d, "origin")).unwrap();
        let d = load();
        let pr_refs: Vec<&str> = d.refs.iter().map(|r| r.name.as_str()).filter(|n| n.starts_with("origin/pr/")).collect();
        assert!(!pr_refs_on(&d, "origin") && pr_refs == ["origin/pr/feature"], "{pr_refs:?}");
        // A real branch pr/7 would collide with pull request 7.
        sh(&up, &["branch", "pr/7"]);
        sh(&dir, &["fetch", "-q", "origin"]);
        assert_eq!(pr_refs_clash(&load(), "origin"), Some("origin/pr/7"));
        assert_eq!(d.cfg_all("remote.origin.fetch").collect::<Vec<_>>(), ["+refs/heads/*:refs/remotes/origin/*"]);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&up);
    }

    /// Short names resolve to tags first: deleting a branch already gone on the remote
    /// must not delete a same-named tag there, and a pull must not take a tag's commit.
    #[test]
    fn branch_ops_never_hit_same_named_tags() {
        let up = scratch("tagclash-up");
        sh(&up, &["init", "-q", "-b", "main"]);
        identity(&up);
        commit(&up, "a.txt", "one\n");
        sh(&up, &["branch", "release-1"]);
        sh(&up, &["tag", "release-1"]);
        sh(&up, &["branch", "dev"]);
        let dir = scratch("tagclash");
        sh(&dir, &["clone", "-q", &up.to_string_lossy(), "."]);
        sh(&dir, &["branch", "dev", "origin/dev"]);
        sh(&up, &["branch", "-D", "release-1"]);
        let seen = sh(&dir, &["rev-parse", "refs/remotes/origin/release-1"]);
        // (Gone since the fetch: the lease refuses.)
        assert!(exec(&dir, &delete_branch("origin", "release-1", seen.trim())).is_err_and(|e| e.contains("changed on the remote")));
        assert_eq!(sh(&up, &["tag"]), "release-1\n");

        sh(&up, &["checkout", "-q", "dev"]);
        commit(&up, "a.txt", "two\n");
        let b = sh(&up, &["rev-parse", "HEAD"]);
        commit(&up, "a.txt", "three\n");
        sh(&up, &["tag", "dev"]);
        sh(&up, &["reset", "-q", "--hard", "HEAD~1"]);
        exec(&dir, &pull_ff("dev", "origin", "dev")).unwrap();
        assert_eq!(sh(&dir, &["rev-parse", "refs/heads/dev"]), b);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&up);
    }

    /// Fork workflow: a pushDefault wins over origin for pushes, a trunk tracking a renamed
    /// remote trunk keeps pushing there, and with no origin/HEAD the default branch is found.
    #[test]
    fn push_targets() {
        let up = scratch("targets-up");
        sh(&up, &["init", "-q", "-b", "master"]);
        identity(&up);
        commit(&up, "a.txt", "one\n");
        let dir = scratch("targets");
        sh(&dir, &["clone", "-q", "-o", "origin", &up.to_string_lossy(), "."]);
        sh(&dir, &["remote", "add", "fork", &up.to_string_lossy()]);
        sh(&dir, &["fetch", "-q", "fork"]);
        sh(&dir, &["remote", "set-head", "-d", "origin"]);
        sh(&dir, &["checkout", "-q", "-b", "main", "--track", "origin/master"]);
        sh(&dir, &["checkout", "-q", "-b", "feat", "--track", "origin/master"]);
        let d = load_all(&dir, 100, View::default()).unwrap();
        assert_eq!(d.default_branch(), "master");
        assert_eq!(d.push_dest("main", "origin/master"), Some(("origin", "master")));
        assert_eq!(d.push_dest("feat", "origin/master"), Some(("origin", "feat")));
        assert_eq!(d.default_remote(), Some("origin"));
        assert_eq!(d.push_target("feat", "origin/master"), Some(("origin", "feat")));
        sh(&dir, &["config", "remote.pushdefault", "fork"]);
        let d = load_all(&dir, 100, View::default()).unwrap();
        assert_eq!(d.push_target("feat", "origin/master"), Some(("fork", "feat")));
        assert_eq!(d.push_target("new", ""), Some(("fork", "new")));
        for p in [dir, up] {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    /// A remote's name may contain `/`: "Delete x from a/b" deletes x on remote a/b,
    /// never branch b/x on a remote named a.
    #[test]
    fn remote_names_with_slashes() {
        let (up1, up2) = (scratch("slash-up1"), scratch("slash-up2"));
        for up in [&up1, &up2] {
            sh(up, &["init", "-q", "-b", "main"]);
            identity(up);
            commit(up, "a.txt", "one\n");
        }
        sh(&up1, &["branch", "x"]);
        sh(&up2, &["branch", "b/x"]);
        let dir = scratch("slash");
        sh(&dir, &["clone", "-q", "-o", "a/b", &up1.to_string_lossy(), "."]);
        // git 2.46+ refuses names where one is a prefix directory of the other.
        if crate::git::run(&dir, &["remote", "add", "a", &up2.to_string_lossy()]).is_err_and(|e| e.contains("superset")) {
            return;
        }
        let d = load_all(&dir, 100, View::default()).unwrap();
        assert_eq!(d.push_dest("main", "a/b/main"), Some(("a/b", "main")));
        let (remote, branch) = d.split_remote("a/b/x").unwrap();
        let oid = &d.refs.iter().find(|r| r.name == "a/b/x").unwrap().target;
        exec(&dir, &delete_branch(remote, branch, oid)).unwrap();
        assert_eq!(sh(&up2, &["branch", "--list", "b/x"]).trim(), "b/x");
        assert_eq!(sh(&up1, &["branch", "--list", "x"]), "");
        for p in [dir, up1, up2] {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    /// "Delete x from origin" after a teammate pushed to x since the last fetch: refused, their
    /// commit stays; once fetched (and seen), the delete goes through.
    #[test]
    fn remote_delete_keeps_commits_pushed_since_the_fetch() {
        let up = scratch("rdel-up");
        sh(&up, &["init", "-q", "-b", "main"]);
        identity(&up);
        commit(&up, "a.txt", "one\n");
        sh(&up, &["branch", "x"]);
        let dir = scratch("rdel");
        sh(&dir, &["clone", "-q", &up.to_string_lossy(), "."]);
        sh(&up, &["checkout", "-q", "x"]);
        commit(&up, "a.txt", "teammate\n");
        sh(&up, &["checkout", "-q", "main"]);
        let theirs = sh(&up, &["rev-parse", "x"]);
        let oid = |dir: &std::path::Path| load_all(dir, 100, View::default()).unwrap().refs.iter().find(|r| r.name == "origin/x").unwrap().target.clone();
        let err = exec(&dir, &delete_branch("origin", "x", &oid(&dir))).unwrap_err();
        assert!(err.contains("origin/x wasn't deleted: it changed on the remote"), "{err}");
        assert_eq!(sh(&up, &["rev-parse", "x"]), theirs);
        sh(&dir, &["fetch", "-q", "origin"]);
        exec(&dir, &delete_branch("origin", "x", &oid(&dir))).unwrap();
        assert_eq!(sh(&up, &["branch", "--list", "x"]), "");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&up);
    }

    /// The background fetch rewrites FETCH_HEAD; one finishing mid-checkout must not
    /// reset pr/<n> to some other branch.
    #[test]
    fn pr_checkout_survives_a_concurrent_fetch() {
        let up = scratch("prrace-up");
        sh(&up, &["init", "-q", "-b", "main"]);
        identity(&up);
        commit(&up, "a.txt", "one\n");
        commit(&up, "a.txt", "pr work\n");
        sh(&up, &["update-ref", "refs/pull/7/head", "HEAD"]);
        let pr = sh(&up, &["rev-parse", "HEAD"]);
        sh(&up, &["reset", "-q", "--hard", "HEAD~1"]);
        let dir = scratch("prrace");
        sh(&dir, &["clone", "-q", &up.to_string_lossy(), "."]);
        let mut op = pr_checkout("origin");
        for a in op.cmds.iter_mut().flatten() {
            *a = a.replace(crate::repo::Prompt::VALUE, "7");
        }
        let mut first = op.clone();
        first.cmds.truncate(1);
        op.cmds.remove(0);
        exec(&dir, &first).unwrap();
        git::fetch_quiet(&dir).unwrap();
        exec(&dir, &op).unwrap();
        assert_eq!(sh(&dir, &["rev-parse", "pr/7"]), pr);
        assert_eq!(sh(&dir, &["rev-parse", "refs/glasir/pr/origin/7"]), pr);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&up);
    }

    /// The PR commit tracks a file that is ignored here: backed up before the checkout
    /// overwrites it (the backup is worked out after the fetch), so ⌘Z brings it back.
    #[test]
    fn pr_checkout_backs_up_an_ignored_file_it_tracks() {
        use crate::repo::testutil::settle;
        use crate::repo::{Action, RepoTab};
        let up = scratch("pr-ignored-up");
        sh(&up, &["init", "-q", "--bare", "-b", "main"]);
        let dir = scratch("pr-ignored");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, ".gitignore", ".env\n");
        sh(&dir, &["remote", "add", "origin", &up.to_string_lossy()]);
        sh(&dir, &["push", "-q", "origin", "main"]);
        sh(&dir, &["checkout", "-q", "-b", "pr"]);
        std::fs::write(dir.join(".env"), "committed\n").unwrap();
        sh(&dir, &["add", "-f", ".env"]);
        sh(&dir, &["commit", "-q", "-m", "pr"]);
        sh(&dir, &["push", "-q", "origin", "pr:refs/pull/1/head"]);
        sh(&dir, &["checkout", "-q", "main"]);
        sh(&dir, &["branch", "-D", "pr"]);
        std::fs::write(dir.join(".env"), "MINE\n").unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let mut op = pr_checkout("origin");
        for a in op.cmds.iter_mut().flatten() {
            *a = a.replace(crate::repo::Prompt::VALUE, "1");
        }
        t.apply(&ctx, Action::Git(op));
        settle(&ctx, &mut t);
        let read = || std::fs::read_to_string(dir.join(".env")).unwrap_or_default();
        assert_eq!(read(), "committed\n");
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!((sh(&dir, &["branch", "--show-current"]).trim(), read().as_str()), ("main", "MINE\n"));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&up);
    }
}
