//! Loaded repository snapshot (`RepoData`), the graph filter (`View`) and
//! the parallel `load_all` that builds them.

use crate::git::{self, Commit, Ref, RefKind, Res, Status};
use crate::graph::{Graph, NONE};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct RepoData {
    pub log: Arc<Vec<Commit>>,
    pub refs: Arc<Vec<Ref>>,
    pub stashes: Arc<Vec<git::Stash>>,
    pub worktrees: Arc<Vec<git::Worktree>>,
    pub submodules: Arc<Vec<git::Submodule>>,
    pub lfs: Arc<git::Lfs>,
    pub health: super::maintenance::Health,
    pub remotes: Arc<Vec<(String, String)>>,
    /// `remote.*`, `branch.*`, `user.*`, `commit.template` and `core.comment*` config, all scopes in git's order (`cfg` looks keys up).
    pub config: Arc<Vec<(String, String)>>,
    /// (name, email) new commits are authored with (`git::author`).
    pub author: (String, String),
    pub git_dir: PathBuf,
    /// `origin/HEAD`'s branch name, if set.
    pub origin_head: String,
    /// History was cut at the commit limit (more can be loaded).
    pub truncated: bool,
    pub status: Status,
    pub state: git::State,
    /// While conflicted: names of the current and incoming side (branch / commit).
    pub conflict_sides: [String; 2],
    /// Conflicted files with every marker gone, which "Mark all resolved" stages.
    pub resolvable: Vec<String>,
    /// Bisect progress while `state` is `Bisecting`.
    pub bisect: Option<git::Bisect>,
    /// Row 0 is the uncommitted-changes pseudo commit.
    pub wip: bool,
    pub graph: Graph,
    /// Commit id -> display row.
    pub row_of: HashMap<String, usize>,
    /// Commit id -> indices into `refs`, most important first.
    pub refs_at: HashMap<String, Vec<usize>>,
    /// Base commit id -> indices into `stashes`.
    pub stash_at: HashMap<String, Vec<usize>>,
    /// The filter this history was loaded with.
    pub view: View,
    /// Stacked local branches (`stack`): child -> (parent, the parent tip it's built on).
    pub stack: Arc<super::stack::Stacks>,
    /// Unique per `build`: tells a reload apart even when it keeps the log.
    pub generation: u64,
    /// Tip row -> rows reachable from it (hover highlight).
    reach: std::sync::Mutex<HashMap<usize, Arc<Vec<bool>>>>,
}

/// Per-repo graph filter, persisted in `Settings` by repo path. Refs are full names.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct View {
    pub hidden: Vec<String>,
    pub solo: Vec<String>,
    pub first_parent: bool,
}

impl View {
    pub fn filtered(&self) -> bool {
        !self.hidden.is_empty() || !self.solo.is_empty()
    }

    pub fn shows(&self, full: &str) -> bool {
        if self.solo.is_empty() { !self.hidden.iter().any(|h| h == full) } else { self.solo.iter().any(|s| s == full) }
    }

    /// `git log` revisions: the soloed refs, or every branch/remote/tag minus the hidden ones, plus HEAD.
    pub(crate) fn revs(&self) -> Vec<String> {
        let mut a: Vec<String> = self.first_parent.then(|| "--first-parent".to_owned()).into_iter().collect();
        if !self.solo.is_empty() {
            a.push("--ignore-missing".into());
            a.extend(self.solo.iter().cloned());
            return a;
        }
        for (prefix, all) in [("refs/heads/", "--branches"), ("refs/remotes/", "--remotes"), ("refs/tags/", "--tags")] {
            a.extend(self.hidden.iter().filter_map(|h| h.strip_prefix(prefix)).map(|h| format!("--exclude={h}")));
            if all == "--remotes" {
                // origin/HEAD would bring a hidden origin/main back; remote HEADs are never shown anyway.
                a.push("--exclude=*/HEAD".into());
            }
            a.push(all.into());
        }
        // An unborn HEAD (orphan branch) is skipped instead of failing the walk.
        a.extend(["--ignore-missing".into(), "HEAD".into()]);
        a
    }
}

pub fn is_trunk(name: &str) -> bool {
    matches!(name, "main" | "master" | "develop" | "trunk")
}

impl RepoData {
    pub fn rows(&self) -> usize {
        self.log.len() + self.wip as usize
    }

    /// `None` for the WIP row.
    pub fn commit(&self, row: usize) -> Option<&Commit> {
        if self.wip && row == 0 { None } else { self.log.get(row - self.wip as usize) }
    }

    /// `r` as a revision: its short name (that's what merge messages quote), or its full ref when
    /// git would read the short one as something else (tag `x` beats branch `x`, local branch
    /// `origin/x` beats the remote's, `stash` / `ORIG_HEAD`-style names beat every branch).
    pub fn rev<'a>(&self, r: &'a Ref) -> &'a str {
        let special = !r.name.contains('/') && (r.name == "stash" || r.name.ends_with("HEAD"));
        if special || self.refs.iter().any(|o| o.name == r.name && o.full != r.full) { &r.full } else { &r.name }
    }

    pub fn local(&self, name: &str) -> Option<&Ref> {
        self.refs.iter().find(|r| r.kind == RefKind::Local && r.name == name)
    }

    /// Web URL of the main remote (github.com or GitHub Enterprise).
    pub fn web(&self) -> Option<String> {
        let r = self.remotes.iter().find(|r| r.0 == "origin").or(self.remotes.first())?;
        git::web_url(&r.1)
    }

    /// Effective value of a `remote.*` / `branch.*` / `user.*` key (variable names lower-case).
    pub fn cfg<'a>(&'a self, key: &str) -> Option<&'a str> {
        self.config.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// A boolean config value (keys as `--get-regexp` prints them: lowercase).
    pub fn cfg_bool(&self, key: &str) -> bool {
        self.cfg(key).is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "" | "true" | "yes" | "on" | "1"))
    }

    pub fn cfg_all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> {
        self.config.iter().filter(move |(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// Default folder for a new worktree: `<parent>/<repo>.worktrees/<name>`.
    pub fn worktree_path(&self, name: &str) -> String {
        let main = self.worktrees.first().map(|w| PathBuf::from(&w.path)).or_else(|| self.git_dir.parent().map(Path::to_path_buf)).unwrap_or_default();
        let repo = main.file_name().map_or("repo".into(), |n| n.to_string_lossy().into_owned());
        let dir = main.parent().unwrap_or(&main).join(format!("{repo}.worktrees")).join(name);
        dir.to_string_lossy().into_owned()
    }

    /// `(remote, web URL)` of every remote with a web page.
    pub fn webs(&self) -> Vec<(&str, String)> {
        self.remotes.iter().filter_map(|(name, url)| Some((name.as_str(), git::web_url(url)?))).collect()
    }

    /// Is commit `sha` in a remote-tracking branch of `remote`? (Else its web page 404s.)
    /// Commits outside the loaded history count as pushed: no false warning.
    pub fn on_remote(&self, sha: &str, remote: &str) -> bool {
        let Some(&row) = self.row_of.get(sha) else { return true };
        self.refs
            .iter()
            .filter(|r| r.kind == RefKind::Remote && self.split_remote(&r.name).is_some_and(|s| s.0 == remote))
            .filter_map(|r| self.row_of.get(&r.target))
            .any(|&tip| self.reachable(tip)[row])
    }

    pub fn web_of(&self, remote: &str) -> Option<String> {
        self.remotes.iter().find(|r| r.0 == remote).and_then(|r| git::web_url(&r.1)).or_else(|| self.web())
    }

    /// Rows reachable from `tip` among the loaded commits, cached per tip.
    pub fn reachable(&self, tip: usize) -> Arc<Vec<bool>> {
        let mut m = self.reach.lock().unwrap_or_else(|e| e.into_inner());
        m.entry(tip).or_insert_with(|| Arc::new(self.graph.reachable(tip))).clone()
    }

    /// Sidebar order: 0 = the remote's default branch, 1 = other long-lived
    /// branches (main, master, develop, trunk), 2 = everything else.
    pub fn branch_rank(&self, name: &str) -> u8 {
        if name == self.default_branch() {
            0
        } else if is_trunk(name) {
            1
        } else {
            2
        }
    }

    /// Where pushing `branch` goes as `(remote, branch)`: its upstream, unless that is a
    /// differently named long-lived branch it was started from (`checkout -b feat origin/main`),
    /// which must not get its commits: then a branch of its own name.
    pub fn push_dest<'a>(&self, branch: &'a str, upstream: &'a str) -> Option<(&'a str, &'a str)> {
        let (remote, rb) = self.split_remote(upstream)?;
        Some(if rb != branch && self.branch_rank(rb) < 2 { (remote, branch) } else { (remote, rb) })
    }

    /// `origin/feature/x` -> `("origin", "feature/x")`. A remote's name may contain `/`
    /// (`a/b/x` is x on remote a/b), so it is the longest remote `name` starts with.
    pub fn split_remote<'a>(&self, name: &'a str) -> Option<(&'a str, &'a str)> {
        split_remote(&self.remotes, name)
    }

    pub fn default_branch(&self) -> String {
        Some(self.origin_head.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let has = |n: &str| self.refs.iter().any(|r| r.kind == RefKind::Remote && r.name == n);
                ["origin/main", "origin/master"].into_iter().find(|n| has(n)).map(|n| n[7..].to_owned())
            })
            .unwrap_or_else(|| "main".into())
    }

    pub(super) fn load_conflicts(&mut self, dir: &Path) {
        if !self.status.conflicted.is_empty() {
            self.conflict_sides = super::merge_editor::labels(dir);
            self.resolvable = super::merge_editor::resolvable(dir, &self.status);
        }
    }

    pub(super) fn load_bisect(&mut self, dir: &Path) {
        self.bisect = (self.state == git::State::Bisecting).then(|| git::bisect(dir).ok()).flatten();
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn build(
        log: Arc<Vec<Commit>>,
        refs: Arc<Vec<Ref>>,
        stashes: Arc<Vec<git::Stash>>,
        remotes: Arc<Vec<(String, String)>>,
        git_dir: PathBuf,
        origin_head: String,
        status: Status,
        view: View,
    ) -> Self {
        let state = git::state(&git_dir);
        // An operation stopped with a clean tree (conflicts resolved to HEAD, a rebase `edit`
        // stop) still needs the WIP row: its banner is where it is continued or aborted.
        let wip = status.is_dirty() || !matches!(state, git::State::Clean | git::State::Bisecting);
        let off = wip as usize;
        let mut row_of: HashMap<String, usize> = log.iter().enumerate().map(|(i, c)| (c.id.clone(), i + off)).collect();
        // First-parent view: git still prints every parent; draw only the first.
        let keep = if view.first_parent { 1 } else { usize::MAX };
        let mut parents: Vec<Vec<u32>> =
            log.iter().map(|c| c.parents.iter().take(keep).map(|p| row_of.get(p).map_or(NONE, |&r| r as u32)).collect()).collect();
        let mut prio = vec![6u32; log.len()];
        let mut refs_at: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, r) in refs.iter().enumerate() {
            let p = match r.kind {
                RefKind::Local if r.head => 0,
                RefKind::Local if is_trunk(&r.name) => 1,
                RefKind::Local => 2,
                RefKind::Remote if split_remote(&remotes, &r.name).is_some_and(|(_, b)| is_trunk(b)) => 3,
                RefKind::Remote => 4,
                RefKind::Tag => 5,
            };
            if let Some(&row) = row_of.get(&r.target) {
                prio[row - off] = prio[row - off].min(p);
            }
            refs_at.entry(r.target.clone()).or_default().push(i);
        }
        for v in refs_at.values_mut() {
            v.sort_by_key(|&i| (!refs[i].head, refs[i].kind as u8, refs[i].name.clone()));
        }
        let head_row = status.head.as_ref().and_then(|h| row_of.get(h)).copied();
        if let Some(r) = head_row {
            prio[r - off] = 0;
        }
        if wip {
            parents.insert(0, vec![head_row.map_or(NONE, |r| r as u32)]);
            prio.insert(0, 0);
            row_of.insert(String::new(), 0);
        }
        let graph = Graph::layout(&parents, &prio);
        let mut stash_at: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, s) in stashes.iter().enumerate() {
            stash_at.entry(s.base.clone()).or_default().push(i);
        }
        Self {
            log,
            refs,
            stashes,
            worktrees: Default::default(),
            submodules: Default::default(),
            lfs: Default::default(),
            health: Default::default(),
            remotes,
            config: Default::default(),
            author: Default::default(),
            git_dir,
            origin_head,
            truncated: false,
            status,
            state,
            conflict_sides: Default::default(),
            resolvable: vec![],
            bisect: None,
            wip,
            graph,
            row_of,
            refs_at,
            stash_at,
            view,
            stack: Default::default(),
            generation: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
            reach: Default::default(),
        }
    }
}

fn split_remote<'a>(remotes: &[(String, String)], name: &'a str) -> Option<(&'a str, &'a str)> {
    let has = |r: &&str| name.strip_prefix(r).and_then(|b| b.strip_prefix('/')).is_some_and(|b| !b.is_empty());
    let n = remotes.iter().map(|r| r.0.as_str()).filter(has).map(str::len).max()?;
    Some((&name[..n], &name[n + 1..]))
}

pub(super) fn load_all(dir: &Path, limit: usize, view: View) -> Res<RepoData> {
    let git_dir = git::git_dir(dir)?;
    let revs = view.revs();
    let (status, refs, log, stashes, remotes) = std::thread::scope(|s| {
        let st = s.spawn(|| git::status(dir));
        let rf = s.spawn(|| git::refs(dir));
        let lg = s.spawn(|| {
            let r: Vec<&str> = revs.iter().map(String::as_str).collect();
            git::log(dir, limit, &r)
        });
        let sh = s.spawn(|| (git::stashes(dir).unwrap_or_default(), git::worktrees(dir).unwrap_or_default()));
        let rm = s.spawn(|| (git::remotes(dir).unwrap_or_default(), git::submodules(dir).unwrap_or_default(), git::lfs(dir), git::config(dir), git::author(dir), super::maintenance::health(dir)));
        (st.join().unwrap(), rf.join().unwrap(), lg.join().unwrap(), sh.join().unwrap(), rm.join().unwrap())
    });
    let (remotes, submodules, lfs, config, author, health) = remotes;
    let origin_head = git::run(dir, &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"])
        .map(|s| s.trim().trim_start_matches("origin/").to_owned())
        .unwrap_or_default();
    let (stashes, worktrees) = stashes;
    let log = log?;
    let truncated = log.len() >= limit;
    let mut d = RepoData::build(Arc::new(log), Arc::new(refs?), Arc::new(stashes), Arc::new(remotes), git_dir, origin_head, status?, view);
    d.truncated = truncated;
    d.worktrees = Arc::new(worktrees);
    d.submodules = Arc::new(submodules);
    d.lfs = Arc::new(lfs);
    d.config = Arc::new(config);
    d.author = author;
    d.health = health;
    d.load_bisect(dir);
    d.load_conflicts(dir);
    d.stack = Arc::new(super::stack::load(dir, &d));
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Change;
    use crate::repo::op::{Op, exec};
    use crate::repo::testutil::{commit, identity, scratch, sh};

    #[test]
    fn view_filters_history() {
        let dir = scratch("view");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "base\n");
        for b in ["feat", "spike"] {
            sh(&dir, &["checkout", "-q", "-b", b, "main"]);
            commit(&dir, &format!("{b}.txt"), &format!("{b} work\n"));
        }
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "m.txt", "main work\n");
        sh(&dir, &["merge", "-q", "--no-edit", "feat"]);
        sh(&dir, &["tag", "v1", "spike"]);
        let msgs = |v: View| {
            let d = load_all(&dir, 100, v).unwrap();
            assert!(d.graph.edges.iter().all(|e| e.to != NONE), "no dangling edges");
            d.log.iter().map(|c| c.summary.clone()).collect::<Vec<_>>()
        };
        let all = msgs(View::default());
        assert!(all.contains(&"spike work".to_owned()) && all.contains(&"feat work".to_owned()));

        // Hiding a branch and the tag on its tip drops its commits; hidden names never touch HEAD.
        let hide = |h: &[&str]| View { hidden: h.iter().map(|s| s.to_string()).collect(), ..Default::default() };
        assert!(msgs(hide(&["refs/heads/spike"])).contains(&"spike work".to_owned()), "tag v1 still shows it");
        let hidden = msgs(hide(&["refs/heads/spike", "refs/tags/v1", "refs/heads/main"]));
        assert!(!hidden.contains(&"spike work".to_owned()) && hidden.contains(&"main work".to_owned()));

        // Solo: only that ref's history; a deleted soloed ref is ignored.
        let solo = msgs(View { solo: vec!["refs/heads/spike".into(), "refs/heads/gone".into()], ..Default::default() });
        assert_eq!(solo, ["spike work", "base\n".trim()]);

        // First-parent: the merged-in commit disappears, the merge keeps one parent edge.
        let fp = msgs(View { first_parent: true, solo: vec!["refs/heads/main".into()], ..Default::default() });
        assert_eq!(fp.len(), 3);
        assert!(!fp.contains(&"feat work".to_owned()));

        // Hiding a remote's default branch: its origin/HEAD symref must not bring it back.
        commit(&dir, "o.txt", "only on origin\n");
        sh(&dir, &["remote", "add", "origin", &dir.to_string_lossy()]);
        sh(&dir, &["fetch", "-q", "origin"]);
        sh(&dir, &["remote", "set-head", "origin", "main"]);
        sh(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
        let only = "only on origin".to_owned();
        assert!(msgs(View::default()).contains(&only));
        assert!(!msgs(hide(&["refs/remotes/origin/main"])).contains(&only));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A link to a commit only works once a remote-tracking branch of that remote contains it.
    #[test]
    fn commit_on_remote_and_remote_webs() {
        let dir = scratch("onremote");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "one\n");
        let pushed = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        sh(&dir, &["remote", "add", "origin", "git@github.com:o/r.git"]);
        sh(&dir, &["remote", "add", "ghe", "https://ghe.corp.se/o/r.git"]);
        sh(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        commit(&dir, "a.txt", "two\n");
        let local = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        let d = load_all(&dir, 100, View::default()).unwrap();
        assert!(d.on_remote(&pushed, "origin") && !d.on_remote(&local, "origin"));
        assert!(!d.on_remote(&pushed, "ghe"), "no tracking branch of ghe");
        assert!(d.on_remote("f".repeat(40).as_str(), "origin"), "unknown commits don't warn");
        let webs: Vec<_> = d.webs().into_iter().map(|(r, w)| format!("{r} {w}")).collect();
        assert_eq!(webs, ["ghe https://ghe.corp.se/o/r", "origin https://github.com/o/r"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Bisect from two ends to the first bad commit, as the banner's buttons do it.
    #[test]
    fn bisect_finds_first_bad_commit() {
        let dir = scratch("bisect");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for i in 1..=12 {
            commit(&dir, "f.txt", &format!("{i}{}\n", if i >= 8 { " bug" } else { "" }));
        }
        let culprit = sh(&dir, &["rev-parse", "HEAD~4"]).trim().to_owned();
        let run = |args: &[&str]| exec(&dir, &Op::new("bisect", args)).unwrap();
        run(&["bisect", "start", "HEAD", "HEAD~11"]);
        let mut steps = 0;
        let found = loop {
            let d = load_all(&dir, 100, View::default()).unwrap();
            assert_eq!(d.state, git::State::Bisecting);
            let b = d.bisect.unwrap();
            if let Some(f) = b.found() {
                break f.to_owned();
            }
            assert!(b.left > 1, "{b:?}");
            steps += 1;
            let bad = std::fs::read_to_string(dir.join("f.txt")).unwrap().contains("bug");
            run(&["bisect", if bad { "bad" } else { "good" }]);
        };
        assert_eq!(found, culprit);
        assert!(steps <= 4, "{steps} steps");
        run(&["bisect", "reset"]);
        let d = load_all(&dir, 100, View::default()).unwrap();
        assert_eq!((d.state, d.bisect, d.status.branch.as_deref()), (git::State::Clean, None, Some("main")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stash_node_and_apply_one_file() {
        let dir = scratch("stashnode");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "one\n");
        commit(&dir, "b.txt", "two\n");
        let base = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        std::fs::write(dir.join("a.txt"), "stashed a\n").unwrap();
        std::fs::write(dir.join("b.txt"), "stashed b\n").unwrap();
        std::fs::write(dir.join("new.txt"), "untracked\n").unwrap();
        sh(&dir, &["stash", "push", "-q", "-u", "-m", "wip"]);
        commit(&dir, "c.txt", "later\n");

        let d = load_all(&dir, 100, View::default()).unwrap();
        let st = &d.stashes[0];
        assert_eq!(st.base, base);
        assert_eq!(d.stash_at[&base], [0]);
        let info = git::stash_info(&dir, &st.id).unwrap();
        let paths = |v: &[Change]| v.iter().map(|c| c.path.clone()).collect::<Vec<_>>();
        assert_eq!(paths(&info.files), ["a.txt", "b.txt"]);
        assert_eq!(paths(&info.untracked), ["new.txt"]);

        // Apply one tracked file and the untracked one; the rest of the tree and the stash stay put.
        let apply = |src: &str, path: &str| exec(&dir, &Op::new("Apply", &["--literal-pathspecs", "restore", "--source", src, "--worktree", "--", path])).unwrap();
        apply(&st.id, "a.txt");
        apply(&format!("{}^3", st.id), "new.txt");
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
        assert_eq!((read("a.txt").as_str(), read("b.txt").as_str(), read("new.txt").as_str()), ("stashed a\n", "two\n", "untracked\n"));
        assert_eq!(sh(&dir, &["diff", "--cached", "--name-only"]), "");
        assert_eq!(sh(&dir, &["stash", "list"]).lines().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tag (or a local `origin/x`) with a branch's short name must not stand in for it, and an
    /// unambiguous branch keeps its short name (it ends up in the merge message).
    #[test]
    fn rev_beats_same_named_tag_and_branch() {
        let up = scratch("rev-up");
        sh(&up, &["init", "-q", "-b", "main"]);
        identity(&up);
        commit(&up, "a.txt", "base\n");
        commit(&up, "u.txt", "upstream\n");
        let dir = scratch("rev");
        sh(&up, &["clone", "-q", &up.to_string_lossy(), &dir.to_string_lossy()]);
        identity(&dir);
        sh(&dir, &["branch", "origin/main", "HEAD~1"]);
        sh(&dir, &["checkout", "-q", "-b", "release-1"]);
        commit(&dir, "r.txt", "r1\n");
        sh(&dir, &["tag", "release-1"]);
        commit(&dir, "r.txt", "r1 fix\n");
        let tip = sh(&dir, &["rev-parse", "refs/heads/release-1"]);
        sh(&dir, &["checkout", "-q", "-b", "feat", "main"]);
        commit(&dir, "f.txt", "feat\n");
        sh(&dir, &["checkout", "-q", "-b", "work", "main~1"]);
        commit(&dir, "w.txt", "work\n");
        let d = load_all(&dir, 100, View::default()).unwrap();
        let rev = |kind, name: &str| d.rev(d.refs.iter().find(|r| r.kind == kind && r.name == name).unwrap()).to_owned();

        exec(&dir, &Op::new("Merge", &["merge", "--no-edit", &rev(RefKind::Remote, "origin/main")])).unwrap();
        assert!(git::run(&dir, &["merge-base", "--is-ancestor", "refs/remotes/origin/main", "HEAD"]).is_ok());
        exec(&dir, &Op::new("Merge", &["merge", "--no-edit", &rev(RefKind::Local, "feat")])).unwrap();
        assert_eq!(sh(&dir, &["log", "-1", "--format=%s"]), "Merge branch 'feat' into work\n");
        exec(&dir, &Op::new("Merge", &["merge", "--no-edit", &rev(RefKind::Local, "release-1")])).unwrap();
        assert!(git::run(&dir, &["merge-base", "--is-ancestor", tip.trim(), "HEAD"]).is_ok());
        // `rebase <onto> <branch>` takes the short name (a branch first); `refs/heads/feat` would detach.
        sh(&dir, &["tag", "feat", "main~1"]);
        exec(&dir, &Op::new("Rebase", &["rebase", &rev(RefKind::Local, "release-1"), "feat"])).unwrap();
        assert_eq!(sh(&dir, &["rev-parse", "refs/heads/feat~1"]), tip);
        assert_eq!(sh(&dir, &["symbolic-ref", "HEAD"]), "refs/heads/feat\n");
        let _ = std::fs::remove_dir_all(&up);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
