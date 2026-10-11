//! "Clean up branches": local branches merged into a target (plainly, by squash or by
//! rebase), whose upstream is gone, or stale, plus stale remote-tracking refs to prune.

use super::data::is_trunk;
use super::{RepoData, RepoTab, Task, delete_branches};
use crate::git::{self, RefKind, Res};
use crate::repo::Op;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// Why a branch is offered, in display order.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Why {
    Merged,
    Squashed,
    Gone,
    Stale,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cand {
    pub name: String,
    /// The tip that was classified; the delete refuses if the branch moved since.
    pub oid: String,
    /// `None`: still active, offered as Stale once older than the chosen days.
    pub why: Option<Why>,
    pub note: String,
    pub date: i64,
    pub ahead: u32,
    pub behind: u32,
    pub on: bool,
    /// Same-named upstream `(remote, branch, oid)` whose commits are all in the local
    /// branch, so "also delete on remote" loses nothing ⌘Z can't bring back locally.
    pub remote: Option<(String, String, String)>,
}

#[derive(Default)]
pub struct Cleanup {
    pub target: String,
    /// Branches the target can be switched to.
    pub targets: Vec<String>,
    /// `None` while scanning.
    pub rows: Option<Res<Vec<Cand>>>,
    pub remote: bool,
    pub prune: bool,
    /// Remote-tracking refs whose branch is gone from its remote: `(ref, oid)`; `None` while checking.
    pub stale_refs: Option<Res<Vec<(String, String)>>>,
    task: Option<Task<Res<Vec<Cand>>>>,
    prune_task: Option<Task<Res<Vec<(String, String)>>>>,
}

/// `release/*`-style pattern: a trailing `*` matches any rest, otherwise the exact name.
fn matches(pat: &str, name: &str) -> bool {
    pat.strip_suffix('*').map_or(pat == name, |p| name.starts_with(p))
}

/// A long-lived branch: a trunk name, `release/*` or one of Settings → Protected branches (`keep`).
pub(super) fn protected(keep: &str, name: &str) -> bool {
    is_trunk(name) || keep.split([',', ' ', '\n']).filter(|p| !p.is_empty()).chain(["release/*"]).any(|p| matches(p, name))
}

impl Cleanup {
    pub fn poll(&mut self) {
        if let Some(r) = Task::poll(&mut self.task) {
            self.rows = Some(r);
        }
        if let Some(r) = Task::poll(&mut self.prune_task) {
            self.stale_refs = Some(r);
        }
    }

    /// The group a row shows in, if any: stale = no commit in `days` days (as of `now`).
    pub fn group(c: &Cand, days: u32, now: i64) -> Option<Why> {
        c.why.or((now - c.date >= days as i64 * 86400).then_some(Why::Stale))
    }

    /// Ticked rows that are shown: a row hidden by a longer stale period is never deleted.
    pub fn chosen(&self, days: u32, now: i64) -> Vec<&Cand> {
        let rows = self.rows.as_ref().and_then(|r| r.as_ref().ok());
        rows.into_iter().flatten().filter(|c| c.on && Self::group(c, days, now).is_some()).collect()
    }

    pub fn pruned(&self) -> &[(String, String)] {
        match (self.prune, &self.stale_refs) {
            (true, Some(Ok(v))) => v,
            _ => &[],
        }
    }

    /// Remote branches "also delete on remote" would delete: `(remote, branch, oid)`.
    pub fn remote_deletes(&self, days: u32, now: i64) -> Vec<&(String, String, String)> {
        let pruned = self.pruned();
        let gone = |(r, b, _): &&(String, String, String)| pruned.iter().any(|(p, _)| *p == format!("refs/remotes/{r}/{b}"));
        self.chosen(days, now).into_iter().filter_map(|c| c.remote.as_ref()).filter(|x| !gone(x)).collect()
    }

    /// Deletes the chosen branches (undoable with their config), prunes the stale
    /// remote-tracking refs (undoable) and, if asked, the remote branches (not undoable: confirmed).
    pub fn op(&self, d: &RepoData, days: u32, now: i64) -> Res<Op> {
        let rows = self.chosen(days, now);
        if let Some(c) = rows.iter().find(|c| d.local(&c.name).is_none_or(|r| r.target != c.oid)) {
            return Err(format!("{} changed since the list was made, so nothing was deleted. Open Clean up branches again.", c.name));
        }
        let names: Vec<&str> = rows.iter().map(|c| c.name.as_str()).collect();
        let mut op = delete_branches(d, "Clean up branches".into(), &names, true);
        let back = op.revert.as_mut().expect("a branch delete is reverted by restoring");
        for (r, o) in self.pruned() {
            op.cmds.push(["update-ref", "-d", r, o].map(String::from).to_vec());
            back.cmds.push(["update-ref", r, o].map(String::from).to_vec());
        }
        if !self.remote {
            return Ok(op);
        }
        let mut by: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
        for (r, b, o) in self.remote_deletes(days, now) {
            by.entry(r).or_default().push((b, o));
        }
        let mut shown = vec![];
        for (r, list) in &by {
            // The lease refuses a remote branch someone pushed to since it was fetched.
            let mut a = vec!["push".to_owned(), r.to_string()];
            a.extend(list.iter().map(|(b, o)| format!("--force-with-lease=refs/heads/{b}:{o}")));
            a.push("--delete".into());
            a.extend(list.iter().map(|(b, _)| format!("refs/heads/{b}")));
            op.cmds.push(a);
            shown.extend(list.iter().map(|(b, _)| format!("{r}/{b}")));
        }
        if shown.is_empty() {
            return Ok(op);
        }
        Ok(op.confirm(format!(
            "Also delete {} on the remote? ⌘Z brings back the local branches only: a remote delete can't be undone (push a restored branch to bring it back).",
            shown.join(", ")
        )))
    }
}

/// Ahead/behind of every local branch against `target`, from one `for-each-ref` (git 2.41+);
/// empty on older git, where `classify` counts per branch.
fn counts(dir: &Path, target: &str) -> HashMap<String, (u32, u32)> {
    let out = git::run(dir, &["for-each-ref", &format!("--format=%(refname:lstrip=2)%1f%(ahead-behind:{target})"), "refs/heads"]).unwrap_or_default();
    let n = |s: Option<&str>| s.and_then(|s| s.parse().ok()).unwrap_or(0);
    let pair = |l: &str| {
        let (name, ab) = l.split_once('\x1f')?;
        let mut ab = ab.split_whitespace();
        Some((name.to_owned(), (n(ab.next()), n(ab.next()))))
    };
    out.lines().filter_map(pair).collect()
}

/// For each of `tips`, whether merging it into `target` gives exactly `target`'s tree: all the
/// merges in one `merge-tree --stdin` (git 2.40+). `None` if the output doesn't parse.
fn same_tree(dir: &Path, target: &str, tree: &str, tips: &[&str]) -> Option<Vec<bool>> {
    let input: String = tips.iter().map(|t| format!("{target} {t}\n")).collect();
    let out = git::run_input(dir, &["merge-tree", "--stdin", "--write-tree", "--no-messages", "-z"], Some(input.as_bytes())).ok()?;
    // Per merge: status NUL tree NUL, then conflicted-file entries up to an empty one.
    let mut f = out.split('\0');
    let mut res = vec![];
    while res.len() < tips.len() {
        let (status, t) = (f.next()?, f.next()?);
        while !f.next()?.is_empty() {}
        res.push(status == "1" && t == tree);
    }
    Some(res)
}

/// What `git cherry <target> <tip>` decides for every tip at once: all its non-merge commits
/// outside `target` have a patch-identical twin among the target commits it lacks. Patch-ids
/// come from one `diff-tree` + one `patch-id` over the commits since the tips' common
/// ancestor. `None` (ask `git cherry` per branch) if there is none or the range is big.
fn twins(dir: &Path, target: &str, tips: &[&str]) -> Option<HashMap<String, bool>> {
    if tips.is_empty() || tips.len() > 5000 {
        return None;
    }
    let base = git::run(dir, &[&["merge-base", "--octopus", target][..], tips].concat()).ok()?;
    let mut ours = git::run(dir, &["rev-list", "--parents", target, "--not", base.trim()]).ok()?;
    let nt = ours.lines().count();
    ours += &git::run(dir, &[&["rev-list", "--parents"][..], tips, &["--not", target]].concat()).ok()?;
    if nt > 20_000 || ours.lines().count() > 50_000 {
        return None;
    }
    let parents: HashMap<&str, Vec<&str>> = ours.lines().map(|l| {
        let mut w = l.split_whitespace();
        (w.next().unwrap_or(""), w.collect())
    }).collect();
    let singles: String = parents.iter().filter(|(_, p)| p.len() < 2).map(|(c, _)| format!("{c}\n")).collect();
    let diffs = git::run_input(dir, &["diff-tree", "--stdin", "-p", "--full-index", "--root"], Some(singles.as_bytes())).ok()?;
    let ids = git::run_input(dir, &["patch-id", "--stable"], Some(diffs.as_bytes())).ok()?;
    let pid: HashMap<&str, &str> = ids.lines().filter_map(|l| l.split_once(' ').map(|(p, c)| (c, p))).collect();
    // Patch-id -> the target commits (inside the range) having it.
    let t: HashSet<&str> = ours.lines().take(nt).filter_map(|l| l.split_whitespace().next()).collect();
    let mut by_patch: HashMap<&str, Vec<&str>> = HashMap::new();
    for c in &t {
        if let Some(p) = pid.get(c) {
            by_patch.entry(p).or_default().push(c);
        }
    }
    Some(tips.iter().map(|tip| {
        let (mut seen, mut todo) = (HashSet::new(), vec![*tip]);
        while let Some(c) = todo.pop() {
            if let Some(ps) = parents.get(c).filter(|_| seen.insert(c)) {
                todo.extend(ps);
            }
        }
        // The tip's own commits: reached, single-parent, not in the target's range.
        let mine: Vec<&&str> = seen.iter().filter(|c| !t.contains(**c) && parents[**c].len() < 2).collect();
        let twin = |c: &&&str| pid.get(**c).is_some_and(|p| by_patch.get(p).is_some_and(|v| v.iter().any(|y| !seen.contains(y))));
        (tip.to_string(), !mine.is_empty() && mine.iter().all(twin))
    }).collect())
}

/// Commits of each branch tip that are on no remote and not in `target`, from one `rev-list`:
/// `None` when that set is too big to walk per branch in memory.
fn unpushed(dir: &Path, target: &str, tips: &[&str]) -> Option<HashMap<String, u32>> {
    let all = git::run(dir, &["rev-list", "--parents", "--branches", "--not", "--remotes", target]).ok()?;
    if all.lines().count() > 50_000 {
        return None;
    }
    let parents: HashMap<&str, Vec<&str>> = all.lines().map(|l| {
        let mut w = l.split_whitespace();
        (w.next().unwrap_or(""), w.collect())
    }).collect();
    Some(tips.iter().map(|t| {
        let (mut seen, mut todo) = (HashSet::new(), vec![*t]);
        while let Some(c) = todo.pop() {
            if let Some(ps) = parents.get(c).filter(|_| seen.insert(c)) {
                todo.extend(ps);
            }
        }
        (t.to_string(), seen.len() as u32)
    }).collect())
}

/// Classifies `rows` (name, tip, date and remote candidate filled in; with "upstream gone")
/// against `target`. Counts, merge results and unpushed commits come from a few bulk git
/// commands; few commands run per branch.
fn scan(dir: &Path, target: &str, mut rows: Vec<(Cand, bool)>) -> Res<Vec<Cand>> {
    let merged = git::run(dir, &["for-each-ref", &format!("--merged={target}"), "--format=%(objectname)", "refs/heads"])?;
    let merged: HashSet<String> = merged.lines().map(String::from).collect();
    let tree = git::run(dir, &["rev-parse", "--verify", "-q", &format!("{target}^{{tree}}")])?;
    let tree = tree.trim();
    let counts = counts(dir, target);
    let open: Vec<String> = rows.iter().map(|r| r.0.oid.clone()).filter(|o| !merged.contains(o)).collect();
    let open: Vec<&str> = open.iter().map(String::as_str).collect();
    let same = same_tree(dir, target, tree, &open).map(|v| open.iter().map(|o| o.to_string()).zip(v).collect::<HashMap<_, _>>());
    let twins = twins(dir, target, &open);
    let unpushed = unpushed(dir, target, &open);
    let s = |n: u32| if n == 1 { "" } else { "s" };
    let classify = |(c, gone): &mut (Cand, bool)| {
            let gone = *gone;
            (c.ahead, c.behind) = counts.get(&c.name).copied().unwrap_or_else(|| {
                let lr = git::run(dir, &["rev-list", "--left-right", "--count", &format!("{target}...{}", c.oid)]).unwrap_or_default();
                let mut n = lr.split_whitespace().filter_map(|n| n.parse().ok());
                let behind = n.next().unwrap_or(0);
                (n.next().unwrap_or(0), behind)
            });
            let same = || match &same {
                Some(m) => m.get(c.oid.as_str()).copied().unwrap_or(false),
                None => git::run(dir, &["merge-tree", "--write-tree", target, &c.oid]).is_ok_and(|o| o.lines().next() == Some(tree)),
            };
            let twin = || match &twins {
                Some(m) => m.get(&c.oid).copied().unwrap_or(false),
                None => git::run(dir, &["cherry", target, &c.oid]).is_ok_and(|o| !o.is_empty() && o.lines().all(|l| l.starts_with('-'))),
            };
            let changes = || git::run(dir, &["diff", "--quiet", &format!("{target}...{}", c.oid), "--"]).is_err();
            if merged.contains(&c.oid) {
                (c.why, c.on) = (Some(Why::Merged), true);
            } else if same() && let Some(how) = if twin() { Some("rebase") } else { changes().then_some("squash") } {
                (c.why, c.on, c.note) = (Some(Why::Squashed), true, format!("{how}-merged · the content of its {} commit{} is in the target", c.ahead, s(c.ahead)));
            } else {
                let n = match unpushed.as_ref().and_then(|u| u.get(&c.oid)) {
                    Some(n) => *n,
                    None => git::run(dir, &["rev-list", "--count", &c.oid, "--not", "--remotes", target])
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok())
                        .unwrap_or(1),
                };
                c.why = gone.then_some(Why::Gone);
                c.on = false;
                c.note = match (gone, n) {
                    (true, 0) => format!("upstream deleted · {} commit{} not in the target, still on a remote", c.ahead, s(c.ahead)),
                    (true, n) => format!("upstream deleted · {n} commit{} not on any remote", s(n)),
                    (false, 0) => String::new(),
                    (false, n) => format!("{n} commit{} not on any remote", s(n)),
                };
            }
            c.remote = c.remote.take().filter(|(_, _, o)| *o == c.oid || git::run(dir, &["merge-base", "--is-ancestor", o, &c.oid]).is_ok());
    };
    // Only the `diff --quiet` and remote checks (and fallbacks) run per branch: spread them over a few threads.
    let per = rows.len().div_ceil(std::thread::available_parallelism().map_or(4, |n| n.get().min(8))).max(1);
    std::thread::scope(|sc| {
        for chunk in rows.chunks_mut(per) {
            sc.spawn(|| chunk.iter_mut().for_each(classify));
        }
    });
    let mut out: Vec<Cand> = rows.into_iter().map(|r| r.0).collect();
    out.sort_by_key(|c| c.why.map_or(4, |w| w as u8));
    Ok(out)
}

/// Remote-tracking refs whose source no longer exists on the remote (what `git remote prune`
/// drops): mapped back through the remote's fetch refspecs, so `pr/*` from `refs/pull/*` count.
fn stale_refs(dir: &Path, remotes: &[String]) -> Res<Vec<(String, String)>> {
    let mut out = vec![];
    for r in remotes {
        let specs = git::run(dir, &["config", "--get-all", &format!("remote.{r}.fetch")]).unwrap_or_default();
        let all = git::run(dir, &["ls-remote", r])?;
        let have: HashSet<&str> = all.lines().filter_map(|l| Some(l.split_once('\t')?.1)).collect();
        let prefix = format!("refs/remotes/{r}/");
        let local = git::run(dir, &["for-each-ref", "--format=%(refname)%1f%(objectname)", &prefix])?;
        for (full, oid) in local.lines().filter_map(|l| l.split_once('\x1f')) {
            let b = &full[prefix.len()..];
            // `refs/remotes/a/b/x` may belong to a remote named `a/b`.
            let other = remotes.iter().any(|o| o.len() > r.len() && full.starts_with(&format!("refs/remotes/{o}/")));
            // A ref no refspec maps to isn't fetched, so git leaves it alone too.
            let srcs: Vec<String> = specs.lines().filter_map(|s| source(s, full)).collect();
            if b != "HEAD" && !other && !srcs.is_empty() && !srcs.iter().any(|s| have.contains(s.as_str())) {
                out.push((full.to_owned(), oid.to_owned()));
            }
        }
    }
    Ok(out)
}

/// The remote ref a fetch refspec (`+refs/heads/*:refs/remotes/o/*`) maps to the local `full`.
fn source(spec: &str, full: &str) -> Option<String> {
    let (src, dst) = spec.trim().trim_start_matches('+').split_once(':')?;
    let s = match (src.split_once('*'), dst.split_once('*')) {
        (Some((sp, ss)), Some((dp, ds))) => format!("{sp}{}{ss}", full.strip_prefix(dp)?.strip_suffix(ds)?),
        (None, None) if full == dst => src.to_owned(),
        _ => return None,
    };
    s.starts_with("refs/").then_some(s)
}

impl RepoTab {
    /// Opens the dialog; the dialog clears `rows` to rescan against a changed target, and
    /// ticking "prune" checks the remotes (network) the first time.
    pub(super) fn open_cleanup(&mut self, ctx: &egui::Context) {
        let Some(d) = &self.data else { return };
        let def = d.default_branch().to_owned();
        let mut c = self.cleanup.take().unwrap_or_default();
        let tracked: Vec<_> = d.refs.iter().filter(|r| matches!(r.kind, RefKind::Local | RefKind::Remote) && !r.name.ends_with("/HEAD")).collect();
        c.targets = tracked.iter().map(|r| r.name.clone()).collect();
        if !c.targets.contains(&c.target) {
            let pick = [format!("origin/{def}"), def.clone()].into_iter().find(|t| c.targets.contains(t));
            c.target = pick.or_else(|| c.targets.iter().find(|t| is_trunk(t)).cloned()).unwrap_or(def.clone());
        }
        let target = c.target.clone();
        let full = tracked.iter().find(|r| r.name == target).map_or(target.clone(), |r| r.full.clone());
        let target_branch = d.split_remote(&target).map_or(target.as_str(), |(_, b)| b);
        let kept = |n: &str| protected(&self.keep_branches, n) || n == def || n == target || n == target_branch;
        // A branch being rebased or bisected has no HEAD marker, but git refuses to delete it.
        let busy: Vec<String> = ["rebase-merge/head-name", "rebase-apply/head-name", "BISECT_START"]
            .iter()
            .filter_map(|f| std::fs::read_to_string(d.git_dir.join(f)).ok())
            .map(|s| s.trim().trim_start_matches("refs/heads/").to_owned())
            .collect();
        let rows: Vec<(Cand, bool)> = d
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Local && !r.head && r.worktree.is_none() && !kept(&r.name) && !busy.contains(&r.name))
            .map(|r| {
                let up = d.split_remote(&r.upstream).filter(|(_, b)| !r.gone && *b == r.name);
                let oid = up.and_then(|_| d.refs.iter().find(|x| x.kind == RefKind::Remote && x.name == r.upstream)).map(|x| x.target.clone());
                let remote = up.zip(oid).map(|((rem, b), o)| (rem.to_owned(), b.to_owned(), o));
                (Cand { name: r.name.clone(), oid: r.target.clone(), date: r.date, remote, ..Default::default() }, r.gone)
            })
            .collect();
        if c.rows.is_none() {
            let dir = self.dir.clone();
            c.task = Some(Task::spawn(ctx, move || scan(&dir, &full, rows)));
        }
        if c.prune && c.stale_refs.is_none() && c.prune_task.is_none() {
            let (dir, remotes) = (self.dir.clone(), d.remotes.iter().map(|r| r.0.clone()).collect::<Vec<_>>());
            c.prune_task = Some(Task::spawn(ctx, move || stale_refs(&dir, &remotes)));
        }
        self.cleanup = Some(c);
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{commit, identity, scratch, settle, sh};
    use super::super::{Action, View};
    use super::*;

    const NOW: i64 = i64::MAX / 2;

    fn scanned(ctx: &egui::Context, t: &mut RepoTab) {
        t.apply(ctx, Action::Cleanup);
        super::super::testutil::wait_for("the scan", || {
            settle(ctx, t);
            let c = t.cleanup.as_ref().unwrap();
            c.rows.is_some() && (!c.prune || c.stale_refs.is_some())
        });
    }

    /// `(name, group, note, pre-selected)` of the rows shown with a 90-day stale period.
    fn shown(t: &RepoTab, now: i64) -> Vec<(String, Why, String, bool)> {
        let rows = t.cleanup.as_ref().unwrap().rows.as_ref().unwrap().as_ref().unwrap();
        rows.iter().filter_map(|c| Some((c.name.clone(), Cleanup::group(c, 90, now)?, c.note.clone(), c.on))).collect()
    }

    fn now() -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
    }

    /// The remote's default branch, protected patterns and active work are never offered.
    #[test]
    fn keeps_the_default_branch() {
        let root = scratch("cleanup");
        sh(&root, &["init", "-q", "--bare", "-b", "stable", "remote.git"]);
        sh(&root, &["clone", "-q", "remote.git", "a"]);
        let a = root.join("a");
        identity(&a);
        commit(&a, "f.txt", "one\n");
        sh(&a, &["push", "-q", "-u", "origin", "stable"]);
        sh(&a, &["remote", "set-head", "origin", "stable"]);
        for b in ["done", "release/1", "hotfix/x"] {
            sh(&a, &["branch", b]);
        }
        sh(&a, &["switch", "-q", "-c", "feature"]);
        commit(&a, "g.txt", "two\n");
        sh(&a, &["switch", "-q", "stable"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, a.clone(), 100, View::default());
        t.keep_branches = "hotfix/*".into();
        settle(&ctx, &mut t);
        scanned(&ctx, &mut t);
        assert_eq!(t.cleanup.as_ref().unwrap().target, "origin/stable");
        assert_eq!(shown(&t, now()), [("done".to_owned(), Why::Merged, String::new(), true)]);
        // Untouched for long, `feature` (1 ahead, unpushed) is offered as stale, but unticked.
        let f = &t.cleanup.as_ref().unwrap().rows.as_ref().unwrap().as_ref().unwrap()[1];
        assert_eq!((f.name.as_str(), f.ahead, f.behind, f.note.as_str()), ("feature", 1, 0, "1 commit not on any remote"));
        assert_eq!(shown(&t, NOW)[1], ("feature".to_owned(), Why::Stale, "1 commit not on any remote".to_owned(), false));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A branch whose upstream was deleted is only pre-selected when no commit of it
    /// would be lost.
    #[test]
    fn gone_branch_with_unpushed_work_is_not_preselected() {
        let root = scratch("cleanup-gone");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        sh(&root, &["clone", "-q", "remote.git", "a"]);
        let a = root.join("a");
        identity(&a);
        commit(&a, "f.txt", "one\n");
        sh(&a, &["push", "-q", "-u", "origin", "main"]);
        for b in ["feature", "pushed"] {
            sh(&a, &["switch", "-q", "-c", b]);
            commit(&a, &format!("{b}.txt"), "pushed\n");
            // Also kept under another name, so its pushed commit stays on a remote.
            sh(&a, &["push", "-q", "-u", "origin", b]);
            sh(&a, &["push", "-q", "origin", &format!("{b}:{b}-copy")]);
        }
        sh(&a, &["switch", "-q", "feature"]);
        commit(&a, "feature.txt", "unpushed\n");
        sh(&a, &["switch", "-q", "main"]);
        sh(&a, &["push", "-q", "origin", "--delete", "feature", "pushed"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, a.clone(), 100, View::default());
        settle(&ctx, &mut t);
        scanned(&ctx, &mut t);
        assert_eq!(
            shown(&t, now()),
            [
                ("feature".to_owned(), Why::Gone, "upstream deleted · 1 commit not on any remote".to_owned(), false),
                ("pushed".to_owned(), Why::Gone, "upstream deleted · 2 commits not in the target, still on a remote".to_owned(), false),
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Squash- and rebase-merged branches (which `--merged` misses) are found and
    /// pre-selected; unmerged work isn't. Deleting them is undoable.
    #[test]
    fn squash_and_rebase_merged_branches_are_detected() {
        let dir = scratch("cleanup-squash");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base\n");
        sh(&dir, &["switch", "-q", "-c", "sq"]);
        commit(&dir, "s1", "one\n");
        commit(&dir, "s2", "two\n");
        sh(&dir, &["switch", "-q", "-c", "rb", "main"]);
        commit(&dir, "r", "rebased\n");
        sh(&dir, &["switch", "-q", "-c", "wip", "main"]);
        commit(&dir, "w", "work\n");
        sh(&dir, &["switch", "-q", "main"]);
        commit(&dir, "m", "main moved on\n");
        sh(&dir, &["merge", "-q", "--squash", "sq"]);
        sh(&dir, &["commit", "-q", "-m", "Squashed sq"]);
        sh(&dir, &["cherry-pick", "rb"]);
        commit(&dir, "m", "after\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        scanned(&ctx, &mut t);
        let sq = |n: &str, how: &str| (n.to_owned(), Why::Squashed, how.to_owned(), true);
        assert_eq!(
            shown(&t, now()),
            [sq("rb", "rebase-merged · the content of its 1 commit is in the target"), sq("sq", "squash-merged · the content of its 2 commits is in the target")]
        );
        let c = t.cleanup.as_ref().unwrap();
        let s = c.rows.as_ref().unwrap().as_ref().unwrap().iter().find(|c| c.name == "sq").unwrap();
        assert_eq!((s.ahead, s.behind), (2, 4));
        assert_eq!(c.chosen(90, now()).len(), 2, "wip is neither shown nor ticked");
        let op = c.op(t.data.as_ref().unwrap(), 90, now()).unwrap();
        assert!(op.confirm.is_none());
        t.apply(&ctx, Action::Git(op));
        settle(&ctx, &mut t);
        let branches = || sh(&dir, &["for-each-ref", "--format=%(refname:short)", "refs/heads"]);
        assert_eq!(branches(), "main\nwip\n");
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(branches(), "main\nrb\nsq\nwip\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A branch with no net change (empty commits, a commit and its revert) also merges
    /// to the target's tree, but was never merged: it isn't offered as squash-merged.
    #[test]
    fn net_empty_branches_are_not_squash_merged() {
        let dir = scratch("cleanup-net-empty");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base\n");
        sh(&dir, &["switch", "-q", "-c", "empty"]);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "empty"]);
        sh(&dir, &["switch", "-q", "-c", "reverted", "main"]);
        commit(&dir, "x", "x\n");
        sh(&dir, &["revert", "--no-edit", "HEAD"]);
        sh(&dir, &["switch", "-q", "main"]);
        commit(&dir, "m", "main moved on\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        scanned(&ctx, &mut t);
        assert_eq!(shown(&t, now()), []);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// "Also delete on remote" deletes the merged branch's upstream (after a confirm saying
    /// it can't be undone); "prune" drops remote-tracking refs gone from the remote, undoably.
    #[test]
    fn deletes_on_remote_and_prunes() {
        let root = scratch("cleanup-remote");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        sh(&root, &["clone", "-q", "remote.git", "a"]);
        let a = root.join("a");
        identity(&a);
        commit(&a, "f.txt", "one\n");
        sh(&a, &["push", "-q", "-u", "origin", "main"]);
        sh(&a, &["remote", "set-head", "origin", "main"]);
        sh(&a, &["push", "-q", "-u", "origin", "main:feat", "main:old"]);
        sh(&a, &["branch", "-q", "--track", "feat", "origin/feat"]);
        sh(&root.join("remote.git"), &["branch", "-D", "old"]);
        let old = sh(&a, &["rev-parse", "origin/old"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, a.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let c = t.cleanup.get_or_insert_default();
        (c.prune, c.remote) = (true, true);
        scanned(&ctx, &mut t);
        let c = t.cleanup.as_ref().unwrap();
        assert_eq!(c.pruned(), [("refs/remotes/origin/old".to_owned(), old.trim().to_owned())]);
        assert_eq!(c.remote_deletes(90, now()).len(), 1);
        t.apply(&ctx, Action::Git(c.op(t.data.as_ref().unwrap(), 90, now()).unwrap()));
        let op = t.confirm.take().expect("a remote delete asks first");
        assert!(op.confirm.as_ref().unwrap().contains("origin/feat"));
        t.apply(&ctx, Action::Git(Op { confirm: None, ..op }));
        settle(&ctx, &mut t);
        let refs = || sh(&a, &["for-each-ref", "--format=%(refname)", "refs/heads", "refs/remotes"]);
        assert_eq!(refs(), "refs/heads/main\nrefs/remotes/origin/HEAD\nrefs/remotes/origin/main\n");
        assert_eq!(sh(&root.join("remote.git"), &["branch", "--format=%(refname:short)"]), "main\n");
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(refs(), "refs/heads/feat\nrefs/heads/main\nrefs/remotes/origin/HEAD\nrefs/remotes/origin/main\nrefs/remotes/origin/old\n");
        assert_eq!(sh(&a, &["config", "branch.feat.remote"]), "origin\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The bulk merge-tree/ahead-behind/unpushed helpers agree with the facts, conflicts included.
    #[test]
    fn bulk_helpers_parse_conflicts() {
        let dir = scratch("cleanup-bulk");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base\n");
        sh(&dir, &["switch", "-q", "-c", "sq"]);
        commit(&dir, "s", "one\n");
        sh(&dir, &["switch", "-q", "-c", "clash", "main"]);
        commit(&dir, "a", "clash\n");
        sh(&dir, &["switch", "-q", "-c", "other", "main"]);
        commit(&dir, "o", "other\n");
        sh(&dir, &["switch", "-q", "main"]);
        commit(&dir, "a", "main\n");
        sh(&dir, &["merge", "-q", "--squash", "sq"]);
        sh(&dir, &["commit", "-q", "-m", "Squashed sq"]);
        let tree = sh(&dir, &["rev-parse", "main^{tree}"]);
        let tips: Vec<String> = ["clash", "sq", "other", "sq"].iter().map(|b| sh(&dir, &["rev-parse", b]).trim().to_owned()).collect();
        let tips: Vec<&str> = tips.iter().map(String::as_str).collect();
        assert_eq!(same_tree(&dir, "main", tree.trim(), &tips), Some(vec![false, true, false, true]));
        assert_eq!(counts(&dir, "main")["sq"], (1, 2));
        assert_eq!(unpushed(&dir, "main", &tips[..3]).unwrap()[tips[2]], 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `git cherry` skips merge commits and whitespace: a "rebase-merged" branch whose
    /// content still differs from the target is listed but not pre-selected.
    #[test]
    fn rebase_merged_needs_matching_content() {
        let dir = scratch("cleanup-cherry");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "Makefile", "all:\n\techo hi\n");
        // A merge of main into the branch adds a file main never got.
        sh(&dir, &["switch", "-q", "-c", "mrg"]);
        commit(&dir, "f1", "one\n");
        sh(&dir, &["switch", "-q", "-c", "ws", "main"]);
        commit(&dir, "Makefile", "all:\n\techo bye\n");
        sh(&dir, &["switch", "-q", "main"]);
        commit(&dir, "m", "main\n");
        sh(&dir, &["switch", "-q", "mrg"]);
        sh(&dir, &["merge", "-q", "--no-commit", "main"]);
        std::fs::write(dir.join("evil"), "extra\n").unwrap();
        sh(&dir, &["add", "evil"]);
        sh(&dir, &["commit", "-q", "-m", "merge main"]);
        sh(&dir, &["switch", "-q", "main"]);
        sh(&dir, &["cherry-pick", "mrg^1"]);
        // Tabs vs spaces only.
        commit(&dir, "Makefile", "all:\n    echo bye\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        scanned(&ctx, &mut t);
        assert_eq!(shown(&t, now()), []);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Refs a fetch refspec maps from outside refs/heads (pull requests) stay while the remote has them.
    #[test]
    fn prune_follows_fetch_refspecs() {
        let root = scratch("cleanup-refspec");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        sh(&root, &["clone", "-q", "remote.git", "a"]);
        let a = root.join("a");
        identity(&a);
        commit(&a, "f.txt", "one\n");
        sh(&a, &["push", "-q", "origin", "main", "main:refs/pull/7/head", "main:old"]);
        sh(&a, &["config", "--add", "remote.origin.fetch", "+refs/pull/*/head:refs/remotes/origin/pr/*"]);
        sh(&a, &["fetch", "-q", "origin"]);
        sh(&root.join("remote.git"), &["branch", "-D", "old"]);
        let stale = stale_refs(&a, &["origin".into()]).unwrap();
        assert_eq!(stale.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(), ["refs/remotes/origin/old"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn rows_for(dir: &Path, names: &[&str]) -> Vec<(Cand, bool)> {
        names.iter().map(|n| (Cand { name: n.to_string(), oid: sh(dir, &["rev-parse", n]).trim().to_owned(), ..Default::default() }, false)).collect()
    }

    #[test]
    fn target_is_an_existing_trunk_by_full_name() {
        let dir = scratch("cleanup-target");
        sh(&dir, &["init", "-q", "-b", "master"]);
        identity(&dir);
        commit(&dir, "a", "base\n");
        sh(&dir, &["switch", "-q", "-c", "feature"]);
        commit(&dir, "f", "work\n");
        sh(&dir, &["tag", "master"]);
        sh(&dir, &["switch", "-q", "master"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        scanned(&ctx, &mut t);
        assert_eq!(t.cleanup.as_ref().unwrap().target, "master");
        assert_eq!(shown(&t, now()), []);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unmerged_branches_are_never_merged() {
        let root = scratch("cleanup-unmerged");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        sh(&root, &["clone", "-q", "remote.git", "a"]);
        let dir = root.join("a");
        identity(&dir);
        commit(&dir, "a", "1\n2\n3\n4\n5\n6\n7\n8\n");
        commit(&dir, "shared", "x\n");
        sh(&dir, &["push", "-q", "-u", "origin", "main"]);
        let fork = |b: &str| sh(&dir, &["switch", "-q", "-c", b, "main"]);
        fork("uniq");
        commit(&dir, "u", "unique\n");
        fork("conflict");
        commit(&dir, "a", "1\nB\n3\n4\n5\n6\n7\n8\n");
        fork("clean");
        commit(&dir, "a", "1\n2\n3\n4\n5\n6\n7\nB\n");
        fork("picked");
        commit(&dir, "p1", "picked\n");
        commit(&dir, "p2", "unique\n");
        fork("reverted");
        commit(&dir, "r", "reverted on main\n");
        fork("collide");
        commit(&dir, "c", "c\n");
        sh(&dir, &["revert", "--no-edit", "HEAD"]);
        commit(&dir, "c", "c\n");
        fork("empty");
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "empty"]);
        commit(&dir, "e", "unique\n");
        fork("remote");
        commit(&dir, "o", "on origin/main only\n");
        sh(&dir, &["push", "-q", "origin", "remote:main"]);
        sh(&dir, &["switch", "-q", "main"]);
        commit(&dir, "a", "1\n2\nM\n4\n5\n6\n7\n8\n");
        commit(&dir, "a", "1\nM\nM\n4\n5\n6\n7\n8\n");
        sh(&dir, &["cherry-pick", "picked~1"]);
        sh(&dir, &["cherry-pick", "reverted"]);
        sh(&dir, &["revert", "--no-edit", "HEAD"]);
        sh(&dir, &["cherry-pick", "collide~2"]);
        sh(&dir, &["cherry-pick", "collide~1"]);
        let names = ["uniq", "conflict", "clean", "picked", "reverted", "collide", "empty", "remote"];
        let main = scan(&dir, "refs/heads/main", rows_for(&dir, &names)).unwrap();
        assert!(main.iter().all(|c| c.why.is_none() && !c.on), "{main:?}");
        let origin = scan(&dir, "refs/remotes/origin/main", rows_for(&dir, &names)).unwrap();
        assert_eq!((origin[0].name.as_str(), origin[0].why, origin[0].on), ("remote", Some(Why::Merged), true));
        assert!(origin[1..].iter().all(|c| c.why.is_none() && !c.on), "{origin:?}");
        let tips: Vec<String> = names.iter().map(|n| sh(&dir, &["rev-parse", n]).trim().to_owned()).collect();
        let tips: Vec<&str> = tips.iter().map(String::as_str).collect();
        let tree = sh(&dir, &["rev-parse", "main^{tree}"]);
        let same = same_tree(&dir, "main", tree.trim(), &tips).unwrap();
        let (twins, unpushed, counts) = (twins(&dir, "main", &tips).unwrap(), unpushed(&dir, "main", &tips).unwrap(), counts(&dir, "main"));
        for (i, (n, t)) in names.iter().zip(&tips).enumerate() {
            let one = git::run(&dir, &["merge-tree", "--write-tree", "main", t]).is_ok_and(|o| o.lines().next() == Some(tree.trim()));
            let cherry = sh(&dir, &["cherry", "main", t]);
            let lr = sh(&dir, &["rev-list", "--left-right", "--count", &format!("main...{t}")]);
            let lr: Vec<u32> = lr.split_whitespace().map(|n| n.parse().unwrap()).collect();
            let up: u32 = sh(&dir, &["rev-list", "--count", t, "--not", "--remotes", "main"]).trim().parse().unwrap();
            assert_eq!(same[i], one, "{n}");
            assert_eq!(twins[*t], !cherry.is_empty() && cherry.lines().all(|l| l.starts_with('-')), "{n}");
            assert_eq!((counts[*n], unpushed[*t]), ((lr[1], lr[0]), up), "{n}");
        }
        assert!(twins[tips[4]] && twins[tips[5]] && !twins[tips[3]], "reverted and collide have twins, picked doesn't");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stale_after_the_chosen_days() {
        let c = Cand { date: 1000, ..Default::default() };
        assert_eq!(Cleanup::group(&c, 90, 1000 + 89 * 86400), None);
        assert_eq!(Cleanup::group(&c, 90, 1000 + 90 * 86400), Some(Why::Stale));
        assert_eq!(Cleanup::group(&Cand { why: Some(Why::Gone), ..c }, 90, 1000), Some(Why::Gone));
        assert_eq!(source("+refs/pull/*/head:refs/remotes/o/pr/*", "refs/remotes/o/pr/7").as_deref(), Some("refs/pull/7/head"));
        assert_eq!(source("refs/heads/main:refs/remotes/o/main", "refs/remotes/o/main").as_deref(), Some("refs/heads/main"));
        assert_eq!(source("+refs/heads/*:refs/remotes/o/*", "refs/remotes/p/x"), None);
        assert!(matches("release/*", "release/1.2") && matches("main", "main") && !matches("main", "mainx"));
        assert!(protected("hotfix/*, staging", "staging") && protected("", "release/2.0") && protected("", "develop") && !protected("staging", "feat"));
    }
}
