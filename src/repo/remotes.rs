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
        let push_key = format!("remote.{old}.pushurl");
        fn set_push<'a>(old: &'a str, key: &'a str, p: &'a str) -> Vec<&'a str> {
            if p.is_empty() { vec!["config", "--unset-all", key] } else { vec!["remote", "set-url", "--push", old, p] }
        }
        let (mut fwd, mut back) = (vec![], vec![]);
        if url != old_url {
            fwd.push(vec!["remote", "set-url", old, url]);
            back.push(vec!["remote", "set-url", old, old_url]);
        }
        if push != old_push {
            fwd.push(set_push(old, &push_key, push));
            back.push(set_push(old, &push_key, old_push));
        }
        // Renamed last, so the URL commands above still use the old name (and first on the way back).
        if name != old {
            fwd.push(vec!["remote", "rename", old, name]);
            back.insert(0, vec!["remote", "rename", name, old]);
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
    back.extend(d.config.iter().filter(|(k, _)| k.starts_with(&prefix)).map(|(k, v)| add(k, v)));
    // `remote remove` also drops the upstream of every branch tracking it.
    for (k, _) in d.config.iter().filter(|(k, v)| k.starts_with("branch.") && k.ends_with(".remote") && v == name) {
        let merge = format!("{}.merge", k.trim_end_matches(".remote"));
        back.push(add(k, name));
        back.extend(d.cfg(&merge).map(|m| add(&merge, m)));
    }
    let refs: String = d.refs.iter().filter(|r| r.full.starts_with(&format!("refs/remotes/{name}/"))).map(|r| format!("create {} {}\n", r.full, r.target)).collect();
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
    let n = d.refs.iter().filter(|r| r.full.starts_with(&format!("refs/remotes/{name}/"))).count();
    Op::new(format!("Remove remote {name}"), &["remote", "remove", name])
        .confirm(format!("Remove remote {name} and its {n} remote-tracking branches? Local branches stay. (Undo with ⌘Z.)"))
        .revert(revert)
}

/// Fetch pull request `{}` (filled in by a prompt) from `remote` into `pr/{}` and check it
/// out. Works on github.com and GitHub Enterprise, which publish `refs/pull/<n>/head`.
pub fn pr_checkout(remote: &str) -> Op {
    Op::new("Check out pull request", &["fetch", "--progress", remote, "refs/pull/{}/head:refs/heads/pr/{}"])
        .then(&["checkout", "pr/{}", "--"])
        .undo(Undo::Keep)
}

fn pr_refspec(remote: &str) -> String {
    format!("+refs/pull/*/head:refs/remotes/{remote}/pr/*")
}

/// Whether `remote` fetches every pull request head as `<remote>/pr/<n>`.
pub fn pr_refs_on(d: &RepoData, remote: &str) -> bool {
    d.cfg_all(&format!("remote.{remote}.fetch")).any(|v| v == pr_refspec(remote))
}

/// Toggle fetching pull request heads; turning it off deletes the fetched `pr/*` refs.
pub fn pr_refs_op(d: &RepoData, remote: &str) -> Op {
    let (key, spec) = (format!("remote.{remote}.fetch"), pr_refspec(remote));
    if !pr_refs_on(d, remote) {
        return Op::new(format!("Fetch pull requests from {remote}"), &["config", "--add", &key, &spec]).then(&["fetch", "--progress", remote]);
    }
    let stale: String = d.refs.iter().filter(|r| r.full.starts_with(&format!("refs/remotes/{remote}/pr/"))).map(|r| format!("delete {}\n", r.full)).collect();
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

        // Remove, then undo: config, the branch's upstream and the tracking refs come back.
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
            *a = a.replace("{}", "7"); // what the prompt does
        }
        exec(&dir, &op).unwrap();
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), pr);
        assert_eq!(sh(&dir, &["branch", "--show-current"]).trim(), "pr/7");

        let load = || load_all(&dir, 100, View::default()).unwrap();
        assert!(!pr_refs_on(&load(), "origin"));
        exec(&dir, &pr_refs_op(&load(), "origin")).unwrap();
        let d = load();
        assert!(pr_refs_on(&d, "origin"));
        assert!(d.refs.iter().any(|r| r.name == "origin/pr/7" && r.target == pr.trim()));
        exec(&dir, &pr_refs_op(&d, "origin")).unwrap();
        let d = load();
        assert!(!pr_refs_on(&d, "origin") && !d.refs.iter().any(|r| r.name.starts_with("origin/pr/")));
        assert_eq!(d.cfg_all("remote.origin.fetch").collect::<Vec<_>>(), ["+refs/heads/*:refs/remotes/origin/*"]);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&up);
    }
}
