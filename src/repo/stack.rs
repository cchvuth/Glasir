//! Stacked branches: local branches built on one another (each tip an ancestor of the next, off the trunk). Stacks move together on commit or rewrite.

use super::data::RepoData;
use super::op::{Op, Undo};
use crate::git::{self, RefKind, Res};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Child branch -> (parent branch, the parent tip it is built on). A stale edge (kept
/// by [`keep_stale`] after the parent moved on) has a base that is no longer the parent's tip.
pub type Stacks = HashMap<String, (String, String)>;

static FOLLOW: AtomicBool = AtomicBool::new(true);
static KEEP: Mutex<Option<String>> = Mutex::new(None);

/// Settings → "Keep stacked branches together" and Protected branches, mirrored every frame.
/// True when Protected branches changed (not the first mirror): the stacks need a reload.
pub fn set_follow(on: bool, keep: &str) -> bool {
    FOLLOW.store(on, Ordering::Relaxed);
    let mut k = KEEP.lock().unwrap_or_else(|e| e.into_inner());
    let changed = k.as_deref().is_some_and(|k| k != keep);
    if k.as_deref() != Some(keep) {
        *k = Some(keep.to_owned());
    }
    changed
}

/// A long-lived branch (`release/*`, Settings → Protected branches..): never a stack member.
pub(super) fn protected(name: &str) -> bool {
    super::cleanup::protected(KEEP.lock().unwrap_or_else(|e| e.into_inner()).as_deref().unwrap_or_default(), name)
}

fn trunk_name(d: &RepoData, name: &str) -> bool {
    d.branch_rank(name) < 2 || protected(name)
}

pub fn follows() -> bool {
    FOLLOW.load(Ordering::Relaxed)
}

/// `exec` pseudo-command: `RESTACK mode trunk.. -- (child parent child-tip base trigger)..`;
/// see [`restack`].
pub(super) const RESTACK: &str = "!restack";

/// Each branch's nearest ancestor branch: walks `rev-list --parents` lines (the commits
/// off the trunk) back from every tip to the first commit another branch points at.
pub(super) fn parse(revs: &str, tips: &[(String, String)]) -> Stacks {
    let parents: HashMap<&str, Vec<&str>> = revs.lines().filter_map(|l| l.split_once(' ').map(|(c, p)| (c, p.split(' ').collect())).or(Some((l, vec![])))).collect();
    let mut at: HashMap<&str, Vec<&str>> = HashMap::new();
    for (n, t) in tips {
        at.entry(t.as_str()).or_default().push(n.as_str());
    }
    at.values_mut().for_each(|v| v.sort_unstable());
    let mut out = Stacks::new();
    for (name, tip) in tips {
        let (mut seen, mut q) = (HashSet::new(), VecDeque::from(parents.get(tip.as_str()).cloned().unwrap_or_default()));
        while let Some(c) = q.pop_front() {
            // Commits on the trunk aren't listed: a merged branch there is no parent.
            let Some(ps) = parents.get(c).filter(|_| seen.insert(c)) else { continue };
            if let Some(n) = at.get(c) {
                out.insert(name.clone(), (n[0].to_owned(), c.to_owned()));
                break;
            }
            q.extend(ps);
        }
    }
    out
}

/// The stacks among the local branches (not the trunk ones: the default branch, main, release/*..).
pub(super) fn load(dir: &Path, d: &RepoData) -> Stacks {
    let tips: Vec<(String, String)> = d.refs.iter().filter(|r| r.kind == RefKind::Local && !trunk_name(d, &r.name)).map(|r| (r.name.clone(), r.target.clone())).collect();
    if tips.len() < 2 {
        return Stacks::new();
    }
    // ponytail: capped walk; a stack further than 20000 commits off the trunk goes undetected.
    let mut args = vec!["rev-list", "--parents", "--max-count=20000"];
    args.extend(tips.iter().map(|t| t.1.as_str()));
    args.push("--not");
    args.extend(d.refs.iter().filter(|r| trunk(d, r)).map(|r| r.full.as_str()));
    git::run(dir, &args).map(|o| parse(&o, &tips)).unwrap_or_default()
}

fn trunk(d: &RepoData, r: &git::Ref) -> bool {
    match r.kind {
        RefKind::Local => trunk_name(d, &r.name),
        RefKind::Remote => d.split_remote(&r.name).is_some_and(|(_, b)| trunk_name(d, b)),
        RefKind::Tag => false,
    }
}

/// After a reload: an edge whose parent moved on (new or rewritten commits) while the child
/// stayed put is still a stack, just stale, until it is restacked.
pub(super) fn keep_stale(old: &RepoData, new: &mut RepoData) {
    let tip = |d: &RepoData, b: &str| d.local(b).map(|r| r.target.clone());
    let mut st = (*new.stack).clone();
    for (c, (p, base)) in old.stack.iter() {
        if tip(old, c).is_some() && tip(old, c) == tip(new, c) && tip(new, p).is_some_and(|t| t != *base) && !trunk_name(new, p) {
            st.insert(c.clone(), (p.clone(), base.clone()));
        }
    }
    new.stack = std::sync::Arc::new(st);
}

pub fn parent<'a>(d: &'a RepoData, b: &str) -> Option<&'a str> {
    d.stack.get(b).map(|(p, _)| p.as_str())
}

/// Built on an older tip of its parent: a restack would bring it along.
pub fn stale(d: &RepoData, b: &str) -> bool {
    d.stack.get(b).is_some_and(|(p, base)| d.local(p).is_some_and(|r| r.target != *base))
}

/// Branches stacked directly on `b`, A–Z.
pub fn children<'a>(d: &'a RepoData, b: &str) -> Vec<&'a str> {
    let mut v: Vec<&str> = d.stack.iter().filter(|(_, (p, _))| p == b).map(|(c, _)| c.as_str()).collect();
    v.sort_unstable();
    v
}

/// The stack `b` is in, bottom first (parents before their children); just `[b]` if none.
pub fn members(d: &RepoData, b: &str) -> Vec<String> {
    let mut root = b;
    for _ in 0..64 {
        match parent(d, root) {
            Some(p) if p != b => root = p,
            _ => break,
        }
    }
    let mut out = vec![root.to_owned()];
    let mut i = 0;
    while i < out.len() {
        for c in children(d, &out[i]) {
            if !out.iter().any(|o| o == c) {
                out.push(c.to_owned());
            }
        }
        i += 1;
    }
    out
}

/// The `RESTACK` command for the edges whose child is in `only` (all if None), parents first.
/// Children checked out in another worktree are left alone (git can't rebase them here).
/// `follow`: restack where this op moves a parent, else where the parent moved on since.
/// Pinned (put back if a `--update-refs` rebase moves them, and not restacked after it): the
/// trunk branches and any other branch at HEAD's commit, a copy rather than a stack member.
fn restack_cmd(d: &RepoData, only: Option<&[String]>, follow: bool) -> Vec<String> {
    let depth = |c: &str| (0..64).scan(c, |c, _| parent(d, c).map(|p| *c = p)).count();
    let mut edges: Vec<(&String, &(String, String))> = d
        .stack
        .iter()
        .filter(|(c, _)| only.is_none_or(|o| o.contains(c)) && d.local(c).is_some_and(|r| r.worktree.is_none()))
        .collect();
    edges.sort_by_key(|(c, _)| (depth(c), c.as_str()));
    let mut cmd: Vec<String> = vec![RESTACK.into(), if follow { "follow" } else { "own" }.into()];
    let head = d.refs.iter().find(|r| r.kind == RefKind::Local && r.head).map(|r| r.target.as_str());
    let pin = |r: &&git::Ref| r.kind == RefKind::Local && (trunk(d, r) || (!r.head && Some(r.target.as_str()) == head));
    cmd.extend(d.refs.iter().filter(pin).flat_map(|r| [r.name.clone(), r.target.clone()]));
    cmd.push("--".into());
    for (c, (p, base)) in edges {
        let (Some(ct), Some(pt)) = (d.local(c), d.local(p)) else { continue };
        if follow && pin(&ct) {
            continue;
        }
        cmd.extend([c.clone(), p.clone(), ct.target.clone(), base.clone(), if follow { pt.target.clone() } else { base.clone() }]);
    }
    cmd
}

/// `git <args>`'s subcommand (after `-c k=v` and other options).
fn subcommand(args: &[String]) -> Option<&str> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-c" {
            it.next();
        } else if !a.starts_with('-') {
            return Some(a);
        }
    }
    None
}

/// A commit or rebase in a repo with stacks: rebases move the stacked branches below
/// along (`--update-refs`), and then the branches stacked on a moved one are restacked.
pub(super) fn follow(mut op: Op, d: Option<&RepoData>) -> Op {
    let Some(d) = d.filter(|d| follows() && !d.stack.is_empty() && !op.custom && op.undo != Undo::No) else { return op };
    if !op.cmds.iter().any(|c| matches!(subcommand(c), Some("commit" | "rebase"))) {
        return op;
    }
    let ctl = ["--continue", "--abort", "--skip", "--quit", "--edit-todo", "--update-refs", "--no-update-refs"];
    for c in op.cmds.iter_mut().filter(|c| subcommand(c) == Some("rebase") && !c.iter().any(|a| ctl.contains(&a.as_str()))) {
        let i = c.iter().position(|a| a == "rebase").unwrap_or(0);
        c.insert(i + 1, "--update-refs".into());
    }
    // Right after the commit / rebase: a push after it may fail.
    let at = op.cmds.iter().rposition(|c| matches!(subcommand(c), Some("commit" | "rebase"))).map_or(op.cmds.len(), |i| i + 1);
    op.cmds.insert(at, restack_cmd(d, None, true));
    op
}

/// Restack the stale branches of `b`'s stack onto their parents, bottom first.
pub fn restack_op(d: &RepoData, b: &str) -> Op {
    let members = members(d, b);
    let mut op = Op::new(format!("Restack {}", members[0]), &[]).undo(Undo::Keep);
    op.cmds = vec![restack_cmd(d, Some(&members), false)];
    op
}

/// Runs a `RESTACK`: first puts back any trunk branch (`main`..) a `--update-refs` rebase
/// just rewrote, then rebases each child whose parent moved (`--onto parent base child`),
/// with uncommitted changes stashed, and returns to the branch it started on. All or
/// nothing: a conflict rolls every restacked branch back. `follow` reports a conflict as a
/// note (the op before it went through) rather than an error.
pub(super) fn restack(dir: &Path, a: &[&str]) -> Res<String> {
    let (follow, rest) = (a.first() == Some(&"follow"), a.get(1..).unwrap_or_default());
    let sep = rest.iter().position(|x| *x == "--").unwrap_or(rest.len());
    let (pins, edges) = (&rest[..sep], rest.get(sep + 1..).unwrap_or_default());
    let head = git::run(dir, &["symbolic-ref", "-q", "HEAD"]).or_else(|_| git::run(dir, &["rev-parse", "HEAD"]))?.trim().to_owned();
    let refs = git::run(dir, &["for-each-ref", "--format=%(refname:lstrip=2) %(objectname)", "refs/heads"])?;
    let tips: HashMap<&str, &str> = refs.lines().filter_map(|l| l.split_once(' ')).collect();
    let tip = |b: &str| tips.get(b).copied();
    for p in pins.chunks(2) {
        let [b, was] = p else { continue };
        if tip(b).is_none_or(|t| t == *was) || format!("refs/heads/{b}") == head {
            continue;
        }
        let log = git::run(dir, &["log", "-g", "-2", "--format=%H%x1f%gs", &format!("refs/heads/{b}"), "--"]).unwrap_or_default();
        let l: Vec<Vec<&str>> = log.lines().map(|l| l.split('\x1f').collect()).collect();
        if l.len() == 2 && l[0].get(1) == Some(&"rewritten during rebase") && l[1][0] == *was {
            git::run(dir, &["update-ref", &format!("refs/heads/{b}"), was, l[0][0]])?;
        }
    }
    let edges: Vec<&[&str]> = edges.chunks(5).filter(|e| e.len() == 5).collect();
    // Tips as the restack starts: a parent it moves itself is in `moved`.
    let needs = |e: &[&str], moved: &[(&str, &str)]| tip(e[0]) == Some(e[2]) && (moved.iter().any(|m| m.0 == e[1]) || tip(e[1]).is_some_and(|t| t != e[4]));
    let nothing = if follow { String::new() } else { "Nothing to restack: every branch is on its parent.".into() };
    if !edges.iter().any(|e| needs(e, &[])) {
        return Ok(nothing);
    }
    let gd = git::git_dir(dir)?;
    if git::state(&gd) != git::State::Clean {
        let why = "Stacked branches weren't restacked: finish the operation in progress, then use Restack in the sidebar.";
        return if follow { Ok(why.into()) } else { Err(why.into()) };
    }
    let back = || match head.strip_prefix("refs/heads/") {
        Some(b) => git::run(dir, &["checkout", "-q", b, "--"]),
        None => git::run(dir, &["checkout", "-q", "--detach", &head, "--"]),
    };
    git::with_autostash(dir, "Glasir: restack stacked branches", || {
        let mut moved: Vec<(&str, &str)> = vec![];
        for e in &edges {
            // Not built on `base` any more: not this stack's to move.
            if !needs(e, &moved) || git::run(dir, &["merge-base", "--is-ancestor", e[3], e[0]]).is_err() {
                continue;
            }
            let err = match git::run(dir, &["rebase", "-q", "--no-update-refs", "--onto", &format!("refs/heads/{}", e[1]), e[3], e[0]]) {
                Ok(_) => {
                    moved.push((e[0], e[2]));
                    continue;
                }
                Err(err) => err,
            };
            let undo = git::Cancel::default().scope(|| -> Res<()> {
                if git::state(&gd) == git::State::Rebasing {
                    git::run(dir, &["rebase", "--abort"])?;
                }
                back()?;
                moved.iter().try_for_each(|(b, old)| git::run(dir, &["update-ref", &format!("refs/heads/{b}"), old]).map(drop))
            });
            if let Err(u) = undo {
                return Err(format!("{err}\nRolling the restack back failed too, so some stacked branches may have moved:\n{u}"));
            }
            if err == git::CANCELLED {
                return Err(err);
            }
            let why = format!(
                "Stacked branches weren't restacked: {} conflicts with the new {}. Check out {0} and run git rebase --onto {1} {} to resolve it.",
                e[0],
                e[1],
                &e[3][..7.min(e[3].len())]
            );
            return if follow { Ok(why) } else { Err(why) };
        }
        back()?;
        Ok(if moved.is_empty() { nothing } else { format!("Restacked {}", moved.iter().map(|m| m.0).collect::<Vec<_>>().join(", ")) })
    })
}

/// Push every branch of `b`'s stack, bottom first: a new one sets its upstream, one behind
/// its remote branch is skipped, and one rewritten since its last push is force-pushed with
/// a lease, only if the remote commit is one it had (`--force-if-includes`).
pub fn push_op(d: &RepoData, b: &str) -> Option<Op> {
    let first = d.remotes.iter().find(|r| r.0 == "origin").or(d.remotes.first())?.0.as_str();
    // Pushes what is shown: a branch moved since refuses the whole push.
    let mut guard = vec![super::rebase::REFS_AT.to_owned()];
    let (mut cmds, mut forced) = (vec![], vec![]);
    for name in members(d, b) {
        let r = d.local(&name)?;
        let fork = d.cfg(&format!("branch.{name}.pushremote")).or(d.cfg("remote.pushdefault"));
        let (remote, rb) = match d.push_dest(&name, &r.upstream) {
            Some((rem, _)) if fork.is_some_and(|f| f != rem) => (fork?, name.as_str()),
            Some(dest) => dest,
            None => (fork.unwrap_or(first), name.as_str()),
        };
        let tracking = d.refs.iter().find(|x| x.kind == RefKind::Remote && x.name == format!("{remote}/{rb}")).filter(|x| x.target != r.target);
        let reaches = |from: &str, to: &str| {
            let (Some(&f), Some(&t)) = (d.row_of.get(from), d.row_of.get(to)) else { return false };
            d.reachable(f)[t]
        };
        if tracking.is_some_and(|t| reaches(&t.target, &r.target)) {
            continue; // behind: nothing to push, and forcing would drop the remote's commits
        }
        let mut c = vec!["push".to_owned(), "--progress".into()];
        if r.upstream.is_empty() {
            c.push("-u".into());
        }
        if let Some(t) = tracking.filter(|t| !reaches(&r.target, &t.target)) {
            // No expected value: with one, git skips the `--force-if-includes` reflog check.
            // The guard pins the remote-tracking ref instead (the lease's expected value).
            c.extend([format!("--force-with-lease=refs/heads/{rb}"), "--force-if-includes".into()]);
            guard.extend([t.full.clone(), t.target.clone()]);
            forced.push(name.clone());
        }
        c.extend([remote.to_owned(), format!("{name}:refs/heads/{rb}")]);
        guard.extend([r.full.clone(), r.target.clone()]);
        cmds.push(c);
    }
    let n = cmds.len();
    let mut op = Op::new(format!("Push stack ({n} branches)"), &[]);
    op.cmds = [guard].into_iter().chain(cmds).collect();
    Some(match forced.is_empty() {
        true => op,
        false => op.confirm(format!(
            "Push the {n} branches of this stack? {} will be force-pushed, replacing the remote commits you rewrote. Each goes through only if its remote branch is still where it's shown and holds no commit you never had.",
            forced.join(", ")
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::testutil::{commit, identity, scratch, settle, sh};
    use crate::repo::{Action, RepoTab, View};

    /// Editing Protected branches asks for a reload (stack detection reads it), the first mirror doesn't.
    #[test]
    fn protected_branches_change_reloads() {
        assert!(!set_follow(true, ""), "the first mirror at startup");
        assert!(!set_follow(true, ""));
        assert!(set_follow(true, "zz-keep-test") && protected("zz-keep-test"));
        assert!(set_follow(true, ""));
    }

    #[test]
    fn parses_chains_and_skips_trunk() {
        let revs = "c3 c2\nc2 c1\nc1 m\nx1 m\n";
        let tips = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
        let st = parse(revs, &tips(&[("top", "c3"), ("mid", "c2"), ("bottom", "c1"), ("other", "x1"), ("merged", "m")]));
        assert_eq!(st.get("top"), Some(&("mid".into(), "c2".into())));
        assert_eq!(st.get("mid"), Some(&("bottom".into(), "c1".into())));
        assert_eq!((st.get("bottom"), st.get("other"), st.get("merged")), (None, None, None));
        assert_eq!(st.len(), 2);
    }

    /// main ← a ← b ← c, each with one commit; HEAD on `a`.
    fn stack_repo(name: &str) -> std::path::PathBuf {
        let dir = scratch(name);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "m0", "m0\n");
        commit(&dir, "base", "base\n");
        for b in ["a", "b", "c"] {
            sh(&dir, &["checkout", "-q", "-b", b]);
            commit(&dir, b, &format!("{b}\n"));
        }
        sh(&dir, &["checkout", "-q", "a"]);
        dir
    }

    fn subjects(dir: &Path, b: &str) -> String {
        sh(dir, &["log", "--format=%s", b, "--"]).lines().collect::<Vec<_>>().join(" ")
    }

    /// Amend the bottom of a 3-branch stack: the two above follow, ⌘Z puts all three back.
    #[test]
    fn amend_bottom_children_follow_and_undo() {
        let dir = stack_repo("stack-amend");
        let tips = |d: &Path| ["a", "b", "c"].map(|b| sh(d, &["rev-parse", b]));
        let before = tips(&dir);
        std::fs::write(dir.join("a"), "a fixed\n").unwrap();
        std::fs::write(dir.join("wip"), "untracked\n").unwrap(); // stays put through the checkouts
        sh(&dir, &["add", "a"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        assert_eq!(parent(t.data.as_ref().unwrap(), "c"), Some("b"));
        t.amend = true;
        t.summary = "a".into();
        t.apply(&ctx, Action::Git(t.commit_op()));
        let msgs = settle(&ctx, &mut t);
        assert!(msgs.iter().any(|m| m.text.contains("Restacked b, c")), "{:?}", msgs.iter().map(|m| &m.text).collect::<Vec<_>>());
        assert_eq!(subjects(&dir, "c"), "c b a base m0");
        assert_eq!(sh(&dir, &["show", "c:a"]), "a fixed\n");
        assert_eq!(sh(&dir, &["symbolic-ref", "--short", "HEAD"]), "a\n");
        assert_eq!(std::fs::read_to_string(dir.join("wip")).unwrap(), "untracked\n");
        let d = t.data.as_ref().unwrap();
        assert!(!stale(d, "b") && !stale(d, "c") && parent(d, "c") == Some("b"));
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(tips(&dir), before);
        assert_eq!(sh(&dir, &["diff", "--cached", "--name-only"]), "a\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rebase from the top moves the branches below (`--update-refs`), never main.
    #[test]
    fn rebase_top_updates_refs_below_but_not_main() {
        let dir = stack_repo("stack-update-refs");
        sh(&dir, &["checkout", "-q", "c"]);
        let main = sh(&dir, &["rev-parse", "main"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let a = sh(&dir, &["rev-parse", "a"]).trim().to_owned();
        t.apply(&ctx, Action::Rewrite { commit: a, act: crate::repo::rebase::Act::Reword, message: Some("a reworded".into()) });
        settle(&ctx, &mut t);
        assert_eq!(subjects(&dir, "c"), "c b a reworded base m0");
        assert_eq!(sh(&dir, &["rev-parse", "b~1"]), sh(&dir, &["rev-parse", "a"]));
        assert_eq!(subjects(&dir, "a"), "a reworded base m0");
        // A rebase whose range takes in main's tip too must leave main itself alone.
        let a = sh(&dir, &["rev-parse", "a"]);
        t.apply(&ctx, Action::Git(Op::new("Rebase", &["rebase", "-q", "--force-rebase", "main~1"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["rev-parse", "main"]), main);
        assert_ne!(sh(&dir, &["rev-parse", "a"]), a);
        assert_eq!(sh(&dir, &["rev-parse", "b~1"]), sh(&dir, &["rev-parse", "a"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Parent moved on with follow off: the child is stale; Restack brings it along, a conflict rolls back.
    #[test]
    fn restack_stale_child_and_conflict_rolls_back() {
        let dir = stack_repo("stack-restack");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        commit(&dir, "a2", "more a\n"); // outside Glasir: `b` and `c` stay on the old `a`
        t.reload(&ctx, true);
        settle(&ctx, &mut t);
        let d = t.data.as_ref().unwrap();
        assert!(stale(d, "b") && !stale(d, "c"));
        assert_eq!(members(d, "c"), ["a", "b", "c"]);
        t.apply(&ctx, Action::Git(restack_op(d, "c")));
        settle(&ctx, &mut t);
        assert_eq!(subjects(&dir, "c"), "c b more a a base m0");
        assert!(!stale(t.data.as_ref().unwrap(), "b"));
        // `a` now changes the file `b` changed: the restack conflicts and moves nothing.
        commit(&dir, "b", "a's b\n");
        t.reload(&ctx, true);
        settle(&ctx, &mut t);
        let tips = ["b", "c"].map(|b| sh(&dir, &["rev-parse", b]));
        t.apply(&ctx, Action::Git(restack_op(t.data.as_ref().unwrap(), "a")));
        let msgs = settle(&ctx, &mut t);
        assert!(msgs.iter().any(|m| m.error && m.text.contains("b conflicts with the new a")));
        assert_eq!(["b", "c"].map(|b| sh(&dir, &["rev-parse", b])), tips);
        assert_eq!((sh(&dir, &["symbolic-ref", "--short", "HEAD"]).as_str(), sh(&dir, &["status", "--porcelain"]).as_str()), ("a\n", ""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Push a stack to a bare remote, then again after rewriting its bottom (leased force push).
    #[test]
    fn push_stack_to_bare_remote() {
        let dir = stack_repo("stack-push");
        let remote = scratch("stack-push-remote");
        sh(&remote, &["init", "-q", "--bare"]);
        sh(&dir, &["remote", "add", "origin", &remote.to_string_lossy()]);
        sh(&dir, &["push", "-q", "origin", "main"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let op = push_op(t.data.as_ref().unwrap(), "b").unwrap();
        assert!(op.confirm.is_none());
        t.apply(&ctx, Action::Git(op));
        settle(&ctx, &mut t);
        for b in ["a", "b", "c"] {
            assert_eq!(sh(&remote, &["rev-parse", b]), sh(&dir, &["rev-parse", b]));
            assert_eq!(sh(&dir, &["rev-parse", "--abbrev-ref", &format!("{b}@{{u}}")]), format!("origin/{b}\n"));
        }
        // Reword the bottom: everything above is rewritten, so the push needs a lease.
        t.summary = "a v2".into();
        t.amend = true;
        t.apply(&ctx, Action::Git(t.commit_op()));
        settle(&ctx, &mut t);
        let op = push_op(t.data.as_ref().unwrap(), "a").unwrap();
        assert!(op.confirm.as_deref().is_some_and(|c| c.contains("a, b, c will be force-pushed")));
        t.apply(&ctx, Action::Git(Op { confirm: None, ..op }));
        settle(&ctx, &mut t);
        assert_eq!(sh(&remote, &["log", "--format=%s", "c"]), "c\nb\na v2\nbase\nm0\n");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&remote);
    }

    /// A teammate's commit on a stacked branch survives Push stack: just behind, the branch
    /// is skipped; diverged without having taken that commit in, the force push is refused.
    #[test]
    fn push_stack_keeps_teammate_commits() {
        let dir = stack_repo("stack-push-mate");
        let (remote, mate) = (scratch("stack-push-mate-remote"), scratch("stack-push-mate-clone"));
        sh(&remote, &["init", "-q", "--bare"]);
        sh(&dir, &["remote", "add", "origin", &remote.to_string_lossy()]);
        sh(&dir, &["push", "-q", "-u", "origin", "main", "a", "b", "c"]);
        sh(&mate, &["clone", "-q", "-b", "b", &remote.to_string_lossy(), "."]);
        identity(&mate);
        commit(&mate, "t", "teammate\n");
        sh(&mate, &["push", "-q", "origin", "b"]);
        sh(&dir, &["fetch", "-q", "origin"]);
        let theirs = sh(&remote, &["rev-parse", "b"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let op = push_op(t.data.as_ref().unwrap(), "a").unwrap();
        assert!(op.confirm.is_none() && !op.cmds.iter().flatten().any(|a| a.starts_with("--force")), "{:?}", op.cmds);
        t.apply(&ctx, Action::Git(op));
        settle(&ctx, &mut t);
        assert_eq!(sh(&remote, &["rev-parse", "b"]), theirs);
        sh(&dir, &["checkout", "-q", "b"]);
        commit(&dir, "mine", "mine\n");
        t.reload(&ctx, true);
        settle(&ctx, &mut t);
        let op = push_op(t.data.as_ref().unwrap(), "a").unwrap();
        t.apply(&ctx, Action::Git(Op { confirm: None, ..op }));
        let msgs = settle(&ctx, &mut t);
        assert!(msgs.iter().any(|m| m.error), "{:?}", msgs.iter().map(|m| &m.text).collect::<Vec<_>>());
        assert_eq!(sh(&remote, &["rev-parse", "b"]), theirs);
        for d in [&dir, &remote, &mate] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// History edits move only the stacked branches below HEAD: a backup at HEAD's tip stays.
    #[test]
    fn backup_branch_at_head_stays_put() {
        let dir = stack_repo("stack-backup");
        sh(&dir, &["checkout", "-q", "c"]);
        sh(&dir, &["branch", "c-backup"]);
        let backup = sh(&dir, &["rev-parse", "c-backup"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let a = sh(&dir, &["rev-parse", "a"]).trim().to_owned();
        t.apply(&ctx, Action::Rewrite { commit: a, act: crate::repo::rebase::Act::Reword, message: Some("a reworded".into()) });
        settle(&ctx, &mut t);
        assert_eq!((subjects(&dir, "c"), sh(&dir, &["rev-parse", "c-backup"])), ("c b a reworded base m0".into(), backup));
        // A plain rebase (`--update-refs`) puts it back too.
        sh(&dir, &["branch", "-f", "c-backup", "c"]);
        let backup = sh(&dir, &["rev-parse", "c-backup"]);
        // main moves on: a same-second `--force-rebase` can rewrite to the very same commits.
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "m1", "m1\n");
        sh(&dir, &["checkout", "-q", "c"]);
        t.reload(&ctx, true);
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Rebase", &["rebase", "-q", "main"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        assert_ne!(sh(&dir, &["rev-parse", "c"]), backup);
        assert_eq!(sh(&dir, &["rev-parse", "c-backup"]), backup);
        assert_eq!(sh(&dir, &["rev-parse", "b~1"]), sh(&dir, &["rev-parse", "a"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A long-lived base like `release/2.0` is trunk, not a stack parent: rebasing the feature
    /// on it onto main leaves the release branch alone (also when other stacks exist).
    #[test]
    fn rebase_leaves_release_branch() {
        let dir = scratch("stack-release");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "m", "m\n");
        for b in ["release/2.0", "feat", "feat2"] {
            sh(&dir, &["checkout", "-q", "-b", b]);
            commit(&dir, b.replace('/', "-").as_str(), &format!("{b}\n"));
        }
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "m2", "m2\n");
        sh(&dir, &["checkout", "-q", "feat"]);
        let release = sh(&dir, &["rev-parse", "release/2.0"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let d = t.data.as_ref().unwrap();
        assert_eq!((parent(d, "feat"), parent(d, "feat2")), (None, Some("feat")));
        t.apply(&ctx, Action::Git(Op::new("Rebase", &["rebase", "-q", "main"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["rev-parse", "release/2.0"]), release);
        assert_eq!(subjects(&dir, "feat2"), "feat2 feat release/2.0 m2 m");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Commit & push: the restack comes right after the commit, so a failed push can't skip it.
    #[test]
    fn follow_restacks_before_the_push() {
        let dir = stack_repo("stack-follow-push");
        let d = crate::repo::data::load_all(&dir, 100, View::default()).unwrap();
        let op = follow(Op::new("Commit & push", &["commit", "-F", "-"]).then(&["push", "origin", "a"]).undo(Undo::Soft), Some(&d));
        assert_eq!(op.cmds.iter().map(|c| c[0].as_str()).collect::<Vec<_>>(), ["commit", RESTACK, "push"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
