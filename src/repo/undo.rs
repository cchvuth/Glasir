//! Undo/redo: a step holds the refs an op moved and durable backups of the files it changed
//! (`git::Snap`, before and after). One transition serves both: a file goes back only where it
//! is as the op left it (else its backup goes to the stash list), and a ref move writes only
//! clean paths, so it never refuses. What neither covers (remotes, the stash list) runs as ops.

use super::op::{Backup, Done, Op, Undo};
use super::task::Task;
use super::{Choice, Msg, RepoTab};
use crate::git::{self, Entry, Res, Snap, Snapshot};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

pub(super) struct Refs {
    pub(super) before: Snapshot,
    pub(super) after: Snapshot,
    pub(super) mode: Undo,
    /// What [`on_remotes`] said when the op finished (a pull's commits); more later = pushed since.
    pub(super) on_remote: usize,
}

/// The files an op changed, backed up before and after it ran; `before.paths` is the scope
/// (`after.paths` is left empty).
#[derive(Clone)]
pub(super) struct Files {
    pub(super) before: Snap,
    pub(super) after: Snap,
}

pub(super) struct UndoEntry {
    pub(super) label: String,
    /// Refs moved before -> after (None: none moved).
    pub(super) refs: Option<Refs>,
    /// The files it changed, oldest first.
    pub(super) files: Vec<Files>,
    /// (undo, redo) ops for the rest, run after the files: remotes, the stash list.
    pub(super) ops: Option<(Box<Op>, Box<Op>)>,
    /// Unique, increasing: a toast's Undo button stays live while its step is on top.
    id: u64,
}

pub struct Steps(Vec<UndoEntry>, Vec<UndoEntry>);

impl UndoEntry {
    pub(super) fn new(label: String, refs: Option<Refs>, files: Vec<Files>, ops: Option<(Box<Op>, Box<Op>)>) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        UndoEntry { label, refs, files, ops, id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) }
    }
}

/// How many of the commits that moving the branches from `after` back to `before` (a
/// [`delta`]; the other branches as they are now) takes off a remote-tracking branch has.
pub(super) fn on_remotes(dir: &Path, before: &Snapshot, after: &Snapshot) -> usize {
    let tips = |s: &Snapshot, not: &str| {
        let heads = s.refs.iter().filter(|(r, _)| r.starts_with("refs/heads/")).map(|(_, o)| o);
        heads.chain(Some(&s.head).filter(|h| !h.starts_with("refs/"))).map(|o| format!("{not}{o}\n")).collect::<String>()
    };
    let now = git::run(dir, &["for-each-ref", "--format=%(objectname) %(refname)", "refs/heads"]).unwrap_or_default();
    let rest: String = now.lines().filter_map(|l| l.split_once(' ')).filter(|(_, r)| !before.refs.contains_key(*r) && !after.refs.contains_key(*r)).map(|(o, _)| format!("^{o}\n")).collect();
    let input = tips(after, "") + &tips(before, "^") + &rest;
    let count = |extra: &[&str]| {
        let args: Vec<&str> = ["rev-list", "--count", "--stdin"].into_iter().chain(extra.iter().copied()).collect();
        git::run_input(dir, &args, Some(input.as_bytes())).ok().and_then(|n| n.trim().parse::<usize>().ok()).unwrap_or(0)
    };
    count(&[]).saturating_sub(count(&["--not", "--remotes"]))
}

/// Pseudo-commands `[cmd, 1 (undo) | 0 (redo), step id]`: undo again, once confirmed (pushed
/// commits) or as a failed one's Retry, and its Skip. Only while that step is still on top.
pub(super) const UNDO_RETRY: &str = "__undo_retry__";
pub(super) const UNDO_SKIP: &str = "__undo_skip__";

pub(super) fn split_z(s: String) -> Vec<String> {
    s.split('\0').filter(|p| !p.is_empty()).map(String::from).collect()
}

/// Uncommitted paths against `base`: changed, staged, conflicted, flagged.
fn dirty(dir: &Path, base: &str) -> Res<BTreeSet<String>> {
    let z = |args: &[&str]| git::run(dir, args).map(split_z);
    let mut ps: BTreeSet<String> = z(&["diff", "--name-only", "-z", "--no-renames", base])?.into_iter().collect();
    ps.extend(z(&["diff", "--cached", "--name-only", "-z", "--no-renames", base])?);
    ps.extend(z(&["ls-files", "-z", "-u"])?.iter().filter_map(|l| Some(l.split_once('\t')?.1.to_owned())));
    ps.extend(git::flagged(dir)?);
    Ok(ps)
}

/// Every path stash `id` touches: tracked changes, staged changes and untracked files (`^3`).
fn stash_paths(dir: &Path, id: &str) -> Res<Vec<String>> {
    let z = |args: &[&str]| git::run(dir, args).map(split_z);
    let mut ps: BTreeSet<String> = z(&["diff", "--name-only", "-z", "--no-renames", &format!("{id}^1"), id])?.into_iter().collect();
    ps.extend(z(&["diff", "--name-only", "-z", "--no-renames", &format!("{id}^1"), &format!("{id}^2")])?);
    ps.extend(z(&["ls-tree", "-r", "-z", "--name-only", &format!("{id}^3")]).unwrap_or_default());
    Ok(ps.into_iter().collect())
}

/// What backup `b` covers, the commits whose content needs no backup (a tree's HEAD, where ⌘Z
/// puts it back, and a split's commit; none for paths, which go back as they were), and the
/// commits a tree names.
pub(super) fn scope(dir: &Path, b: &Backup) -> Res<(Vec<String>, Vec<String>, Vec<String>)> {
    let (revs, untracked) = match b {
        Backup::Paths(ps) => return Ok((ps.clone(), vec![], vec![])),
        Backup::Stash(id) => return Ok((stash_paths(dir, id)?, vec![], vec![])),
        Backup::Staged(ps) => {
            let want: HashSet<&String> = ps.iter().collect();
            let staged: BTreeSet<String> = git::run(dir, &["diff", "--cached", "--name-only", "-z", "--no-renames"]).map(split_z)?.into_iter().filter(|p| ps.is_empty() || want.contains(p)).collect();
            let both = git::run(dir, &["diff", "--name-only", "-z", "--no-renames"]).map(split_z)?.into_iter().any(|p| staged.contains(&p));
            return Ok((if both { staged.into_iter().collect() } else { vec![] }, vec![], vec![]));
        }
        Backup::Tree { revs, untracked } => (&revs[..], *untracked),
        Backup::Split(id) => (std::slice::from_ref(id), false),
    };
    let z = |args: &[&str]| git::run(dir, args).map(split_z);
    let head = git::head(dir);
    let base_tree = head.clone().map_or_else(|| git::empty_tree(dir), Ok)?;
    let mut ps = dirty(dir, &base_tree)?;
    ps.extend(if untracked { z(&["ls-files", "-z", "-o", "--exclude-standard"])? } else { vec![] });
    // Untracked and ignored files where the op may put a file: any commit between HEAD and
    // what it names, their parents (a revert), the upstream (a pull), and what a stopped merge,
    // pick, revert or rebase still applies.
    let g = git::git_dir(dir)?;
    let read = |f: &str| std::fs::read_to_string(g.join(f)).unwrap_or_default();
    let orig = ["rebase-merge", "rebase-apply"].map(|d| read(&format!("{d}/orig-head")).trim().to_owned());
    let todo = read("sequencer/todo");
    let picks = todo.lines().filter_map(|l| l.split_whitespace().nth(1)).map(String::from);
    let up = git::run(dir, &["rev-parse", "-q", "--verify", "@{upstream}"]).ok();
    let others = ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"].map(String::from).into_iter().chain(picks).chain(orig).chain(up);
    // (name, named by the op); an `@{…}` that doesn't resolve would stop cat-file.
    let names: Vec<(String, bool)> = revs.iter().map(|r| (r.trim().to_owned(), true)).chain(others.map(|o| (o.trim().to_owned(), false))).filter(|(n, _)| !n.is_empty() && !n.starts_with('-') && !n.contains('\n') && !n.contains("@{")).collect();
    let input: String = names.iter().map(|(n, _)| format!("{n}^{{commit}}\n{n}^^{{commit}}\n")).collect();
    let found = git::run_input(dir, &["cat-file", "--batch-check=%(objectname)"], Some(input.as_bytes()))?;
    // (commit, named by the op, a parent)
    let found: Vec<(&str, bool, bool)> = found.lines().zip(names.iter().flat_map(|n| [(n.1, false), (false, true)])).filter(|(l, _)| l.len() >= 40 && l.bytes().all(|b| b.is_ascii_hexdigit())).map(|(l, (n, p))| (l, n, p)).collect();
    let tips: BTreeSet<&str> = head.iter().map(String::as_str).chain(found.iter().map(|c| c.0)).collect();
    let mut way = vec![];
    if found.iter().any(|c| Some(c.0) != head.as_deref()) {
        // What each commit since their common base changed (a replay writes those) ...
        let mut a: Vec<&str> = vec!["merge-base", "--octopus"];
        a.extend(&tips);
        let base = git::run(dir, &a).ok().and_then(|b| Some(format!("^{}", b.lines().next()?.trim()))).unwrap_or_default();
        let input = tips.iter().map(|t| format!("{t}\n")).collect::<String>() + &base;
        // ponytail: the newest 3000 commits (~150 ms); a longer replay's oldest go unchecked.
        way = split_z(git::run_input(dir, &["log", "-n", "3000", "--format=", "--name-only", "-z", "--no-renames", "--stdin"], Some(input.as_bytes()))?);
        // ... and where each of them differs from HEAD (a far checkout).
        for c in found.iter().filter(|c| !c.2).map(|c| c.0).collect::<BTreeSet<_>>() {
            way.extend(z(&["diff-tree", "-r", "--name-only", "-z", "--no-renames", &base_tree, c])?);
        }
    }
    let index: HashSet<String> = if way.is_empty() { HashSet::new() } else { z(&["ls-files", "-z"])?.into_iter().collect() };
    let there = |p: &str| std::fs::symlink_metadata(dir.join(p)).is_ok() || git::ancestors(p).any(|a| std::fs::symlink_metadata(dir.join(a)).is_ok_and(|m| !m.is_dir()));
    ps.extend(way.into_iter().filter(|p| !index.contains(p) && there(p)));
    let named: Vec<String> = found.iter().filter(|c| c.1).map(|c| c.0.to_owned()).collect();
    let like = head.into_iter().chain(named.iter().filter(|_| matches!(b, Backup::Split(_))).cloned());
    Ok((ps.into_iter().collect(), like.collect(), named))
}

/// Backs up what `b` covers before an op runs (None: nothing there). Err refuses the op:
/// nothing has run yet.
pub(super) fn backup(dir: &Path, b: &Backup, label: &str) -> Res<Option<Snap>> {
    let refuse = |e: String| format!("Nothing was changed: couldn't back up your files first ({e}).");
    let (scope, like, named) = scope(dir, b).map_err(refuse)?;
    if scope.is_empty() { Ok(None) } else { git::snap(dir, &scope, &like, &named, &format!("Glasir backup: {label}")).map_err(refuse) }
}

/// Why `op` can't run: with `submodule.recurse` on, one that forces the tree (a discard, a hard
/// reset, an abort) resets submodules too, and no backup can hold a submodule's files.
pub(super) fn recursed(dir: &Path, op: &Op) -> Option<String> {
    let forced = op.backup.is_some() || op.cmds.iter().flatten().any(|a| matches!(a.as_str(), "--hard" | "--abort" | "-f" | "--force"));
    if !forced || !git::run(dir, &["config", "--type=bool", "submodule.recurse"]).is_ok_and(|s| s.trim() == "true") {
        return None;
    }
    // `S<c><m><u>`: m = changes to its tracked files.
    let p = git::status(dir).ok()?.submodules.into_iter().filter(|(_, f)| f.as_bytes().get(2) == Some(&b'M')).map(|(p, _)| p).find(|p| super::tools::touches(op, p))?;
    Some(format!("Nothing was changed: submodule {p} has uncommitted changes this would reset (submodule.recurse is on), and they can't be backed up. Commit or stash them in {p} first."))
}

/// The op's files once it ran (`before`: from [`backup`] of `b`, taken at HEAD `head`): None
/// when it changed none of them and left HEAD there (it failed, say), and their backups go. A
/// tree backup of an op that left HEAD there (a squash merge) also covers the tracked files it
/// changed: those were clean then (None: all were). Err: a note, what it replaced is in the stash list.
pub(super) fn backed_up(dir: &Path, b: &Backup, before: Option<Snap>, head: Option<&str>, label: &str) -> Result<Option<Files>, String> {
    let still = head.filter(|h| matches!(b, Backup::Tree { .. }) && git::head(dir).as_deref() == Some(h) && git::git_dir(dir).is_ok_and(|g| git::state(&g) == git::State::Clean));
    let more = still.map_or(Ok(BTreeSet::new()), |h| dirty(dir, h)).unwrap_or_default();
    let before = match (before, still) {
        (Some(s), _) => s,
        (None, Some(h)) if !more.is_empty() => match git::clean_snap(dir, h, &format!("Glasir backup: {label}")) {
            Ok(s) => s,
            Err(_) => return Ok(None),
        },
        _ => return Ok(None),
    };
    let scope: Vec<String> = before.paths.iter().cloned().chain(more).collect::<BTreeSet<_>>().into_iter().collect();
    let before = Snap { paths: scope.clone(), ..before };
    match git::snap(dir, &scope, &[], &[], &format!("Glasir backup: after {label}")) {
        Ok(Some(after)) if head.is_none_or(|h| git::head(dir).as_deref() == Some(h)) && unchanged(dir, &before, &after) => {
            git::drop_backups(dir, &[before.refname, after.refname]);
            Ok(None)
        }
        Ok(after) => Ok(after.map(|after| Files { before, after: Snap { paths: vec![], ..after } })),
        Err(e) => {
            let msg = format!("Glasir: {label}");
            git::keep(dir, &before.commit, &msg).map_err(|k| format!("{label} can't be undone ({e}), and its backup stays only in {} ({k}).", before.refname))?;
            git::drop_backups(dir, &[before.refname]);
            Err(format!("{label} can't be undone ({e}); what it replaced is in the stash list as \"{msg}\"."))
        }
    }
}

/// Whether backup `after` holds `before`'s paths as `before` does, each against its own HEAD.
pub(super) fn unchanged(dir: &Path, before: &Snap, after: &Snap) -> bool {
    git::view(dir, before, &before.paths).is_ok_and(|b| git::view(dir, after, &before.paths).is_ok_and(|a| a == b))
}

type Half<'a> = (Option<&'a (u32, String)>, bool);

/// A path's two halves: its file, and its index entry (staged version, conflicted).
fn halves(e: Option<&Entry>) -> [Half<'_>; 2] {
    e.map_or([(None, false); 2], |e| [(e.disk.as_ref(), false), (e.idx.as_ref(), e.unmerged)])
}

/// Equal, and readable: a [`git::FOREIGN`] file equals nothing.
fn eq(a: Half, b: Half) -> bool {
    a == b && a.0.is_none_or(|(_, o)| o != git::FOREIGN)
}

/// Legs (from, to) in the order they apply (undo: newest first, from=after to=before; redo:
/// oldest first, from=before to=after). Each half (file, index entry) a leg changes moves from
/// `from` to `to` only while it holds `from` and nothing at, under or above the path changed in
/// the way since. Returns the entries whose file, and those whose index entry, differ from
/// `cur`, and each leg not fully applied, with the paths that changed.
pub(super) fn plan(cur: &git::View, legs: &[(git::View, git::View)]) -> (Vec<Entry>, Vec<Entry>, Vec<(usize, Vec<String>)>) {
    let mut v = cur.clone();
    let mut partial = vec![];
    for (i, (from, to)) in legs.iter().enumerate() {
        let mut keys: BTreeSet<String> = from.keys().chain(to.keys()).cloned().collect();
        let touched: Vec<String> = {
            let related = git::related(&keys);
            v.keys().filter(|k| related(k)).cloned().collect()
        };
        keys.extend(touched);
        // Clashes: a half the leg moves changed since; strays: a path it leaves alone changed.
        let (mut want, mut clash, mut stray) = (BTreeMap::new(), vec![], vec![]);
        for k in &keys {
            let (c, f, t) = (halves(v.get(k)), halves(from.get(k)), halves(to.get(k)));
            let moves: Vec<usize> = (0..2).filter(|&h| !eq(f[h], t[h])).collect();
            if moves.is_empty() {
                if (0..2).any(|h| !eq(c[h], f[h])) {
                    stray.push(k.clone());
                }
            } else if moves.iter().any(|&h| !eq(c[h], f[h]) && !eq(c[h], t[h])) {
                clash.push(k.clone());
            } else if moves.iter().any(|&h| !eq(c[h], t[h])) {
                want.insert(k.clone(), moves);
            }
        }
        let near = git::related(want.keys());
        let changed: Vec<String> = clash.into_iter().chain(stray.into_iter().filter(|k| near(k))).collect();
        let blocked = git::related(&changed);
        for (k, moves) in want.iter().filter(|(k, _)| !blocked(k)) {
            let t = to.get(k);
            let name = [&v, from, to].iter().find_map(|m| m.get(k)).map_or(k.clone(), |e| e.name.clone());
            let mut e = v.get(k).cloned().unwrap_or(Entry { name, disk: None, idx: None, unmerged: false });
            for h in moves {
                match h {
                    0 => e.disk = t.and_then(|t| t.disk.clone()),
                    _ => (e.idx, e.unmerged) = t.map_or((None, false), |t| (t.idx.clone(), t.unmerged)),
                }
            }
            match e.disk.is_none() && e.idx.is_none() && !e.unmerged {
                true => drop(v.remove(k)),
                false => drop(v.insert(k.clone(), e)),
            }
        }
        if !changed.is_empty() {
            let name = |k: &String| [&v, from, to].iter().find_map(|m| m.get(k)).map_or(k.clone(), |e| e.name.clone());
            partial.push((i, changed.iter().map(name).collect()));
        }
    }
    let (mut files, mut index) = (vec![], vec![]);
    for k in cur.keys().chain(v.keys()).collect::<BTreeSet<_>>() {
        let (a, b) = (halves(cur.get(k)), halves(v.get(k)));
        let e = || v.get(k).cloned().unwrap_or_else(|| Entry { name: cur[k].name.clone(), disk: None, idx: None, unmerged: false });
        if a[0] != b[0] {
            files.push(e());
        }
        if a[1] != b[1] {
            index.push(e());
        }
    }
    (files, index, partial)
}

/// `s`'s checked-out commit (None: an unborn branch).
fn tip(s: &Snapshot) -> Option<String> {
    s.refs.get(&s.head).cloned().or_else(|| (!s.head.starts_with("refs/")).then(|| s.head.clone()))
}

/// Where ref `r` (or `HEAD`) is in `s`.
pub(super) fn at<'a>(s: &'a Snapshot, r: &str) -> Option<&'a String> {
    if r == "HEAD" { Some(&s.head) } else { s.refs.get(r) }
}

/// Sets ref `r` (or `HEAD`) in `to` as it is in `from`.
pub(super) fn copy(to: &mut Snapshot, from: &Snapshot, r: &str) {
    match (r, from.refs.get(r)) {
        ("HEAD", _) => to.head = from.head.clone(),
        (_, Some(o)) => drop(to.refs.insert(r.to_owned(), o.clone())),
        (_, None) => drop(to.refs.remove(r)),
    }
}

/// The refs that differ between `a` and `b`, and `HEAD` when it does.
pub(super) fn moved(a: &Snapshot, b: &Snapshot) -> Vec<String> {
    let m: BTreeSet<&String> = a.refs.keys().chain(b.refs.keys()).filter(|r| a.refs.get(*r) != b.refs.get(*r)).collect();
    m.into_iter().cloned().chain((a.head != b.head).then(|| "HEAD".to_owned())).collect()
}

pub(super) fn delta(mut b: Snapshot, mut a: Snapshot) -> (Snapshot, Snapshot) {
    let mv: HashSet<String> = moved(&b, &a).into_iter().collect();
    b.refs.retain(|r, _| mv.contains(r));
    a.refs.retain(|r, _| mv.contains(r));
    (b, a)
}

pub(super) fn own(op: &Op, before: &Snapshot, mut after: Snapshot) -> Snapshot {
    if op.custom {
        return after;
    }
    let text = op.cmds.iter().flatten().fold(String::from_utf8_lossy(op.input.as_deref().unwrap_or_default()).into_owned(), |t, a| t + "\n" + a);
    let names: HashSet<&str> = text
        .split(|c: char| c.is_whitespace() || c == ':')
        .flat_map(|t| [Some(t), t.strip_prefix('+'), t.split_once('=').map(|(_, v)| v)])
        .flatten()
        .collect();
    let head = after.head.clone();
    after.refs.retain(|r, _| {
        let short = r.strip_prefix("refs/heads/").or_else(|| r.strip_prefix("refs/tags/")).unwrap_or(r);
        before.refs.contains_key(r) || *r == before.head || *r == head || names.contains(r.as_str()) || names.contains(short)
    });
    after
}

/// One undo or redo: `refs` (from, to, mode) move if each ref they move is still at either
/// side (and HEAD is still on the branch a soft or mixed one moves), then `legs` (from, to,
/// scope) apply in order. Files go first and refs last, in one transaction: a failure leaves
/// the refs as they were, and a retry finishes the job. Ok: one note, and whether the refs
/// were valid (else only the files were put back).
pub(super) fn transition(dir: &Path, label: &str, refs: Option<(Snapshot, Snapshot, Undo)>, legs: &[(Snap, Snap, Vec<String>)]) -> Res<(Option<String>, bool)> {
    let mut notes = vec![];
    let cur_refs = git::snapshot(dir)?;
    let valid = refs.as_ref().is_none_or(|(f, t, m)| {
        let mv = moved(f, t);
        mv.iter().all(|r| at(&cur_refs, r) == at(f, r) || at(&cur_refs, r) == at(t, r)) && (!matches!(m, Undo::Soft | Undo::Mixed) || !mv.contains(&f.head) || cur_refs.head == f.head)
    });
    if !valid {
        notes.push(format!("{label}: its branches changed outside Glasir since, so {}.", if legs.is_empty() { "nothing was moved" } else { "only its files were put back" }));
    }
    let refs = refs.filter(|_| valid).map(|(f, t, m)| (moved(&f, &t), t, m));
    if let Some((mv, ..)) = &refs
        && let Some(r) = git::used_elsewhere(dir)?.into_iter().find(|r| *r != cur_refs.head && mv.contains(r))
    {
        return Err(format!("{} is in use in another worktree (checked out, or being rebased or bisected there); finish there or switch that worktree to another branch first, then try again.", r.strip_prefix("refs/heads/").unwrap_or(&r)));
    }
    let head_now = git::head(dir).map_or_else(|| git::empty_tree(dir), Ok)?;
    // Where HEAD ends up: the refs it moves as in `to`, the others as they are now.
    let mut end = cur_refs.clone();
    refs.iter().for_each(|(mv, t, _)| mv.iter().for_each(|r| copy(&mut end, t, r)));
    if let Some((mv, t, _)) = &refs
        && mv.contains(&end.head)
        && !end.refs.contains_key(&end.head)
        && (end.refs.contains_key(&t.head) || !t.head.starts_with("refs/"))
    {
        return Err(format!("{} is checked out here; switch to another branch first, then try again.", end.head.strip_prefix("refs/heads/").unwrap_or(&end.head)));
    }
    let final_head = tip(&end).map_or_else(|| git::empty_tree(dir), Ok)?;
    let scope: Vec<String> = legs.iter().flat_map(|l| l.2.iter().cloned()).collect::<BTreeSet<_>>().into_iter().collect();
    let cur = if legs.is_empty() { git::View::new() } else { git::current(dir, &scope)? };
    let views: Vec<(git::View, git::View)> = legs.iter().map(|(from, to, sc)| {
        git::view(dir, from, sc).and_then(|f| Ok((f, git::view(dir, to, sc)?))).unwrap_or_else(|e| {
            notes.push(format!("A backup of {label} is gone ({e}), so it was skipped."));
            Default::default()
        })
    }).collect();
    let (files, mut index, partial) = plan(&cur, &views);
    if let Some((_, _, mode)) = &refs
        && final_head != head_now
    {
        // An index entry that follows HEAD follows the new one: every path written gets one.
        index.extend(files.iter().filter(|f| !index.iter().any(|i| i.name == f.name)).cloned().collect::<Vec<_>>());
        let back: Vec<Entry> = views.iter().flat_map(|(f, t)| t.iter().filter(|(k, e)| f.get(*k) == Some(e) && cur.get(*k) == Some(e)).map(|(_, e)| e.clone())).collect();
        index.extend(back.into_iter().filter(|b| !index.iter().any(|i| i.name == b.name)).collect::<Vec<_>>());
        let kept = git::move_files(dir, &head_now, &final_head, mode.mode(), &index)?;
        if !kept.is_empty() {
            notes.push(format!("Kept your changes to {} (they now differ from {}).", kept.join(", "), &final_head[..final_head.len().min(7)]));
        }
    }
    git::write(dir, &files, &index, &final_head, true)?;
    if let Some((mv, to, _)) = &refs {
        git::move_refs(dir, &cur_refs, to, mv, label)?;
    }
    // A resolution undone gets its conflict back (git's resolve-undo), while that merge still runs
    // or HEAD is where the conflict was (a stash pop's, redone).
    let written: HashSet<&str> = index.iter().map(|e| e.name.as_str()).collect();
    let running = git::git_dir(dir).is_ok_and(|g| !matches!(git::state(&g), git::State::Clean | git::State::Bisecting));
    let there = |s: &Snap| git::run(dir, &["rev-parse", &format!("{}^1", s.commit)]).is_ok_and(|b| b.trim() == final_head);
    let whole = legs.iter().enumerate().filter(|(i, l)| !partial.iter().any(|(p, _)| p == i) && !l.1.unmerged.is_empty() && (running || there(&l.1)));
    let unresolve: Vec<&str> = whole.flat_map(|(_, l)| l.1.unmerged.iter().map(String::as_str)).filter(|p| written.contains(p)).collect();
    if !unresolve.is_empty() {
        // One path per run: before 2.43 one path git can't unresolve (resolved by deletion) made
        // it write none. No `--`: `--unresolve` takes every argument after it as a path.
        for p in unresolve {
            let _ = git::run(dir, &["update-index", "--unresolve", p]);
        }
    }
    let msg = format!("Glasir: {label}");
    let mut kept = BTreeSet::new();
    for (j, changed) in &partial {
        git::keep(dir, &legs[*j].1.commit, &msg)?;
        kept.extend(changed.iter().cloned());
    }
    if !kept.is_empty() {
        let it = if kept.len() == 1 { "its backup is" } else { "their backups are" };
        notes.push(format!("{} changed since, so {it} in the stash list as \"{msg}\" instead of put back.", kept.into_iter().collect::<Vec<_>>().join(", ")));
    }
    Ok(((!notes.is_empty()).then(|| notes.join("\n")), valid))
}

impl RepoTab {
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|u| u.label.as_str())
    }
    /// The newest undo step's id; a new step (or a redo) makes it larger.
    pub fn undo_top(&self) -> Option<u64> {
        self.undo.last().map(|u| u.id)
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|u| u.label.as_str())
    }
    pub fn take_steps(&mut self) -> Steps {
        Steps(std::mem::take(&mut self.undo), std::mem::take(&mut self.redo))
    }
    pub fn put_steps(&mut self, s: Steps) {
        (self.undo, self.redo) = (s.0, s.1);
    }

    /// A new step makes the redos stale: their backups go (off the UI thread).
    pub(super) fn clear_redo(&mut self) {
        let refs: Vec<String> = self.redo.drain(..).flat_map(|e| e.files).flat_map(|f| [f.before.refname, f.after.refname]).collect();
        if !refs.is_empty() {
            let dir = self.dir.clone();
            std::thread::spawn(move || git::drop_backups(&dir, &refs));
        }
    }

    /// Retry / Skip of a failed undo (`undo`) or redo, if the step `id` is still on top.
    pub(super) fn retry_or_skip(&mut self, ctx: &egui::Context, skip: bool, undo: bool, id: &str) {
        let stack = if undo { &mut self.undo } else { &mut self.redo };
        let Some(top) = stack.last().filter(|e| e.id.to_string() == id) else { return };
        if let Some((busy, _)) = &self.running {
            let what = if skip { "Skip" } else if undo { "Undo" } else { "Redo" };
            return self.notes.push(Msg { text: format!("'{what} {}' skipped: '{busy}' is still running.", top.label), error: true });
        }
        if !skip {
            return self.undo_redo(ctx, undo, true);
        }
        let Some(e) = stack.pop() else { return };
        let msg = format!("Glasir: {} {}", if undo { "Undo" } else { "Redo" }, e.label);
        let dir = self.dir.clone();
        self.running = Some((format!("Skip {}", e.label), Task::spawn(ctx, move || {
            // Each side the files no longer hold: a half-done step may have overwritten either. One
            // that couldn't be kept stays in refs/glasir/backups (pruned in 7 days).
            let (mut done, mut names) = (vec![], BTreeSet::new());
            for f in &e.files {
                let cur = git::current(&dir, &f.before.paths).ok();
                for (s, name) in [(&f.before, msg.clone()), (&f.after, format!("{msg} (its result)"))] {
                    let need = cur.is_none() || git::view(&dir, s, &f.before.paths).ok() != cur;
                    if need && git::keep(&dir, &s.commit, &name).is_ok() {
                        names.insert(format!("\"{name}\""));
                    } else if need {
                        continue;
                    }
                    done.push(s.refname.clone());
                }
            }
            git::drop_backups(&dir, &done);
            let mut listed: HashSet<String> = git::stashes(&dir).unwrap_or_default().into_iter().map(|s| s.id).collect();
            for c in e.ops.iter().flat_map(|(u, _)| &u.cmds) {
                match c.iter().map(String::as_str).collect::<Vec<_>>()[..] {
                    ["stash", "store", "-m", m, id] | [super::op::STASH_UNPOP | super::op::STASH_RENAME, id, m] => {
                        if listed.insert(id.to_owned()) && git::run(&dir, &["cat-file", "-e", &format!("{id}^{{commit}}")]).is_ok() && git::keep(&dir, id, m).is_ok() {
                            names.insert(format!("\"{m}\""));
                        }
                    }
                    [super::op::BRANCH_RESTORE, b, oid, ..] if git::run(&dir, &["for-each-ref", "--count=1", "--contains", oid, "--format=x", "refs/heads", "refs/remotes", "refs/tags"]).is_ok_and(|o| o.trim().is_empty()) => {
                        let name = format!("{msg} (branch {b} at {})", &oid[..oid.len().min(7)]);
                        if let Ok(s) = git::clean_snap(&dir, oid, &name)
                            && git::keep(&dir, &s.commit, &name).is_ok()
                        {
                            git::drop_backups(&dir, &[s.refname]);
                            names.insert(format!("\"{name}\""));
                        }
                    }
                    _ => {}
                }
            }
            let names = names.into_iter().collect::<Vec<_>>().join(" and ");
            Done::Skipped(format!("Skipped {}{}.", e.label, if names.is_empty() { String::new() } else { format!(": its files are in the stash list as {names}") }))
        })));
    }

    /// `sure`: the user confirmed dropping commits that were pushed since the step.
    pub(super) fn undo_redo(&mut self, ctx: &egui::Context, undo: bool, sure: bool) {
        let stack = if undo { &self.undo } else { &self.redo };
        let Some(e) = stack.last() else { return };
        if self.running.is_some() {
            return;
        }
        let touched = |p: &str| {
            e.refs.is_some()
                || e.files.iter().any(|f| f.before.paths.iter().any(|q| q == p || p.strip_prefix(q.as_str()).is_some_and(|r| r.starts_with('/'))))
                || e.ops.as_ref().is_some_and(|(u, r)| super::tools::touches(if undo { u } else { r }, p))
        };
        let label = format!("{} {}", if undo { "Undo" } else { "Redo" }, e.label);
        if let Some(open) = self.ext_tools.iter().find(|t| t.kind == "merge" && touched(&t.path)).map(|t| t.path.clone()) {
            self.tool_blocks(&label, |p| p == open);
            return;
        }
        if let (true, false, Some(r)) = (undo, sure, &e.refs)
            && let n = on_remotes(&self.dir, &r.before, &r.after)
            && n > r.on_remote
        {
            let n = n - r.on_remote;
            let mut op = Op::new(label, &[UNDO_RETRY, "1", &e.id.to_string()]);
            let (what, it) = if n == 1 { ("The commit this undoes is".to_owned(), "it") } else { (format!("{n} commits this undoes are"), "them") };
            op.confirm = Some(format!("{what} already pushed. Undo only moves your local branch: the remote keeps {it}, so dropping {it} there needs a force push (otherwise a pull brings {it} back)."));
            self.confirm = Some(op);
            return;
        }
        // Moving refs mid merge/rebase would corrupt it; a guided split's own steps are fine, and
        // so is a step whose op state (a merge it concluded or started) is the one in progress.
        let d = self.data.as_ref().filter(|d| !matches!(git::state(&d.git_dir), git::State::Clean | git::State::Bisecting));
        let own = d.and_then(|d| e.ops.as_ref().and_then(|(u, r)| super::op::own_state(&d.git_dir, if undo { u } else { r })));
        if d.is_some() && self.split.as_ref().is_none_or(|sp| sp.replaying) && (e.refs.is_some() || own.is_some()) && own != Some(true) {
            return self.notes.push(Msg { text: "Finish or abort the operation in progress first, then undo.".into(), error: true });
        }
        let dir = self.dir.clone();
        // Ok: a redone op's fresh undo, a note, and whether the refs were still valid.
        type Job = Box<dyn FnOnce() -> Res<(Option<Box<Op>>, Option<String>, bool)> + Send>;
        // Undo: from after to before, newest first; redo: the other way, oldest first.
        let refs = e.refs.as_ref().map(|r| if undo { (r.after.clone(), r.before.clone(), r.mode) } else { (r.before.clone(), r.after.clone(), r.mode) });
        let mut legs: Vec<(Snap, Snap, Vec<String>)> = e.files.iter().map(|f| if undo { (f.after.clone(), f.before.clone(), f.before.paths.clone()) } else { (f.before.clone(), f.after.clone(), f.before.paths.clone()) }).collect();
        if undo {
            legs.reverse();
        }
        let op = e.ops.as_ref().map(|(u, r)| if undo { (**u).clone() } else { (**r).clone() });
        let common = op.as_ref().filter(|o| super::op::network(o)).map(|_| self.common.clone());
        let l = label.clone();
        let job: Job = Box::new(move || {
            let _turn = common.map(super::fetch::Fetching::user);
            let (note, valid) = if refs.is_some() || !legs.is_empty() { transition(&dir, &l, refs, &legs)? } else { (None, true) };
            let fresh = match op {
                Some(op) if undo => super::op::exec(&dir, &op).map(|_| None)?,
                // A redone stash save made a new stash: its undo pops that one.
                Some(mut op) => super::op::exec_stash(&dir, &mut op).map(|_| op.revert.take())?,
                None => None,
            };
            Ok((fresh, note, valid))
        });
        let id = e.id;
        self.running = Some((label, Task::spawn(ctx, move || Done::Undo(undo, id, job()))));
    }

    /// A finished undo or redo: Ok moves the step to the other stack; Err keeps it and offers
    /// Retry, or Skip (its files go to the stash list), so ⌘Z never sticks.
    pub(super) fn undone(&mut self, undo: bool, id: u64, res: Res<(Option<Box<Op>>, Option<String>, bool)>, msgs: &mut Vec<Msg>) {
        let (from, to) = if undo { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
        // By id: a merge tool closing meanwhile may have pushed a step on top.
        let at = from.iter().position(|e| e.id == id);
        match res {
            Ok((fresh, note, valid)) => {
                if let Some(mut e) = at.map(|i| from.remove(i)) {
                    if let (Some(f), Some((u, _))) = (fresh, &mut e.ops) {
                        *u = f;
                    }
                    e.refs = e.refs.take().filter(|_| valid);
                    // Nothing left to redo when its refs changed outside and it had nothing else.
                    if e.refs.is_some() || !e.files.is_empty() || e.ops.is_some() {
                        to.push(e);
                    }
                }
                msgs.extend(note.map(|text| Msg { text, error: true }));
            }
            Err(e) => {
                let Some(top) = at.map(|i| &from[i]) else { return };
                let (u, id, what) = (if undo { "1" } else { "0" }, id.to_string(), if undo { "Undo" } else { "Redo" });
                let ops = vec![("Retry".to_owned(), Op::new("Retry", &[UNDO_RETRY, u, &id])), ("Skip it: keep its files in the stash list".to_owned(), Op::new("Skip", &[UNDO_SKIP, u, &id]))];
                self.choice = Some(Choice { title: format!("{what} failed"), text: format!("{what} {} didn't finish:\n{e}\n\nRetry once that's sorted out (it picks up where it stopped), or skip the step: its files go to the stash list.", top.label), ops });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{commit, identity, run_backed, scratch, settle, sh, undo_files};
    use super::super::{Action, View, reset_op};
    use super::*;

    fn e(name: &str, disk: Option<&str>, idx: Option<&str>) -> Entry {
        let m = |o: &str| (0o100644, o.to_owned());
        Entry { name: name.into(), disk: disk.map(m), idx: idx.map(m), unmerged: false }
    }

    fn view(es: &[Entry]) -> git::View {
        es.iter().map(|e| (e.name.to_lowercase(), e.clone())).collect()
    }

    /// A scratch repo with an identity and one commit `keep`.
    fn repo(name: &str) -> std::path::PathBuf {
        let dir = scratch(name);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "keep", "k\n");
        dir
    }

    fn tab(dir: &Path) -> (egui::Context, RepoTab) {
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.to_path_buf(), 100, View::default());
        settle(&ctx, &mut t);
        (ctx, t)
    }

    /// Applies `a` and waits: the toasts, then the notes.
    fn step(ctx: &egui::Context, t: &mut RepoTab, a: Action) -> Vec<String> {
        t.apply(ctx, a);
        let mut m: Vec<String> = settle(ctx, t).into_iter().map(|m| m.text).collect();
        m.extend(t.notes.drain(..).map(|n| n.text));
        m
    }

    fn hard_reset(ctx: &egui::Context, t: &mut RepoTab, id: &str) -> Vec<String> {
        let mut op = reset_op(t.data.as_ref().unwrap(), id, "--hard");
        op.confirm = None;
        step(ctx, t, Action::Git(op))
    }

    fn discard(p: &str) -> Action {
        Action::Git(Op::new("Discard", &["--literal-pathspecs", "checkout", "--", p]).light().backup(Backup::Paths(vec![p.into()])))
    }

    fn clean(p: &str) -> Action {
        Action::Git(Op::new("Discard", &["--literal-pathspecs", "clean", "-fq", "--", p]).light().backup(Backup::Paths(vec![p.into()])))
    }

    fn branch(name: &str) -> Action {
        Action::Git(Op::new(format!("Create branch {name}"), &["branch", name]).undo(Undo::Keep))
    }

    fn discard_all() -> Action {
        Action::Git(Op::new("Discard all", &[crate::repo::DISCARD_ALL]).backup(Backup::Tree { revs: vec![], untracked: true }))
    }

    fn labels(t: &RepoTab) -> Vec<String> {
        t.undo.iter().map(|e| e.label.clone()).collect()
    }

    fn read(dir: &Path, p: &str) -> Option<String> {
        std::fs::read_to_string(dir.join(p)).ok()
    }

    fn rev(dir: &Path, r: &str) -> String {
        sh(dir, &["rev-parse", r]).trim().to_owned()
    }

    /// Some stash entry holds `want` at `p`: its files, index (`^2`) or untracked files (`^3`).
    fn in_stash(dir: &Path, p: &str, want: &str) -> bool {
        let list = git::run(dir, &["stash", "list", "--format=%H"]).unwrap_or_default();
        list.lines().any(|h| ["", "^2", "^3"].iter().any(|s| git::run(dir, &["show", &format!("{h}{s}:{p}")]).is_ok_and(|s| s == want)))
    }

    /// `want` on disk at `p` or kept in the stash list.
    fn safe(dir: &Path, p: &str, want: &str) -> bool {
        read(dir, p).as_deref() == Some(want) || in_stash(dir, p, want)
    }

    fn backups(dir: &Path) -> usize {
        sh(dir, &["for-each-ref", "refs/glasir/backups/"]).lines().count()
    }

    /// Waits for `clear_redo`'s background drop.
    fn backups_settle(dir: &Path, want: usize) -> usize {
        for _ in 0..400 {
            if backups(dir) == want {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        backups(dir)
    }

    #[cfg(unix)]
    fn mode(dir: &Path, p: &str) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(dir.join(p)).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn chmod(dir: &Path, p: &str, m: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join(p), std::fs::Permissions::from_mode(m)).unwrap();
    }

    // ---- plan(): pure, over views ----

    #[test]
    fn plan_writes_what_still_holds_the_op_result() {
        let (after, before) = (view(&[e("a", Some("1"), None)]), view(&[e("a", Some("mine"), Some("st"))]));
        // Equal to `from`: goes to `to`, file and index entry.
        let mine = e("a", Some("mine"), Some("st"));
        assert_eq!(plan(&view(&[e("a", Some("1"), None)]), &[(after.clone(), before.clone())]), (vec![mine.clone()], vec![mine], vec![]));
        // Already at `to`: nothing to write.
        assert_eq!(plan(&before, &[(after.clone(), before.clone())]), (vec![], vec![], vec![]));
        // Changed since: nothing written, the leg is partial.
        assert_eq!(plan(&view(&[e("a", Some("new"), None)]), &[(after.clone(), before.clone())]), (vec![], vec![], vec![(0, vec!["a".to_owned()])]));
        // Unreadable now: never equal, never overwritten.
        let mut f = e("a", None, None);
        f.disk = Some((0o100644, git::FOREIGN.into()));
        let (w, i, p) = plan(&view(&[f]), &[(view(&[e("a", Some("1"), None)]), view(&[e("a", Some("2"), None)]))]);
        assert!(w.is_empty() && i.is_empty() && p.len() == 1);
        // Gone in `to`: removed; missing from a view = absent.
        assert_eq!(plan(&view(&[e("u", Some("x"), None)]), &[(view(&[e("u", Some("x"), None)]), view(&[]))]), (vec![e("u", None, None)], vec![], vec![]));
    }

    /// A half the leg doesn't move (a mixed reset's file) is neither checked nor written, and
    /// a change to a path it leaves alone is no clash.
    #[test]
    fn plan_moves_only_the_halves_a_leg_changed() {
        let leg = (view(&[e("f", Some("w"), None), e("g", Some("g"), None)]), view(&[e("f", Some("w"), Some("st")), e("g", Some("g"), None)]));
        let cur = view(&[e("f", Some("edited"), None), e("g", Some("edited"), None)]);
        assert_eq!(plan(&cur, &[leg]), (vec![], vec![e("f", Some("edited"), Some("st"))], vec![]));
    }

    #[test]
    fn plan_never_writes_next_to_a_change() {
        let leg = (view(&[e("d/x", None, None)]), view(&[e("d/x", Some("x"), None)]));
        // A file where the folder goes (an ancestor), or something new under a path: the related key stays.
        for cur in [view(&[e("d", Some("new"), None)]), view(&[e("d/x/y", Some("y"), None)])] {
            let (w, _, p) = plan(&cur, &[leg.clone()]);
            assert!(w.is_empty(), "{w:?}");
            assert_eq!(p.len(), 1);
        }
        // An unrelated change doesn't block it.
        assert_eq!(plan(&view(&[e("dd", Some("new"), None)]), &[leg.clone()]), (vec![e("d/x", Some("x"), None)], vec![], vec![]));
        // Keys are folded case: `README` now where `Readme` was backed up is the same path.
        let leg = (view(&[e("readme", None, None)]), view(&[e("Readme", Some("mine"), None)]));
        let (w, _, p) = plan(&view(&[e("README", Some("theirs"), None)]), &[leg]);
        assert!(w.is_empty() && p.len() == 1);
    }

    #[test]
    fn plan_chains_legs_newest_first() {
        // Two discards of `b` (FIVE then SIX), undone newest first: SIX goes back, FIVE is kept.
        let five = (view(&[e("b", Some("1"), None)]), view(&[e("b", Some("FIVE"), None)]));
        let six = (view(&[e("b", Some("1"), None)]), view(&[e("b", Some("SIX"), None)]));
        assert_eq!(plan(&view(&[e("b", Some("1"), None)]), &[six.clone(), five.clone()]), (vec![e("b", Some("SIX"), None)], vec![], vec![(1, vec!["b".to_owned()])]));
        // A chain that hands the path on: undo of a discard, then of a restore under it.
        let restore = (view(&[e("b", Some("FIVE"), None)]), view(&[e("b", Some("old"), None)]));
        assert_eq!(plan(&view(&[e("b", Some("1"), None)]), &[five, restore]), (vec![e("b", Some("old"), None)], vec![], vec![]));
    }

    // ---- git layer: leaves, snap ----

    #[test]
    fn leaves_cover_ignored_files_nested_repos_and_files_in_a_folders_place() {
        let dir = repo("undo-leaves");
        commit(&dir, ".gitignore", "*.log\n");
        std::fs::create_dir_all(dir.join("d/n")).unwrap();
        std::fs::write(dir.join("d/x.log"), "ign").unwrap();
        std::fs::write(dir.join("d/t"), "t").unwrap();
        sh(&dir.join("d/n"), &["init", "-q"]);
        std::fs::write(dir.join("d/n/f"), "f").unwrap();
        std::fs::write(dir.join("f"), "a file").unwrap();
        let l = git::leaves(&dir, &["d".into(), "f/x".into(), "gone".into()]).unwrap();
        assert_eq!(l, ["d/n/", "d/t", "d/x.log", "f", "f/x", "gone"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn snap_keeps_modes_links_and_big_files_and_refuses_what_git_cant_store() {
        let dir = repo("undo-snap");
        std::fs::write(dir.join("run.sh"), "#!/bin/sh\n").unwrap();
        chmod(&dir, "run.sh", 0o755);
        std::os::unix::fs::symlink("keep", dir.join("lnk")).unwrap();
        let f = std::fs::File::create(dir.join("big")).unwrap();
        f.set_len(300 << 20).unwrap();
        let scope: Vec<String> = ["run.sh", "lnk", "big"].map(String::from).to_vec();
        let s = git::snap(&dir, &scope, &[], &[], "m").unwrap().unwrap();
        let ls = sh(&dir, &["ls-tree", &format!("{}^3", s.commit)]);
        assert!(ls.contains("100755 blob") && ls.contains("120000 blob"), "{ls}");
        assert_eq!(sh(&dir, &["cat-file", "-s", &format!("{}^3:big", s.commit)]).trim(), (300u64 << 20).to_string());
        assert!(sh(&dir, &["for-each-ref", "refs/glasir/backups/"]).contains(&s.commit));
        std::fs::write(dir.join("git~1"), "x").unwrap();
        assert!(git::snap(&dir, &["git~1".into()], &[], &[], "m").is_err_and(|e| e.contains("git~1")));
        // Nothing uncommitted, measured against HEAD: no backup.
        assert!(git::snap(&dir, &["keep".into()], &["HEAD".into()], &[], "m").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snap_and_undo_work_on_an_unborn_branch_and_mid_conflict() {
        let dir = scratch("undo-unborn");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        std::fs::write(dir.join("u"), "u\n").unwrap();
        std::fs::write(dir.join("s"), "staged\n").unwrap();
        sh(&dir, &["add", "s"]);
        let op = Op::new("Discard all", &["rm", "-q", "--cached", "s"]).then(&["clean", "-fdq"]).backup(Backup::Tree { revs: vec![], untracked: true });
        let f = run_backed(&dir, &op).unwrap().unwrap();
        assert!(!dir.join("s").exists() && !dir.join("u").exists());
        undo_files(&dir, &f).unwrap();
        assert_eq!((read(&dir, "s"), read(&dir, "u")), (Some("staged\n".into()), Some("u\n".into())));
        assert_eq!(sh(&dir, &["status", "--porcelain"]), "A  s\n?? u\n");
        sh(&dir, &["rm", "-qf", "s"]);

        commit(&dir, "f", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "f", "theirs\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "f", "ours\n");
        assert!(git::run(&dir, &["merge", "-q", "feat"]).is_err());
        let conflicted = read(&dir, "f");
        let op = Op::new("Discard all", &[crate::repo::DISCARD_ALL]).backup(Backup::Tree { revs: vec![], untracked: true });
        let f = run_backed(&dir, &op).unwrap().unwrap();
        assert!(!dir.join("u").exists());
        undo_files(&dir, &f).unwrap();
        assert_eq!((read(&dir, "f"), read(&dir, "u")), (conflicted, Some("u\n".into())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S (staged), D (edited, untracked in the way) and clean paths under each mode.
    #[test]
    fn ref_moves_write_only_clean_paths() {
        for mode in ["--soft", "--mixed", "--keep"] {
            let dir = repo(&format!("undo-move-{}", &mode[2..]));
            for f in ["a", "b", "c", "u"] {
                commit(&dir, f, "1\n");
            }
            let c1 = rev(&dir, "HEAD");
            sh(&dir, &["rm", "-q", "u"]);
            for f in ["a", "b", "c"] {
                commit(&dir, f, "2\n");
            }
            let c2 = rev(&dir, "HEAD");
            std::fs::write(dir.join("a"), "staged\n").unwrap();
            sh(&dir, &["add", "a"]);
            std::fs::write(dir.join("b"), "edited\n").unwrap();
            std::fs::write(dir.join("u"), "mine\n").unwrap();
            let kept = git::move_files(&dir, &c2, &c1, mode, &[]).unwrap();
            let files = ["a", "b", "c", "u"].map(|f| read(&dir, f).unwrap());
            let index = ["a", "b", "c"].map(|f| sh(&dir, &["show", &format!(":{f}")]));
            match mode {
                "--soft" => assert!(kept.is_empty() && sh(&dir, &["status", "--porcelain"]) == "M  a\n M b\n?? u\n"),
                "--mixed" => {
                    assert_eq!(kept, ["a"]);
                    assert_eq!((files, index), (["staged\n", "edited\n", "2\n", "mine\n"].map(String::from), ["staged\n", "1\n", "1\n"].map(String::from)));
                }
                _ => {
                    assert_eq!(kept, ["a", "b", "u"]);
                    assert_eq!((files, index), (["staged\n", "edited\n", "1\n", "mine\n"].map(String::from), ["staged\n", "1\n", "1\n"].map(String::from)));
                }
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    // ---- backup lifetime (policy) ----

    /// 1: the backup ref exists before the op's first command runs. 6: a refused op leaves none.
    #[test]
    fn backup_is_durable_before_the_op_and_a_refused_op_leaves_none() {
        let dir = repo("undo-policy-durable");
        std::fs::write(dir.join("keep"), "edit\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        let probe = Op::new("Probe", &[super::super::tools::SHELL, "git for-each-ref refs/glasir/backups/ > .git/probe-refs.txt; git checkout -- keep"]).light().backup(Backup::Paths(vec!["keep".into()]));
        step(&ctx, &mut t, Action::Git(probe));
        let out = dir.join(".git/probe-refs.txt");
        // Windows PowerShell writes UTF-16.
        let refs: Vec<u8> = std::fs::read(&out).unwrap().into_iter().filter(|b| *b != 0).collect();
        assert_eq!(String::from_utf8_lossy(&refs).matches("refs/glasir/backups/").count(), 1);
        let _ = std::fs::remove_file(out);
        assert_eq!(backups(&dir), 2);
        // Refused up front (a name git can't store): no ref at all; refused by its guard: none either.
        if cfg!(not(windows)) {
            std::fs::write(dir.join("git~1"), "x").unwrap();
            let msgs = step(&ctx, &mut t, clean("git~1"));
            assert!(msgs.iter().any(|m| m.contains("Nothing was changed")), "{msgs:?}");
            std::fs::remove_file(dir.join("git~1")).unwrap();
        }
        std::fs::write(dir.join("keep"), "edit 2\n").unwrap();
        let mut stale = reset_op(t.data.as_ref().unwrap(), &rev(&dir, "HEAD"), "--hard");
        stale.confirm = None;
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "moved"]);
        step(&ctx, &mut t, Action::Git(stale));
        assert_eq!((backups(&dir), read(&dir, "keep")), (2, Some("edit 2\n".into())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 2: undo, then a new op: the backups go and nothing lands in the stash list.
    /// 3: undo, redo, undo: they live throughout; a new op then drops them.
    #[test]
    fn backups_live_while_their_step_can_come_back() {
        let dir = repo("undo-policy-life");
        let (ctx, mut t) = tab(&dir);
        std::fs::write(dir.join("keep"), "edit\n").unwrap();
        step(&ctx, &mut t, discard("keep"));
        assert_eq!(backups(&dir), 2);
        step(&ctx, &mut t, Action::Undo);
        step(&ctx, &mut t, branch("x"));
        assert_eq!((backups_settle(&dir, 0), sh(&dir, &["stash", "list"])), (0, String::new()));
        step(&ctx, &mut t, discard("keep"));
        for a in [Action::Undo, Action::Redo, Action::Undo] {
            step(&ctx, &mut t, a);
            assert_eq!(backups(&dir), 2);
        }
        assert_eq!(read(&dir, "keep").as_deref(), Some("edit\n"));
        step(&ctx, &mut t, branch("y"));
        assert_eq!(backups_settle(&dir, 0), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 4: a session that ended (its pid runs no more) leaves its backups; a later open prunes them
    /// after 7 days (not before). 5: pruning never touches stash entries, user refs or other Glasir refs.
    #[test]
    fn backups_of_lost_steps_are_pruned_after_a_week() {
        let dir = repo("undo-policy-prune");
        {
            let (ctx, mut t) = tab(&dir);
            std::fs::write(dir.join("keep"), "edit\n").unwrap();
            step(&ctx, &mut t, discard("keep"));
        }
        // As made by a Glasir that quit since.
        for l in sh(&dir, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/glasir/backups/"]).lines() {
            let (r, o) = l.split_once(' ').unwrap();
            sh(&dir, &["update-ref", &r.replace(&format!("-{}-", std::process::id()), &format!("-{}-", i32::MAX - 1)), o]);
            sh(&dir, &["update-ref", "-d", r]);
        }
        assert_eq!(backups(&dir), 2);
        std::fs::write(dir.join("keep"), "kept by an undo\n").unwrap();
        let c = sh(&dir, &["stash", "create"]).trim().to_owned();
        std::fs::write(dir.join("keep"), "user stash\n").unwrap();
        sh(&dir, &["stash", "push", "-q"]);
        sh(&dir, &["stash", "store", "-m", "Glasir: Undo x", &c]);
        sh(&dir, &["update-ref", "refs/glasir/pr/1", &c]);
        let others = || sh(&dir, &["for-each-ref", "--format=%(refname)"]).lines().filter(|r| !r.starts_with("refs/glasir/backups/")).map(String::from).collect::<Vec<_>>();
        let (before, stashes) = (others(), sh(&dir, &["stash", "list"]));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        git::prune(&dir, now + 6 * 86_400, 7);
        assert_eq!(backups(&dir), 2);
        git::prune(&dir, now + 8 * 86_400, 7);
        assert_eq!(backups(&dir), 0);
        assert!(stashes.contains("Glasir: Undo x") && stashes.lines().count() == 2, "{stashes}");
        assert_eq!((others(), sh(&dir, &["stash", "list"])), (before, stashes));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 7: a failed undo offers Retry and Skip; Skip keeps the step's files as exactly one stash
    /// entry, drops its backups and pops it: the steps below stay undoable.
    #[test]
    fn a_failed_undo_can_be_retried_or_skipped() {
        let dir = repo("undo-policy-skip");
        let wt = scratch("undo-policy-skip-wt");
        let _ = std::fs::remove_dir_all(&wt);
        sh(&dir, &["branch", "x"]);
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, branch("y"));
        std::fs::write(dir.join("keep"), "MINE\n").unwrap();
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "more"]);
        let op = Op::new("Move x and discard", &["branch", "-f", "x", "HEAD"]).then(&["checkout", "--", "keep"]).undo(Undo::Keep).backup(Backup::Paths(vec!["keep".into()]));
        step(&ctx, &mut t, Action::Git(op));
        sh(&dir, &["worktree", "add", "-q", wt.to_str().unwrap(), "x"]);
        step(&ctx, &mut t, Action::Undo);
        let c = t.choice.take().expect("offers a way out");
        assert!(c.text.contains("another worktree") && c.ops.len() == 2, "{}", c.text);
        assert_eq!(labels(&t).len(), 2);
        // Retry while still blocked: the same choice; Skip: one stash entry, the step gone.
        step(&ctx, &mut t, Action::Git(c.ops[0].1.clone()));
        let c = t.choice.take().expect("still blocked");
        let (_busy, rx) = std::sync::mpsc::channel();
        t.running = Some(("Fetch".into(), Task(rx)));
        for (op, what) in [(&c.ops[0].1, "Undo"), (&c.ops[1].1, "Skip")] {
            t.apply(&ctx, Action::Git(op.clone()));
            assert_eq!(t.notes.pop().map(|n| n.text), Some(format!("'{what} Move x and discard' skipped: 'Fetch' is still running.")));
        }
        t.running = None;
        t.apply(&ctx, Action::Git(c.ops[1].1.clone()));
        assert!(t.running.is_some());
        let msgs: Vec<String> = settle(&ctx, &mut t).into_iter().map(|m| m.text).collect();
        assert_eq!(sh(&dir, &["stash", "list"]).lines().count(), 1, "{msgs:?}");
        assert!(in_stash(&dir, "keep", "MINE\n"));
        assert_eq!((labels(&t), backups(&dir)), (vec!["Create branch y".to_owned()], 0));
        step(&ctx, &mut t, Action::Undo);
        assert!(git::run(&dir, &["rev-parse", "--verify", "y"]).is_err());
        sh(&dir, &["worktree", "remove", "--force", wt.to_str().unwrap()]);
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }


    #[test]
    fn undoing_a_commit_refuses_once_head_switched_branches() {
        let dir = repo("undo-switched");
        sh(&dir, &["branch", "y"]);
        let (ctx, mut t) = tab(&dir);
        std::fs::write(dir.join("f"), "f\n").unwrap();
        sh(&dir, &["add", "f"]);
        t.summary = "add f".into();
        let op = t.commit_op();
        step(&ctx, &mut t, Action::Git(op));
        let c = rev(&dir, "main");
        sh(&dir, &["checkout", "-q", "y"]);
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(rev(&dir, "main"), c, "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("outside Glasir")), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undoing_a_new_branch_refuses_while_it_is_checked_out() {
        let dir = repo("undo-new-branch-head");
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, branch("x"));
        sh(&dir, &["switch", "-q", "x"]);
        step(&ctx, &mut t, Action::Undo);
        let c = t.choice.take().expect("offers a way out");
        assert!(c.text.contains("x is checked out here"), "{}", c.text);
        assert_eq!((sh(&dir, &["symbolic-ref", "HEAD"]).trim(), read(&dir, "keep").as_deref()), ("refs/heads/x", Some("k\n")));
        assert!(git::run(&dir, &["rev-parse", "-q", "--verify", "refs/heads/x"]).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discard_is_restorable() {
        let dir = repo("undo-discard");
        commit(&dir, "a.txt", "one\n");
        std::fs::write(dir.join("a.txt"), "edited\n").unwrap();
        std::fs::create_dir_all(dir.join("new")).unwrap();
        std::fs::write(dir.join("new/b.txt"), "fresh\n").unwrap();
        let op = Op::new("Discard all", &[crate::repo::DISCARD_ALL]).backup(Backup::Tree { revs: vec![], untracked: true });
        let f = run_backed(&dir, &op).unwrap().unwrap();
        assert_eq!(read(&dir, "a.txt").as_deref(), Some("one\n"));
        assert!(!dir.join("new/b.txt").exists());
        undo_files(&dir, &f).unwrap();
        assert_eq!((read(&dir, "a.txt"), read(&dir, "new/b.txt")), (Some("edited\n".into()), Some("fresh\n".into())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_keeps_refs_made_elsewhere_while_the_op_ran() {
        let dir = repo("undo-elsewhere");
        let hooks = dir.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(hooks.join("pre-commit"), "#!/bin/sh\ntouch .git/started\nwhile [ ! -f .git/go ]; do sleep 0.05; done\n").unwrap();
        #[cfg(unix)]
        chmod(&dir, ".git/hooks/pre-commit", 0o755);
        std::fs::write(dir.join("keep"), "second\n").unwrap();
        sh(&dir, &["add", "keep"]);
        let (ctx, mut t) = tab(&dir);
        let first = rev(&dir, "HEAD");
        t.summary = "second".into();
        t.run(&ctx, t.commit_op());
        super::super::testutil::wait_for("the hook", || dir.join(".git/started").exists());
        sh(&dir, &["tag", "-a", "v1", "-m", "elsewhere"]);
        sh(&dir, &["branch", "foo"]);
        std::fs::write(dir.join(".git/go"), "").unwrap();
        settle(&ctx, &mut t);
        assert_ne!(rev(&dir, "HEAD"), first);
        step(&ctx, &mut t, Action::Undo);
        assert_eq!((rev(&dir, "HEAD"), rev(&dir, "foo"), rev(&dir, "v1^{commit}")), (first.clone(), first.clone(), first));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn own_keeps_refs_named_with_plus_or_equals_and_all_refs_of_custom_commands() {
        let dir = repo("undo-own");
        let before = git::snapshot(&dir).unwrap();
        for b in ["c++", "a=b", "x", "other"] {
            sh(&dir, &["branch", b]);
        }
        let after = git::snapshot(&dir).unwrap();
        let op = Op::new("Make", &["branch", "c++"]).then(&["branch", "a=b"]).then(&["push", "origin", "+x"]);
        let kept = own(&op, &before, after.clone()).refs.into_keys().collect::<Vec<_>>();
        assert_eq!(kept, ["refs/heads/a=b", "refs/heads/c++", "refs/heads/main", "refs/heads/x"]);
        let custom = Op { custom: true, ..Op::new("Tag today", &[super::super::tools::SHELL, "git branch \"rel-$(date +%F)\""]) };
        assert!(own(&custom, &before, after.clone()) == after);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tag fetched since an undo entry doesn't block undoing it, and stays.
    /// A rebase stopped on a conflict keeps its entry, but undo waits until it's finished.
    #[test]
    fn undo_survives_new_tags_and_waits_for_stopped_rebases() {
        let dir = repo("undo-tags");
        commit(&dir, "a.txt", "one\n");
        let before = git::snapshot(&dir).unwrap();
        commit(&dir, "a.txt", "two\n");
        let after = git::snapshot(&dir).unwrap();
        sh(&dir, &["update-ref", "refs/tags/v2", "HEAD"]);
        transition(&dir, "Undo", Some((after, before.clone(), Undo::Keep)), &[]).unwrap();
        assert!(sh(&dir, &["tag"]).contains("v2"));
        assert_eq!(git::snapshot(&dir).unwrap().refs.get("refs/heads/main"), before.refs.get("refs/heads/main"));

        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "a.txt", "feat\n");
        let feat_tip = sh(&dir, &["rev-parse", "feat"]);
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "a.txt", "main\n");
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, Action::Git(Op::new("Rebase feat onto main", &["rebase", "main", "feat"]).undo(Undo::Keep)));
        assert_eq!(t.data.as_ref().unwrap().state, git::State::Rebasing);
        assert_eq!(t.undo_label(), Some("Rebase feat onto main"));
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(t.data.as_ref().unwrap().state, git::State::Rebasing);
        assert_eq!(t.undo_label(), Some("Rebase feat onto main"));
        assert!(msgs.iter().any(|m| m.contains("Finish or abort")), "{msgs:?}");

        std::fs::write(dir.join("a.txt"), "both\n").unwrap();
        sh(&dir, &["add", "a.txt"]);
        step(&ctx, &mut t, Action::Git(Op::new("Continue rebase", &["-c", "core.editor=true", "rebase", "--continue"]).undo(Undo::Keep)));
        for _ in 0..2 {
            step(&ctx, &mut t, Action::Undo);
        }
        assert_eq!(t.redo_label(), Some("Rebase feat onto main"));
        assert_eq!(sh(&dir, &["rev-parse", "feat"]), feat_tip);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_step_at_the_same_depth_has_a_new_id() {
        let dir = repo("undo-top");
        let (ctx, mut t) = tab(&dir);
        let top = |t: &mut RepoTab, a: Action| {
            step(&ctx, t, a);
            t.undo_top()
        };
        let first = top(&mut t, branch("x"));
        assert!(first.is_some());
        assert_eq!(top(&mut t, Action::Undo), None);
        let second = top(&mut t, branch("y"));
        assert!(second.is_some() && second != first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo of a discard after the path changed again: the edit stays, the backup goes to the
    /// stash list (said once), the step is done and the ref step below stays undoable.
    #[test]
    fn undo_discard_keeps_newer_edits_and_stashes_the_backup() {
        let dir = repo("undo-newer");
        commit(&dir, "a", "base\n");
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, branch("x"));
        std::fs::write(dir.join("a"), "E1\n").unwrap();
        step(&ctx, &mut t, discard("a"));
        std::fs::write(dir.join("a"), "E2\n").unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, "a").as_deref(), Some("E2\n"));
        assert!(in_stash(&dir, "a", "E1\n"));
        assert_eq!(msgs.iter().filter(|m| m.contains("stash list")).count(), 1, "{msgs:?}");
        assert_eq!(t.undo_label(), Some("Create branch x"));

        std::fs::write(dir.join("notes.txt"), "A\n").unwrap();
        step(&ctx, &mut t, clean("notes.txt"));
        std::fs::write(dir.join("notes.txt"), "B\n").unwrap();
        step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, "notes.txt").as_deref(), Some("B\n"));
        assert!(in_stash(&dir, "notes.txt", "A\n"));
        step(&ctx, &mut t, Action::Undo);
        assert!(git::run(&dir, &["rev-parse", "--verify", "x"]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A ref undo over a local edit of a file it moves: the edit stays (said once), the steps
    /// below stay, a force-deleted branch stays restorable.
    #[test]
    fn ref_undo_keeps_local_edits_and_op_entries() {
        let dir = repo("undo-keep-op");
        commit(&dir, "a", "main\n");
        sh(&dir, &["checkout", "-q", "-b", "x"]);
        commit(&dir, "x", "only on x\n");
        let x = rev(&dir, "x");
        sh(&dir, &["checkout", "-q", "-b", "y", "main"]);
        commit(&dir, "a", "y\n");
        sh(&dir, &["checkout", "-q", "main"]);
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, Action::Git(Op::new("Delete x", &["branch", "-D", "x"]).revert(Op::new("Restore x", &["branch", "x", &x]))));
        step(&ctx, &mut t, Action::Git(Op::new("Checkout y", &["checkout", "-q", "y"]).undo(Undo::Keep)));
        std::fs::write(dir.join("a"), "local edit\n").unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(sh(&dir, &["symbolic-ref", "--short", "HEAD"]), "main\n");
        assert_eq!(read(&dir, "a").as_deref(), Some("local edit\n"));
        assert_eq!(msgs.iter().filter(|m| m.contains("Kept your changes to a")).count(), 1, "{msgs:?}");
        assert_eq!(t.undo_label(), Some("Delete x"));
        step(&ctx, &mut t, Action::Undo);
        assert_eq!(rev(&dir, "x"), x);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Thousands of long paths don't fit on a command line: they go through stdin.
    #[test]
    fn undo_discard_of_many_files() {
        let dir = repo("undo-many");
        let long = "d".repeat(190);
        let sub = dir.join(&long);
        std::fs::create_dir_all(&sub).unwrap();
        let write = |text: &str| (0..12000).for_each(|i| std::fs::write(sub.join(format!("f{i}")), text).unwrap());
        write("v1\n");
        sh(&dir, &["add", "-A"]);
        sh(&dir, &["commit", "-q", "-m", "init"]);
        write("v2\n");
        let f = run_backed(&dir, &Op::new("Discard all", &["reset", "--hard", "-q"]).backup(Backup::Tree { revs: vec![], untracked: true })).unwrap().unwrap();
        assert_eq!(sh(&dir, &["show", &format!("{}:{long}/f11999", f.before.commit), "--"]), "v2\n");
        assert_eq!(read(&dir, &format!("{long}/f11999")).as_deref(), Some("v1\n"));
        undo_files(&dir, &f).unwrap();
        assert_eq!(read(&dir, &format!("{long}/f11999")).as_deref(), Some("v2\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo replaces a symlink the restore created instead of writing through it.
    #[cfg(unix)]
    #[test]
    fn undo_restore_does_not_write_through_a_symlink() {
        let dir = repo("undo-symlink");
        commit(&dir, "real.env", "TRACKED=1\n");
        std::os::unix::fs::symlink("real.env", dir.join(".env")).unwrap();
        sh(&dir, &["add", ".env"]);
        sh(&dir, &["commit", "-q", "-m", "link"]);
        let c = rev(&dir, "HEAD");
        sh(&dir, &["rm", "-q", "--cached", ".env"]);
        commit(&dir, ".gitignore", ".env\n");
        std::fs::remove_file(dir.join(".env")).unwrap();
        std::fs::write(dir.join(".env"), "SECRET=mine\n").unwrap();
        let op = Op::new("Restore", &["restore", "--source", &c, "--worktree", "--", ".env"]).backup(Backup::Paths(vec![".env".into()]));
        let f = run_backed(&dir, &op).unwrap().unwrap();
        assert!(std::fs::symlink_metadata(dir.join(".env")).unwrap().is_symlink());
        undo_files(&dir, &f).unwrap();
        assert!(!std::fs::symlink_metadata(dir.join(".env")).unwrap().is_symlink());
        assert_eq!(read(&dir, ".env").as_deref(), Some("SECRET=mine\n"));
        assert_eq!(read(&dir, "real.env").as_deref(), Some("TRACKED=1\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo never moves or deletes a branch another worktree has checked out, rebases or
    /// bisects: it fails before anything moved, the step stays, and Retry works once it's free.
    #[test]
    fn undo_leaves_other_worktrees_branches_alone() {
        for how in ["checkout", "rebasing"] {
            let (dir, wt) = (repo(&format!("undo-wt-{how}")), scratch(&format!("undo-wt-{how}-other")));
            let _ = std::fs::remove_dir_all(&wt);
            commit(&dir, "a", "base\n");
            sh(&dir, &["branch", "feat"]);
            commit(&dir, "a", "main\n");
            let (ctx, mut t) = tab(&dir);
            step(&ctx, &mut t, Action::Git(Op::new("Move feat", &["branch", "-f", "feat", "main"]).undo(Undo::Keep)));
            sh(&dir, &["worktree", "add", "-q", wt.to_str().unwrap(), "feat"]);
            // Stopped there: the branch shows as detached in that worktree.
            if how == "rebasing" {
                assert!(git::run(&wt, &["rebase", "-q", "--exec", "false", "HEAD~1"]).is_err());
            }
            let feat = rev(&dir, "feat");
            step(&ctx, &mut t, Action::Undo);
            let c = t.choice.take().expect("refused");
            assert!(c.text.contains("another worktree"), "{how}: {}", c.text);
            assert_eq!((rev(&dir, "feat"), t.undo_label()), (feat, Some("Move feat")));
            assert_eq!(sh(&wt, &["status", "--porcelain"]), "");
            let _ = git::run(&wt, &["rebase", "--abort"]);
            sh(&dir, &["worktree", "remove", "--force", wt.to_str().unwrap()]);
            step(&ctx, &mut t, Action::Git(c.ops[0].1.clone()));
            assert_ne!(rev(&dir, "feat"), rev(&dir, "main"), "{how}: the retry goes through");
            let _ = std::fs::remove_dir_all(&wt);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A name that isn't valid UTF-8 comes back from git with U+FFFD: refuse to discard it.
    #[test]
    fn discard_refuses_undecodable_names() {
        let dir = repo("undo-fffd");
        std::fs::write(dir.join("caf\u{FFFD}.txt"), "x\n").unwrap();
        let op = Op::new("Discard all", &["clean", "-fdq"]).backup(Backup::Tree { revs: vec![], untracked: true });
        assert!(run_backed(&dir, &op).is_err_and(|e| e.contains("Nothing was changed")));
        assert!(dir.join("caf\u{FFFD}.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo of "Discard all" brings back the staged changes and the exact bytes.
    #[test]
    fn undo_discard_all_restores_index_and_line_endings() {
        let dir = repo("undo-index");
        sh(&dir, &["config", "core.autocrlf", "true"]);
        commit(&dir, "b", "one\n");
        commit(&dir, "c", "c0\n");
        std::fs::write(dir.join("b"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("c"), "c1\n").unwrap();
        sh(&dir, &["add", "c"]);
        std::fs::write(dir.join("c"), "c1\nc2\n").unwrap();
        std::fs::write(dir.join("new"), "n\n").unwrap();
        sh(&dir, &["add", "new"]);
        let before = sh(&dir, &["status", "--porcelain"]);
        let op = Op::new("Discard all", &[crate::repo::DISCARD_ALL]).backup(Backup::Tree { revs: vec![], untracked: true });
        let f = run_backed(&dir, &op).unwrap().unwrap();
        undo_files(&dir, &f).unwrap();
        assert_eq!(std::fs::read(dir.join("b")).unwrap(), b"one\ntwo\n");
        assert_eq!(sh(&dir, &["status", "--porcelain"]), before);
        assert_eq!(sh(&dir, &["show", ":c"]).replace('\r', ""), "c1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_discard_restores_the_bytes_past_info_attributes() {
        let dir = repo("undo-raw-attr");
        std::fs::create_dir_all(dir.join(".git/info")).unwrap();
        std::fs::write(dir.join(".git/info/attributes"), "*.txt text eol=crlf working-tree-encoding=UTF-16LE\n").unwrap();
        let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
        std::fs::write(dir.join("u.txt"), utf16("h\r\n")).unwrap();
        sh(&dir, &["add", "u.txt"]);
        sh(&dir, &["commit", "-q", "-m", "u"]);
        std::fs::write(dir.join("u.txt"), utf16("hi\r\n")).unwrap();
        let f = run_backed(&dir, &Op::new("Discard", &["checkout", "--", "u.txt"]).backup(Backup::Paths(vec!["u.txt".into()]))).unwrap().unwrap();
        assert_eq!(std::fs::read(dir.join("u.txt")).unwrap(), utf16("h\r\n"));
        undo_files(&dir, &f).unwrap();
        assert_eq!(std::fs::read(dir.join("u.txt")).unwrap(), utf16("hi\r\n"));
        assert_eq!(sh(&dir, &["status", "--porcelain"]), " M u.txt\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ⌘Z on a Discard all after the file was edited again: the edit stays, the backup is kept.
    #[test]
    fn undo_of_a_discard_all_keeps_edits_made_since() {
        let dir = repo("undo-discard-newer");
        commit(&dir, "f", "base\n");
        std::fs::write(dir.join("f"), "first edit\n").unwrap();
        std::fs::write(dir.join("g"), "untracked\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, discard_all());
        std::fs::write(dir.join("f"), "second edit\n").unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, "f").as_deref(), Some("second edit\n"));
        assert_eq!(read(&dir, "g").as_deref(), Some("untracked\n"), "{msgs:?}");
        assert!(in_stash(&dir, "f", "first edit\n") && t.undo_label().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Changes that exist only in the index ("MM" back to HEAD, "AD") come back too.
    #[test]
    fn undo_discard_all_restores_index_only_changes() {
        let dir = repo("undo-index-only");
        commit(&dir, "f", "v1\n");
        std::fs::write(dir.join("f"), "v2\n").unwrap();
        std::fs::write(dir.join("n"), "new\n").unwrap();
        sh(&dir, &["add", "f", "n"]);
        std::fs::write(dir.join("f"), "v1\n").unwrap();
        std::fs::remove_file(dir.join("n")).unwrap();
        let before = sh(&dir, &["status", "--porcelain"]);
        let f = run_backed(&dir, &Op::new("Discard all", &["reset", "--hard", "-q"]).backup(Backup::Tree { revs: vec![], untracked: true })).unwrap().unwrap();
        undo_files(&dir, &f).unwrap();
        assert_eq!(sh(&dir, &["status", "--porcelain"]), before);
        assert_eq!(sh(&dir, &["show", ":f"]), "v2\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An undo that fails partway (a folder it writes into is read-only) leaves the refs as they
    /// were; a retry finishes it, whatever it had already written.
    #[cfg(unix)]
    #[test]
    fn undo_retries_after_a_partial_failure() {
        let dir = repo("undo-half");
        std::fs::create_dir(dir.join("z")).unwrap();
        commit(&dir, "z/b", "b0\n");
        commit(&dir, "c", "c0\n");
        let c0 = rev(&dir, "HEAD");
        commit(&dir, "c", "c1\n");
        let c1 = rev(&dir, "HEAD");
        std::fs::write(dir.join("c"), "MINE-C\n").unwrap();
        std::fs::write(dir.join("z/b"), "MINE-B\n").unwrap();
        std::fs::write(dir.join("z/new"), "MINE-NEW\n").unwrap();
        sh(&dir, &["add", "z/new"]);
        let (ctx, mut t) = tab(&dir);
        hard_reset(&ctx, &mut t, &c0);
        chmod(&dir, "z", 0o555);
        step(&ctx, &mut t, Action::Undo);
        chmod(&dir, "z", 0o755);
        let c = t.choice.take().expect("a failure offers retry");
        assert_eq!(rev(&dir, "HEAD"), c0);
        let msgs = step(&ctx, &mut t, Action::Git(c.ops[0].1.clone()));
        assert_eq!(rev(&dir, "HEAD"), c1, "{msgs:?}");
        assert_eq!(["c", "z/b", "z/new"].map(|p| read(&dir, p).unwrap()), ["MINE-C\n", "MINE-B\n", "MINE-NEW\n"].map(String::from), "{msgs:?}");
        assert_eq!(sh(&dir, &["diff", "--cached", "--name-only"]), "z/new\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ⌘Z after Commit & push asks first: the commit is on the remote already. Undoing a
    /// pull (its commits were on the remote before it ran) doesn't.
    #[test]
    fn undo_of_a_pushed_commit_asks_first() {
        let root = scratch("undo-pushed");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        sh(&root, &["clone", "-q", "remote.git", "a"]);
        let a = root.join("a");
        identity(&a);
        commit(&a, "f", "one\n");
        let one = sh(&a, &["rev-parse", "HEAD"]);
        commit(&a, "f", "two\n");
        sh(&a, &["push", "-q", "-u", "origin", "main"]);
        let two = sh(&a, &["rev-parse", "HEAD"]);
        sh(&a, &["reset", "-q", "--hard", "HEAD~1"]);
        let (ctx, mut t) = tab(&a);
        let pull = || Action::Git(Op::new("Pull", &["merge", "-q", "--ff-only", "@{u}"]).undo(Undo::Keep));
        step(&ctx, &mut t, pull());
        t.apply(&ctx, Action::Undo);
        assert!(t.confirm.is_none());
        settle(&ctx, &mut t);
        assert_eq!(sh(&a, &["rev-parse", "HEAD"]), one);
        step(&ctx, &mut t, pull());

        std::fs::write(a.join("f"), "three\n").unwrap();
        sh(&a, &["add", "f"]);
        (t.summary, t.push_after) = ("three".into(), true);
        let op = t.commit_op();
        step(&ctx, &mut t, Action::Git(op));
        let three = sh(&a, &["rev-parse", "HEAD"]);
        assert_eq!(sh(&root.join("remote.git"), &["rev-parse", "main"]), three);
        t.apply(&ctx, Action::Undo);
        let ask = t.confirm.take().expect("asks before undoing a pushed commit");
        assert!(ask.confirm.as_deref().unwrap().contains("force push"), "{:?}", ask.confirm);
        settle(&ctx, &mut t);
        assert_eq!(sh(&a, &["rev-parse", "HEAD"]), three);
        step(&ctx, &mut t, Action::Git(Op { confirm: None, ..ask }));
        assert_eq!(sh(&a, &["rev-parse", "HEAD"]), two);
        assert_eq!(sh(&a, &["diff", "--cached", "--name-only"]).trim(), "f");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// On a case-insensitive disk, an untracked `Readme` is in the way of a tracked
    /// `README`: the hard reset backs it up and ⌘Z brings it back.
    #[test]
    fn hard_reset_backs_up_a_file_in_the_way_in_another_case() {
        let dir = scratch("undo-case");
        std::fs::write(dir.join("CaseProbe"), "").unwrap();
        if !dir.join("caseprobe").exists() {
            return; // A case-sensitive disk keeps both files.
        }
        std::fs::remove_file(dir.join("CaseProbe")).unwrap();
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "x", "x\n");
        commit(&dir, "README", "theirs\n");
        let x = rev(&dir, "HEAD");
        sh(&dir, &["rm", "-q", "README"]);
        sh(&dir, &["commit", "-q", "-m", "rm readme"]);
        std::fs::write(dir.join("Readme"), "MY NOTES\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        hard_reset(&ctx, &mut t, &x);
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, "Readme").as_deref(), Some("MY NOTES\n"), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A hard reset's backup kept in the stash list (the file was edited since)
    /// keeps the staged version too, in the entry's index (`^2`). Also on a detached HEAD.
    #[test]
    fn kept_hard_reset_backup_keeps_the_staged_version() {
        for detached in [false, true] {
            let dir = repo(if detached { "undo-kept-index-d" } else { "undo-kept-index" });
            commit(&dir, "a", "base\n");
            commit(&dir, "b", "b1\n");
            let x = rev(&dir, "HEAD");
            commit(&dir, "b", "b2\n");
            if detached {
                sh(&dir, &["checkout", "-q", "--detach"]);
            }
            std::fs::write(dir.join("a"), "STAGED-ONLY\n").unwrap();
            sh(&dir, &["add", "a"]);
            std::fs::write(dir.join("a"), "WORKTREE\n").unwrap();
            let (ctx, mut t) = tab(&dir);
            let msgs = hard_reset(&ctx, &mut t, &x);
            assert_eq!(read(&dir, "a").as_deref(), Some("base\n"), "{msgs:?}");
            std::fs::write(dir.join("a"), "NEW EDIT\n").unwrap();
            let msgs = step(&ctx, &mut t, Action::Undo);
            assert_eq!(read(&dir, "a").as_deref(), Some("NEW EDIT\n"));
            assert!(msgs.iter().any(|m| m.contains("stash")), "{msgs:?}");
            assert_eq!(sh(&dir, &["show", "stash@{0}^2:a"]), "STAGED-ONLY\n");
            assert_eq!(sh(&dir, &["show", "stash@{0}:a"]), "WORKTREE\n");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// An Op step's undo that git refuses before changing anything (a stash pop
    /// over a local edit) keeps every step below it undoable.
    #[test]
    fn refused_op_undo_keeps_the_ref_steps() {
        let dir = repo("undo-op-refused");
        commit(&dir, "a", "a0\n");
        let c0 = rev(&dir, "HEAD");
        commit(&dir, "a", "a1\n");
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, branch("x"));
        std::fs::write(dir.join("a"), "MINE\n").unwrap();
        hard_reset(&ctx, &mut t, &c0);
        std::fs::write(dir.join("a"), "STASHED\n").unwrap();
        step(&ctx, &mut t, Action::Git(Op::new("Stash", &["stash", "push", "--include-untracked", "-m", "s"])));
        let before = labels(&t);
        assert_eq!(before.len(), 3);
        std::fs::write(dir.join("a"), "LATER EDIT\n").unwrap();
        step(&ctx, &mut t, Action::Undo);
        assert!(t.choice.take().is_some_and(|c| c.text.contains("would be overwritten")));
        assert_eq!(labels(&t), before);
        assert!(t.undo[1].refs.is_some());
        sh(&dir, &["checkout", "--", "a"]);
        step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, "a").as_deref(), Some("STASHED\n"));
        sh(&dir, &["stash", "push", "-q"]);
        for _ in 0..2 {
            step(&ctx, &mut t, Action::Undo);
        }
        assert_eq!(t.undo_label(), None);
        assert!(git::run(&dir, &["rev-parse", "--verify", "x"]).is_err());
        assert_eq!(read(&dir, "a").as_deref(), Some("MINE\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A staged new `d/x` discarded (Discard all, or the file's own discard), then a file
    /// `d` made where its folder was: ⌘Z keeps that file, and the backup of `d/x` goes to the
    /// stash list; the step is done.
    #[test]
    fn undo_discard_keeps_a_file_where_a_folder_was() {
        for all in [true, false] {
            let dir = repo(if all { "undo-dir-file-all" } else { "undo-dir-file-one" });
            std::fs::create_dir(dir.join("d")).unwrap();
            std::fs::write(dir.join("d/x"), "STAGED NEW\n").unwrap();
            sh(&dir, &["add", "d/x"]);
            let (ctx, mut t) = tab(&dir);
            let op = match all {
                true => Op::new("Discard all", &[crate::repo::DISCARD_ALL]).backup(Backup::Tree { revs: vec![], untracked: true }),
                false => Op::new("Discard", &["--literal-pathspecs", "rm", "-fq", "--", "d/x"]).light().backup(Backup::Paths(vec!["d/x".into()])),
            };
            step(&ctx, &mut t, Action::Git(op));
            assert!(!dir.join("d").exists());
            std::fs::write(dir.join("d"), "NEW WORK\n").unwrap();
            let msgs = step(&ctx, &mut t, Action::Undo);
            assert_eq!(read(&dir, "d").as_deref(), Some("NEW WORK\n"), "{msgs:?}");
            assert!(in_stash(&dir, "d/x", "STAGED NEW\n"), "{msgs:?}");
            assert_eq!(t.undo_label(), None);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// After a hard reset, a backed-up file's folder replaced by a file: ⌘Z keeps that
    /// file and the backup (in the stash list), said once.
    #[test]
    fn hard_reset_undo_keeps_a_file_where_a_folder_was() {
        let dir = repo("undo-reset-dir-file");
        std::fs::create_dir(dir.join("d")).unwrap();
        commit(&dir, "d/x", "base\n");
        let c0 = rev(&dir, "HEAD");
        commit(&dir, "y", "y\n");
        let c1 = rev(&dir, "HEAD");
        std::fs::write(dir.join("d/x"), "MINE\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        hard_reset(&ctx, &mut t, &c0);
        std::fs::remove_dir_all(dir.join("d")).unwrap();
        std::fs::write(dir.join("d"), "NEW WORK\n").unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(rev(&dir, "HEAD"), c1, "{msgs:?}");
        assert_eq!(read(&dir, "d").as_deref(), Some("NEW WORK\n"), "{msgs:?}");
        assert!(in_stash(&dir, "d/x", "MINE\n"), "{msgs:?}");
        assert_eq!(msgs.iter().filter(|m| m.contains("stash")).count(), 1, "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An untracked file with a name git won't index (`git~1`) refuses its discard, so
    /// a restore that fails partway can't lose it.
    #[cfg(not(windows))] // there `git~1` is .git's 8.3 short name: no such file can exist
    #[test]
    fn discard_refuses_a_name_git_cannot_store() {
        let dir = repo("undo-unstorable");
        std::fs::create_dir(dir.join("a")).unwrap();
        std::fs::write(dir.join("a/b.txt"), "AB\n").unwrap();
        std::fs::write(dir.join("git~1"), "SECRET\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        let paths = vec!["a/b.txt".to_owned(), "git~1".to_owned()];
        let msgs = step(&ctx, &mut t, Action::Git(Op::new("Discard 2 files", &["--literal-pathspecs", "clean", "-fq", "--", "a/b.txt", "git~1"]).light().backup(Backup::Paths(paths))));
        assert!(msgs.iter().any(|m| m.contains("Nothing was changed")), "{msgs:?}");
        assert_eq!((read(&dir, "git~1"), read(&dir, "a/b.txt")), (Some("SECRET\n".into()), Some("AB\n".into())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A branch restore that finds a branch already back at its commit skips it, and a
    /// failed one keeps every step: ⌘Z never sticks or wipes the steps below.
    #[test]
    fn branch_restore_never_sticks() {
        for same in [true, false] {
            let dir = repo(if same { "undo-branch-same" } else { "undo-branch-other" });
            sh(&dir, &["branch", "a"]);
            sh(&dir, &["branch", "b"]);
            let (ctx, mut t) = tab(&dir);
            step(&ctx, &mut t, branch("x"));
            let op = crate::repo::delete_branches(t.data.as_ref().unwrap(), "Delete a, b".into(), &["a", "b"], true);
            step(&ctx, &mut t, Action::Git(op));
            if same {
                sh(&dir, &["branch", "b"]);
            } else {
                commit(&dir, "other", "o\n");
                sh(&dir, &["branch", "b"]);
                sh(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
                let msgs = step(&ctx, &mut t, Action::Undo);
                assert_eq!(labels(&t), ["Create branch x", "Delete a, b"], "{msgs:?}");
                assert!(t.choice.take().is_some());
                sh(&dir, &["branch", "-D", "b"]);
            }
            let msgs = step(&ctx, &mut t, Action::Undo);
            assert_eq!(labels(&t), ["Create branch x"], "{msgs:?}");
            step(&ctx, &mut t, Action::Undo);
            assert!(t.undo.is_empty());
            assert!(git::run(&dir, &["rev-parse", "--verify", "x"]).is_err());
            assert_eq!(sh(&dir, &["branch", "--format=%(refname:short)"]), "a\nb\nmain\n");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }


    /// Exec bits survive a discard's undo (untracked, tracked mode-only)
    /// and a hard reset's (an untracked executable in its way).
    #[cfg(unix)]
    #[test]
    fn exec_bits_survive_undo() {
        let dir = repo("undo-exec");
        std::fs::write(dir.join("run.sh"), "#!/bin/sh\n").unwrap();
        chmod(&dir, "run.sh", 0o755);
        commit(&dir, "s", "echo\n");
        chmod(&dir, "s", 0o755);
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, clean("run.sh"));
        step(&ctx, &mut t, discard("s"));
        assert!(!dir.join("run.sh").exists() && mode(&dir, "s") == 0o644);
        step(&ctx, &mut t, Action::Undo);
        step(&ctx, &mut t, Action::Undo);
        assert_eq!((mode(&dir, "run.sh"), mode(&dir, "s")), (0o755, 0o755));

        commit(&dir, "tool", "tracked tool\n");
        let c1 = rev(&dir, "HEAD");
        sh(&dir, &["rm", "-q", "tool"]);
        sh(&dir, &["commit", "-q", "-m", "rm"]);
        std::fs::write(dir.join("tool"), "#!/bin/sh\nmine\n").unwrap();
        chmod(&dir, "tool", 0o755);
        step(&ctx, &mut t, Action::Reload);
        hard_reset(&ctx, &mut t, &c1);
        step(&ctx, &mut t, Action::Undo);
        assert_eq!((read(&dir, "tool").as_deref(), mode(&dir, "tool")), (Some("#!/bin/sh\nmine\n"), 0o755));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A chmod since a discard is a change: its undo keeps the file and stashes the backup.
    #[cfg(unix)]
    #[test]
    fn a_chmod_since_a_discard_is_kept() {
        let dir = repo("undo-chmod-since");
        commit(&dir, "s", "base\n");
        std::fs::write(dir.join("s"), "E1\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, discard("s"));
        chmod(&dir, "s", 0o755);
        step(&ctx, &mut t, Action::Undo);
        assert_eq!((read(&dir, "s").as_deref(), mode(&dir, "s")), (Some("base\n"), 0o755));
        assert!(in_stash(&dir, "s", "E1\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tracked discard edited again, then the edit moved aside: ⌘Z
    /// never refuses forever; the discarded edit is put back or kept in the stash list.
    #[test]
    fn p2_discard_undo_never_refuses_after_a_move_aside() {
        let dir = repo("undo-p2");
        commit(&dir, "a", "base\n");
        std::fs::write(dir.join("a"), "E1\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, branch("x"));
        step(&ctx, &mut t, discard("a"));
        std::fs::write(dir.join("a"), "E2\n").unwrap();
        std::fs::rename(dir.join("a"), dir.join("a.aside")).unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert!(safe(&dir, "a", "E1\n"), "{msgs:?}");
        assert_eq!(read(&dir, "a.aside").as_deref(), Some("E2\n"));
        assert_eq!(labels(&t), ["Create branch x"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undoing a pull whose changes the user edited since never wipes the
    /// undo history (no autostash pop that conflicts: the edit stays as it is).
    #[test]
    fn p3_pull_undo_over_an_edit_keeps_history() {
        let root = scratch("undo-p3");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        sh(&root, &["clone", "-q", "remote.git", "a"]);
        let a = root.join("a");
        identity(&a);
        commit(&a, "f", "1\n2\n3\n");
        sh(&a, &["push", "-q", "-u", "origin", "main"]);
        commit(&a, "f", "1\nTWO\n3\n");
        sh(&a, &["push", "-q", "origin", "main"]);
        sh(&a, &["reset", "-q", "--hard", "HEAD~1"]);
        let base = rev(&a, "HEAD");
        let (ctx, mut t) = tab(&a);
        step(&ctx, &mut t, branch("x"));
        step(&ctx, &mut t, Action::Git(super::super::pull_op(super::super::PullMode::Merge)));
        std::fs::write(a.join("f"), "1\nTWO-edited\n3\n").unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(rev(&a, "HEAD"), base, "{msgs:?}");
        assert_eq!(read(&a, "f").as_deref(), Some("1\nTWO-edited\n3\n"));
        assert_eq!(labels(&t), ["Create branch x"], "{msgs:?}");
        assert_eq!(t.redo_label(), Some("Pull"));
        step(&ctx, &mut t, Action::Undo);
        assert!(git::run(&a, &["rev-parse", "--verify", "x"]).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A soft undo never looks at a big ignored file the target tracks.
    #[test]
    fn p4_soft_undo_ignores_a_big_ignored_file() {
        let dir = repo("undo-p4");
        commit(&dir, "big.bin", "small at first\n");
        let (ctx, mut t) = tab(&dir);
        sh(&dir, &["rm", "-q", "--cached", "big.bin"]);
        std::fs::write(dir.join(".gitignore"), "big.bin\n").unwrap();
        sh(&dir, &["add", ".gitignore"]);
        step(&ctx, &mut t, branch("x"));
        t.summary = "stop tracking".into();
        let op = t.commit_op();
        step(&ctx, &mut t, Action::Git(op));
        std::fs::OpenOptions::new().write(true).open(dir.join("big.bin")).unwrap().set_len(300 << 20).unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(labels(&t), ["Create branch x"], "{msgs:?}");
        assert_eq!(std::fs::metadata(dir.join("big.bin")).unwrap().len(), 300 << 20);
        step(&ctx, &mut t, Action::Undo);
        assert!(t.undo.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A hard reset's undo that fails partway (a folder it
    /// writes into is read-only) never sticks and never holds the backup only in memory: a
    /// retry finishes it, and nothing is lost.
    #[cfg(unix)]
    #[test]
    fn p5_partial_hard_reset_undo_retries() {
        for discard_first in [false, true] {
            let dir = repo(if discard_first { "undo-p5b" } else { "undo-p5" });
            commit(&dir, "a", "a0\n");
            let c0 = rev(&dir, "HEAD");
            std::fs::create_dir(dir.join("d")).unwrap();
            commit(&dir, "d/x", "x1\n");
            commit(&dir, "a", "a1\n");
            std::fs::write(dir.join("a"), "MINE-A\n").unwrap();
            std::fs::write(dir.join("d/x"), "MINE-X\n").unwrap();
            std::fs::write(dir.join("d/new"), "MINE-NEW\n").unwrap();
            sh(&dir, &["add", "d/new"]);
            let (ctx, mut t) = tab(&dir);
            let c1 = rev(&dir, "HEAD");
            hard_reset(&ctx, &mut t, &c0);
            std::fs::create_dir(dir.join("d")).unwrap();
            chmod(&dir, "d", 0o555);
            step(&ctx, &mut t, Action::Undo);
            chmod(&dir, "d", 0o755);
            let c = t.choice.take().expect("a way out");
            assert!(backups(&dir) >= 2, "the backups are durable");
            if discard_first {
                // The in-app route: whatever the half-done undo wrote, discarding it is a new step.
                step(&ctx, &mut t, Action::Reload);
                if t.data.as_ref().unwrap().status.unstaged.iter().any(|c| c.path == "a") {
                    step(&ctx, &mut t, discard("a"));
                    step(&ctx, &mut t, Action::Undo);
                }
            }
            let msgs = step(&ctx, &mut t, Action::Git(c.ops[0].1.clone()));
            for (p, want) in [("a", "MINE-A\n"), ("d/x", "MINE-X\n"), ("d/new", "MINE-NEW\n")] {
                assert!(safe(&dir, p, want), "{p} {msgs:?}");
            }
            assert!(t.undo.is_empty() && t.choice.is_none(), "{msgs:?}");
            assert_eq!(rev(&dir, "HEAD"), c1, "{msgs:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A long chain of branch ops, discards and a hard reset undoes
    /// all the way down; every discarded edit ends up on disk or in the stash list.
    #[test]
    fn p6_long_chain_undoes_to_the_bottom() {
        let dir = repo("undo-p6");
        commit(&dir, "a", "a0\n");
        let c0 = rev(&dir, "HEAD");
        commit(&dir, "b", "b1\n");
        sh(&dir, &["branch", "y"]);
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, branch("x"));
        std::fs::write(dir.join("a"), "A-EDIT1\n").unwrap();
        step(&ctx, &mut t, discard("a"));
        let op = crate::repo::delete_branches(t.data.as_ref().unwrap(), "Delete y".into(), &["y"], true);
        step(&ctx, &mut t, Action::Git(op));
        std::fs::write(dir.join("b"), "B-DIRTY\n").unwrap();
        std::fs::write(dir.join("u"), "UNTRACKED\n").unwrap();
        hard_reset(&ctx, &mut t, &c0);
        step(&ctx, &mut t, branch("z"));
        std::fs::write(dir.join("a"), "A-EDIT2\n").unwrap();
        step(&ctx, &mut t, discard("a"));
        std::fs::write(dir.join("c"), "c\n").unwrap();
        sh(&dir, &["add", "c"]);
        t.summary = "add c".into();
        let op = t.commit_op();
        step(&ctx, &mut t, Action::Git(op));
        assert_eq!(t.undo.len(), 7);
        for _ in 0..7 {
            let msgs = step(&ctx, &mut t, Action::Undo);
            assert!(t.choice.is_none(), "{msgs:?}");
        }
        assert!(t.undo.is_empty());
        for (p, want) in [("a", "A-EDIT1\n"), ("a", "A-EDIT2\n"), ("b", "B-DIRTY\n"), ("u", "UNTRACKED\n"), ("c", "c\n")] {
            assert!(safe(&dir, p, want), "{p}={want}");
        }
        assert_eq!(sh(&dir, &["branch", "--format=%(refname:short)"]), "main\ny\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A confirm for undoing a pushed commit, accepted after a newer step, does nothing.
    #[test]
    fn a_stale_pushed_undo_confirm_does_nothing() {
        let root = scratch("undo-p9");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        sh(&root, &["clone", "-q", "remote.git", "a"]);
        let a = root.join("a");
        identity(&a);
        commit(&a, "f", "one\n");
        sh(&a, &["push", "-q", "-u", "origin", "main"]);
        let (ctx, mut t) = tab(&a);
        std::fs::write(a.join("f"), "two\n").unwrap();
        sh(&a, &["add", "f"]);
        (t.summary, t.push_after) = ("two".into(), true);
        let op = t.commit_op();
        step(&ctx, &mut t, Action::Git(op));
        let two = rev(&a, "HEAD");
        t.apply(&ctx, Action::Undo);
        let ask = t.confirm.take().expect("asks");
        step(&ctx, &mut t, branch("x"));
        step(&ctx, &mut t, Action::Git(Op { confirm: None, ..ask }));
        assert_eq!(rev(&a, "HEAD"), two);
        assert!(git::run(&a, &["rev-parse", "--verify", "x"]).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Discard all, then a file where an untracked file's folder was: ⌘Z keeps that
    /// file and the untracked file's backup.
    #[test]
    fn probe_g_discard_all_undo_blocked_by_a_file() {
        let dir = repo("undo-probe-g");
        std::fs::create_dir_all(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/n"), "UNTRACKED WORK\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, branch("x"));
        step(&ctx, &mut t, discard_all());
        std::fs::write(dir.join("d"), "a file now\n").unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, "d").as_deref(), Some("a file now\n"));
        assert!(in_stash(&dir, "d/n", "UNTRACKED WORK\n"), "{msgs:?}");
        assert_eq!(labels(&t), ["Create branch x"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Hard resets, undone: an untracked file the target tracks comes
    /// back; on a detached HEAD too; a file made where the undo needs a folder is kept.
    #[test]
    fn hard_reset_undo_cases() {
        let dir = repo("undo-probe-h");
        commit(&dir, "a", "a0\n");
        let base = rev(&dir, "HEAD");
        commit(&dir, "u", "tracked u\n");
        let x = rev(&dir, "HEAD");
        sh(&dir, &["reset", "-q", "--hard", &base]);
        std::fs::write(dir.join("u"), "MY UNTRACKED U\n").unwrap();
        std::fs::write(dir.join("a"), "MINE\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        hard_reset(&ctx, &mut t, &x);
        step(&ctx, &mut t, Action::Undo);
        assert_eq!((read(&dir, "u"), read(&dir, "a")), (Some("MY UNTRACKED U\n".into()), Some("MINE\n".into())));
        let _ = std::fs::remove_dir_all(&dir);

        let dir = repo("undo-probe-j");
        commit(&dir, "a", "a0\n");
        let c0 = rev(&dir, "HEAD");
        commit(&dir, "a", "a1\n");
        sh(&dir, &["checkout", "-q", "--detach"]);
        std::fs::write(dir.join("a"), "MINE\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        hard_reset(&ctx, &mut t, &c0);
        step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, "a").as_deref(), Some("MINE\n"));
        let _ = std::fs::remove_dir_all(&dir);

        let dir = repo("undo-p11");
        commit(&dir, "a", "a0\n");
        let c0 = rev(&dir, "HEAD");
        std::fs::create_dir(dir.join("d")).unwrap();
        commit(&dir, "d/x", "x1\n");
        commit(&dir, "a", "a1\n");
        std::fs::write(dir.join("d/x"), "MINE-X\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        hard_reset(&ctx, &mut t, &c0);
        std::fs::write(dir.join("d"), "NEW FILE D\n").unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert!(safe(&dir, "d", "NEW FILE D\n") && safe(&dir, "d/x", "MINE-X\n"), "{msgs:?}");
        assert!(t.undo.is_empty() && t.choice.is_none(), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hard_reset_undo_keeps_an_edit_that_matches_the_target() {
        let dir = repo("undo-like-target");
        commit(&dir, "f", "1\n");
        let c1 = rev(&dir, "HEAD");
        commit(&dir, "f", "2\n");
        std::fs::write(dir.join("f"), "1\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        hard_reset(&ctx, &mut t, &c1);
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, "f").as_deref(), Some("1\n"), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Rebase Skip discards a resolution and an untracked file, stops on a later
    /// conflict; Abort; ⌘Z all the way: nothing lost, the stack empties.
    #[test]
    fn probe_k_skip_then_conflict_then_abort() {
        let dir = repo("undo-probe-k");
        commit(&dir, "f", "base\n");
        commit(&dir, "g", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "f", "feat1\n");
        commit(&dir, "g", "feat2\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "f", "main1\n");
        commit(&dir, "g", "main2\n");
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, Action::Git(Op::new("Rebase feat onto main", &["rebase", "main", "feat"]).undo(Undo::Keep)));
        std::fs::write(dir.join("f"), "MY CAREFUL RESOLUTION\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "UNRELATED UNTRACKED\n").unwrap();
        let mut op = super::super::sequencer_op("rebase", false);
        op.confirm = None;
        step(&ctx, &mut t, Action::Git(op));
        assert!(safe(&dir, "notes.txt", "UNRELATED UNTRACKED\n"));
        let mut op = super::super::sequencer_op("rebase", true);
        op.confirm = None;
        step(&ctx, &mut t, Action::Git(op));
        for _ in 0..4 {
            step(&ctx, &mut t, Action::Undo);
        }
        assert!(t.undo.is_empty() && t.choice.is_none(), "{:?}", labels(&t));
        assert!(safe(&dir, "f", "MY CAREFUL RESOLUTION\n"));
        assert!(safe(&dir, "notes.txt", "UNRELATED UNTRACKED\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An untracked `d/f` where the reset target has a file `d`.
    #[test]
    fn hard_reset_over_an_untracked_folder_undoes() {
        let dir = repo("undo-reset-dirfile");
        commit(&dir, "d", "1");
        sh(&dir, &["rm", "-q", "d"]);
        commit(&dir, "e", "e");
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/f"), "PRECIOUS").unwrap();
        let (ctx, mut t) = tab(&dir);
        hard_reset(&ctx, &mut t, &rev(&dir, "HEAD~1"));
        assert_eq!(read(&dir, "d").as_deref(), Some("1"));
        step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, "d/f").as_deref(), Some("PRECIOUS"));
        assert!(t.undo.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- every op that moves HEAD backs up what git would overwrite ----

    /// A checkout overwrites an ignored file the target tracks; ⌘Z brings it back.
    /// Abort merge throws a hand resolution away; ⌘Z brings it back.
    #[test]
    fn checkout_and_merge_abort_back_up_what_they_overwrite() {
        let dir = repo("undo-way-checkout");
        commit(&dir, "f", "1\n");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, ".env", "committed\n");
        commit(&dir, "f", "B\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "f", "M\n");
        std::fs::write(dir.join(".git/info/exclude"), ".env\n").unwrap();
        std::fs::write(dir.join(".env"), "PRECIOUS\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        let msgs = step(&ctx, &mut t, Action::Git(Op::new("Checkout feat", &["checkout", "feat", "--"]).undo(Undo::Keep)));
        assert_eq!(read(&dir, ".env").as_deref(), Some("committed\n"), "{msgs:?}");
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, ".env").as_deref(), Some("PRECIOUS\n"), "{msgs:?}");
        std::fs::remove_file(dir.join(".env")).unwrap();
        assert!(git::run(&dir, &["merge", "feat"]).is_err());
        std::fs::write(dir.join("f"), "RESOLVED BY HAND\n").unwrap();
        step(&ctx, &mut t, Action::Git(super::super::sequencer_op("merge", true)));
        assert_eq!(read(&dir, "f").as_deref(), Some("M\n"));
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, "f").as_deref(), Some("RESOLVED BY HAND\n"), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undoing_an_abort_or_discard_all_resumes_the_merge_or_pick() {
        for (n, cmd) in ["merge", "cherry-pick", "discard"].into_iter().enumerate() {
            let dir = repo(&format!("undo-abort-resumes-{n}"));
            commit(&dir, "f", "base\n");
            commit(&dir, "g", "base\n");
            sh(&dir, &["checkout", "-q", "-b", "feat"]);
            std::fs::write(dir.join("f"), "theirs\n").unwrap();
            std::fs::write(dir.join("g"), "theirs\n").unwrap();
            sh(&dir, &["commit", "-q", "-am", "theirs"]);
            sh(&dir, &["checkout", "-q", "main"]);
            std::fs::write(dir.join("f"), "ours\n").unwrap();
            std::fs::write(dir.join("g"), "ours\n").unwrap();
            sh(&dir, &["commit", "-q", "-am", "ours"]);
            assert!(git::run(&dir, &[if cmd == "cherry-pick" { cmd } else { "merge" }, "feat"]).is_err());
            std::fs::write(dir.join("f"), "both\n").unwrap();
            sh(&dir, &["add", "f"]);
            let git_dir = dir.join(".git");
            let now = || {
                let marks = ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "CHERRY_PICK_HEAD"].map(|m| std::fs::read_to_string(git_dir.join(m)).ok());
                (marks, sh(&dir, &["ls-files", "-s"]), sh(&dir, &["status", "--porcelain"]), read(&dir, "g"))
            };
            let started = now();
            assert!(started.2.contains("UU g"), "{started:?}");
            let (ctx, mut t) = tab(&dir);
            step(&ctx, &mut t, if cmd == "discard" { discard_all() } else { Action::Git(super::super::sequencer_op(cmd, true)) });
            assert_eq!(git::state(&git_dir), git::State::Clean);
            for redo in [true, false] {
                let msgs = step(&ctx, &mut t, Action::Undo);
                assert_eq!(now(), started, "{cmd} {msgs:?}");
                if redo {
                    step(&ctx, &mut t, Action::Redo);
                    assert_eq!(git::state(&git_dir), git::State::Clean, "{cmd}");
                    assert!(sh(&dir, &["ls-files", "-u"]).is_empty());
                }
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_tracking_branch_undone_takes_its_config_and_redo_brings_it_back() {
        let dir = repo("undo-tracking-config");
        sh(&dir, &["remote", "add", "origin", "https://example.invalid/r.git"]);
        sh(&dir, &["update-ref", "refs/remotes/origin/rem", "HEAD"]);
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, Action::Git(Op::new("Checkout origin/rem", &["checkout", "-b", "rem", "--track", "origin/rem", "--"]).undo(Undo::Keep)));
        let cfg = || git::run(&dir, &["config", "--get-regexp", r"^branch\.rem\."]).unwrap_or_default();
        let tracked = cfg();
        assert!(tracked.contains("branch.rem.merge refs/heads/rem"), "{tracked}");
        for _ in 0..2 {
            step(&ctx, &mut t, Action::Undo);
            assert!(sh(&dir, &["branch", "--list", "rem"]).is_empty() && cfg().is_empty());
            step(&ctx, &mut t, Action::Redo);
            assert_eq!(cfg(), tracked);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undoing_a_squash_merge_drops_its_message() {
        let dir = repo("undo-squash-msg");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "f", "feat\n");
        sh(&dir, &["checkout", "-q", "main"]);
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, Action::Git(super::super::merge_opts::merge_op("feat", "feat", Some(super::super::merge_opts::Merge::Squash))));
        let msg = dir.join(".git/SQUASH_MSG");
        assert!(msg.exists());
        step(&ctx, &mut t, Action::Undo);
        assert!(!msg.exists() && read(&dir, "f").is_none());
        step(&ctx, &mut t, Action::Redo);
        assert!(msg.exists() && read(&dir, "f").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_discarded_intent_to_add_comes_back_as_one() {
        let dir = repo("undo-intent");
        std::fs::write(dir.join("ita"), "intent\n").unwrap();
        sh(&dir, &["add", "-N", "ita"]);
        let (ctx, mut t) = tab(&dir);
        let c = t.data.as_ref().unwrap().status.unstaged.iter().find(|c| c.path == "ita").unwrap().clone();
        let mut op = super::super::discard_op(&c);
        op.confirm = None;
        step(&ctx, &mut t, Action::Git(op));
        assert!(read(&dir, "ita").is_none() && sh(&dir, &["status", "--porcelain"]).is_empty());
        for redo in [true, false] {
            step(&ctx, &mut t, Action::Undo);
            assert_eq!(sh(&dir, &["status", "--porcelain"]), " A ita\n");
            assert_eq!(read(&dir, "ita").as_deref(), Some("intent\n"));
            if redo {
                step(&ctx, &mut t, Action::Redo);
                assert!(read(&dir, "ita").is_none() && sh(&dir, &["status", "--porcelain"]).is_empty());
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn redoing_a_conflicted_stash_pop_brings_the_conflict_back() {
        let dir = repo("redo-pop-conflict");
        commit(&dir, "a", "base\n");
        std::fs::write(dir.join("a"), "stashed\n").unwrap();
        sh(&dir, &["stash", "push", "-q"]);
        commit(&dir, "a", "committed\n");
        let (ctx, mut t) = tab(&dir);
        let s = t.data.as_ref().unwrap().stashes[0].clone();
        step(&ctx, &mut t, Action::Git(super::super::pop_stash(&s)));
        let popped = (sh(&dir, &["ls-files", "-s"]), read(&dir, "a"));
        assert!(sh(&dir, &["ls-files", "-u"]).contains("\ta"), "{popped:?}");
        step(&ctx, &mut t, Action::Undo);
        assert!(sh(&dir, &["ls-files", "-u"]).is_empty() && read(&dir, "a").as_deref() == Some("committed\n"));
        step(&ctx, &mut t, Action::Redo);
        assert_eq!((sh(&dir, &["ls-files", "-s"]), read(&dir, "a")), popped);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pull overwrites an ignored file the fetched commits track; ⌘Z brings it back.
    #[test]
    fn pull_backs_up_an_ignored_file_it_overwrites() {
        let root = scratch("undo-way-pull");
        sh(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        sh(&root, &["clone", "-q", "remote.git", "a"]);
        sh(&root, &["clone", "-q", "remote.git", "b"]);
        let (a, b) = (root.join("a"), root.join("b"));
        identity(&a);
        commit(&a, "keep", "k\n");
        sh(&a, &["push", "-q", "-u", "origin", "main"]);
        identity(&b);
        sh(&b, &["pull", "-q", "origin", "main"]);
        sh(&b, &["branch", "-q", "-u", "origin/main"]);
        commit(&a, "local.cfg", "UPSTREAM\n");
        sh(&a, &["push", "-q", "origin", "main"]);
        std::fs::write(b.join(".git/info/exclude"), "local.cfg\n").unwrap();
        std::fs::write(b.join("local.cfg"), "MY LOCAL\n").unwrap();
        let (ctx, mut t) = tab(&b);
        step(&ctx, &mut t, Action::Git(super::super::pull_op(super::super::PullMode::Merge)));
        assert_eq!(read(&b, "local.cfg").as_deref(), Some("UPSTREAM\n"));
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&b, "local.cfg").as_deref(), Some("MY LOCAL\n"), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The banner's Continue rebase replays a commit that tracks an ignored file made meanwhile.
    #[test]
    fn continue_rebase_backs_up_an_ignored_file_it_overwrites() {
        let dir = repo("undo-way-continue");
        commit(&dir, ".gitignore", ".env\n");
        commit(&dir, "f", "1\n");
        sh(&dir, &["checkout", "-q", "-b", "topic"]);
        commit(&dir, "f", "T\n");
        std::fs::write(dir.join(".env"), "committed").unwrap();
        sh(&dir, &["add", "-f", ".env"]);
        sh(&dir, &["commit", "-q", "-m", "env"]);
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "f", "M\n");
        sh(&dir, &["checkout", "-q", "topic"]);
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, Action::Git(Op::new("Rebase", &["rebase", "main"]).undo(Undo::Keep)));
        std::fs::write(dir.join("f"), "RES\n").unwrap();
        sh(&dir, &["add", "f"]);
        std::fs::write(dir.join(".env"), "PRECIOUS").unwrap();
        step(&ctx, &mut t, Action::Git(Op::new("Continue rebase", &["-c", "core.editor=true", "rebase", "--continue"]).undo(Undo::Keep)));
        assert_eq!(read(&dir, ".env").as_deref(), Some("committed"));
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert!(safe(&dir, ".env", "PRECIOUS"), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An assume-unchanged edit counts as a local change: a ref undo keeps it.
    #[test]
    fn ref_undo_keeps_an_assume_unchanged_edit() {
        let dir = repo("undo-assume");
        commit(&dir, "cfg", "main cfg\n");
        sh(&dir, &["checkout", "-q", "-b", "x"]);
        commit(&dir, "cfg", "x cfg\n");
        sh(&dir, &["checkout", "-q", "main"]);
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, Action::Git(Op::new("Checkout x", &["checkout", "x", "--"]).undo(Undo::Keep)));
        sh(&dir, &["update-index", "--assume-unchanged", "cfg"]);
        std::fs::write(dir.join("cfg"), "MY LOCAL CFG\n").unwrap();
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, "cfg").as_deref(), Some("MY LOCAL CFG\n"), "{msgs:?}");
        assert_eq!(sh(&dir, &["symbolic-ref", "--short", "HEAD"]), "main\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A FIFO where a backed-up file was never hangs ⌘Z: it counts as changed.
    #[cfg(unix)]
    #[test]
    fn undo_never_reads_a_fifo() {
        let dir = repo("undo-fifo");
        std::fs::write(dir.join("keep"), "P15\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, discard("keep"));
        std::fs::remove_file(dir.join("keep")).unwrap();
        assert!(std::process::Command::new("mkfifo").arg(dir.join("keep")).status().unwrap().success());
        let fifo = dir.join("keep");
        // Unblocks a reader that hangs, so the test fails instead of hanging.
        let guard = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(8));
            let _ = std::fs::OpenOptions::new().write(true).open(fifo).map(drop);
        });
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert!(in_stash(&dir, "keep", "P15\n") && t.undo.is_empty(), "{msgs:?}");
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Names that `--stdin-paths` would rewrite (a leading quote, a trailing CR) are
    /// backed up as themselves, beside the file their rewrite names.
    #[cfg(unix)]
    #[test]
    fn odd_names_are_backed_up_as_themselves() {
        for odd in ["\"notes\"", "x\r"] {
            let dir = repo("undo-oddname");
            let plain = odd.trim_matches(['"', '\r']);
            std::fs::write(dir.join(odd), "SECRET").unwrap();
            std::fs::write(dir.join(plain), "OTHER").unwrap();
            let (ctx, mut t) = tab(&dir);
            step(&ctx, &mut t, Action::Git(Op::new("Discard", &["--literal-pathspecs", "clean", "-fq", "--", odd]).light().backup(Backup::Paths(vec![odd.into()]))));
            assert!(read(&dir, odd).is_none());
            let msgs = step(&ctx, &mut t, Action::Undo);
            assert_eq!(read(&dir, odd).as_deref(), Some("SECRET"), "{odd:?} {msgs:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
        // A hard reset over an edit to `"q"` beside a clean `q` is backed up and undone.
        let dir = repo("undo-oddname-reset");
        std::fs::write(dir.join("\"q\""), "base").unwrap();
        std::fs::write(dir.join("q"), "Q").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-q", "-m", "q"]);
        std::fs::write(dir.join("\"q\""), "MINE").unwrap();
        let (ctx, mut t) = tab(&dir);
        hard_reset(&ctx, &mut t, &rev(&dir, "HEAD"));
        let msgs = step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, "\"q\"").as_deref(), Some("MINE"), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A merge tool closing while a redo runs keeps the step being redone.
    #[cfg(unix)]
    #[test]
    fn a_merge_tool_closing_mid_redo_keeps_the_step() {
        let dir = repo("undo-tool-redo");
        commit(&dir, "c.txt", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "feature"]);
        commit(&dir, "c.txt", "feature\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "c.txt", "main\n");
        assert!(git::run(&dir, &["merge", "feature"]).is_err());
        for kv in [["merge.tool", "fake"], ["mergetool.fake.cmd", "cp \"$REMOTE\" \"$MERGED\""], ["mergetool.fake.trustExitCode", "true"], ["mergetool.keepBackup", "false"]] {
            sh(&dir, &["config", kv[0], kv[1]]);
        }
        std::fs::write(dir.join("keep"), "MINE\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, discard("keep"));
        step(&ctx, &mut t, Action::Undo);
        t.apply(&ctx, Action::Git(super::super::tools::mergetool_op("c.txt")));
        // The tool exits at once; its result waits for a poll.
        std::thread::sleep(std::time::Duration::from_millis(500));
        t.apply(&ctx, Action::Redo);
        assert!(t.running.is_some());
        let mut msgs = vec![];
        t.poll_tools(&ctx, &mut msgs);
        settle(&ctx, &mut t);
        while !t.ext_tools.is_empty() {
            settle(&ctx, &mut t);
        }
        assert_eq!(read(&dir, "keep").as_deref(), Some("k\n"));
        assert!(labels(&t).contains(&"Discard".to_owned()), "{:?}", labels(&t));
        assert_eq!(backups_settle(&dir, 4), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pruning skips the backups of a live session (another tab, a linked worktree, another
    /// Glasir) however old, and names it can't read; an ended session's go after 7 days.
    #[test]
    fn prune_never_takes_a_live_steps_backup() {
        let dir = repo("undo-prune-live");
        let c = sh(&dir, &["commit-tree", "-m", "b", &rev(&dir, "HEAD^{tree}")]).trim().to_owned();
        let (me, gone) = (std::process::id(), i32::MAX as u32 - 1);
        let names = [format!("1-{me}-1"), format!("1-{gone}-1"), "1-x-1".into()];
        for n in &names {
            sh(&dir, &["update-ref", &format!("refs/glasir/backups/{n}"), &c]);
        }
        let later = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 8 * 86_400;
        git::prune(&dir, later, 7);
        assert_eq!(sh(&dir, &["for-each-ref", "--format=%(refname:lstrip=3)", "refs/glasir/backups/"]), format!("1-{me}-1\n1-x-1\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }


    /// A redo that wrote files, then failed on a stale ref lock, then Skip: both
    /// sides of the step's files stay in the stash list (the user's work is the `before` one).
    #[test]
    fn x3_skip_after_a_half_done_redo_keeps_both_sides() {
        let dir = repo("x3-skip");
        commit(&dir, "f", "1\n");
        commit(&dir, "f", "2\n");
        std::fs::write(dir.join("f"), "MINE\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        hard_reset(&ctx, &mut t, &rev(&dir, "HEAD~1"));
        step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, "f").as_deref(), Some("MINE\n"));
        std::fs::write(dir.join(".git/refs/heads/main.lock"), "").unwrap();
        step(&ctx, &mut t, Action::Redo);
        let c = t.choice.take().expect("redo fails");
        std::fs::remove_file(dir.join(".git/refs/heads/main.lock")).unwrap();
        step(&ctx, &mut t, Action::Git(c.ops[1].1.clone()));
        assert!(safe(&dir, "f", "MINE\n"), "{}", sh(&dir, &["stash", "list"]));
        assert_eq!((t.undo.len(), t.redo.len(), backups(&dir)), (0, 0, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A mixed reset (and a HEAD split, which is one) backs up a partly staged file;
    /// ⌘Z brings the staged version back and never touches the file, even edited since.
    #[test]
    fn x3_mixed_undo_restores_the_index_only() {
        for split in [false, true] {
            let dir = repo(&format!("x3-mixed-{split}"));
            commit(&dir, "f", "1\n");
            commit(&dir, "g", "2\n");
            let (ctx, mut t) = tab(&dir);
            std::fs::write(dir.join("f"), "STAGED\n").unwrap();
            sh(&dir, &["add", "f"]);
            std::fs::write(dir.join("f"), "WORK\n").unwrap();
            match split {
                true => drop(step(&ctx, &mut t, Action::Split(rev(&dir, "HEAD")))),
                false => {
                    let mut op = reset_op(t.data.as_ref().unwrap(), &rev(&dir, "HEAD~1"), "--mixed");
                    op.confirm = None;
                    step(&ctx, &mut t, Action::Git(op));
                }
            }
            assert_eq!(rev(&dir, "HEAD"), rev(&dir, "main"));
            std::fs::write(dir.join("f"), "EDITED SINCE\n").unwrap();
            let m = step(&ctx, &mut t, Action::Undo);
            assert!(t.choice.is_none() && m.is_empty(), "{split}: {m:?}");
            assert_eq!(sh(&dir, &["show", ":f"]), "STAGED\n", "{split}");
            assert_eq!(read(&dir, "f").as_deref(), Some("EDITED SINCE\n"));
            assert_eq!(sh(&dir, &["stash", "list"]), "");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// `.gitignore` has `.env`; feat force-adds `.env`='theirs'. The user's ignored `.env`='MINE'.
    fn ignored_env(name: &str) -> (std::path::PathBuf, String) {
        let dir = repo(name);
        commit(&dir, ".gitignore", ".env\n");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        std::fs::write(dir.join(".env"), "theirs\n").unwrap();
        sh(&dir, &["add", "-f", ".env"]);
        sh(&dir, &["commit", "-q", "-m", "env"]);
        let c = rev(&dir, "HEAD");
        sh(&dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join(".env"), "MINE\n").unwrap();
        (dir, c)
    }

    /// Squash merge, cherry-pick -n, stash apply and pop leave HEAD where it was: they back up
    /// the ignored file they overwrite, and ⌘Z puts it back and takes their changes out.
    #[test]
    fn x3_file_only_ops_back_up_and_undo() {
        for kind in 0..4 {
            let (dir, c) = ignored_env(&format!("x3-fileonly-{kind}"));
            let (ctx, mut t) = tab(&dir);
            let op = match kind {
                0 => super::super::merge_opts::merge_op("feat", "feat", Some(super::super::merge_opts::Merge::Squash)),
                1 => super::super::merge_opts::pick_op("Cherry-pick", &[&c], false, true),
                _ => {
                    std::fs::rename(dir.join(".env"), dir.join("mine")).unwrap();
                    std::fs::write(dir.join(".env"), "theirs\n").unwrap();
                    sh(&dir, &["add", "-f", ".env"]);
                    sh(&dir, &["stash", "push", "-q"]);
                    std::fs::rename(dir.join("mine"), dir.join(".env")).unwrap();
                    step(&ctx, &mut t, Action::Reload);
                    let s = git::stashes(&dir).unwrap().remove(0);
                    if kind == 2 { super::super::apply_stash_op(&s) } else { super::super::pop_stash(&s) }
                }
            };
            let m = step(&ctx, &mut t, Action::Git(op));
            assert_eq!(read(&dir, ".env").as_deref(), Some("theirs\n"), "{kind}: {m:?}");
            assert_eq!(labels(&t).len(), 1, "{kind}: {m:?}");
            let m = step(&ctx, &mut t, Action::Undo);
            assert_eq!(read(&dir, ".env").as_deref(), Some("MINE\n"), "{kind}: {m:?}");
            assert_eq!(sh(&dir, &["status", "--porcelain"]), "", "{kind}");
            assert_eq!(sh(&dir, &["stash", "list"]).lines().count(), usize::from(kind >= 2), "{kind}");
            step(&ctx, &mut t, Action::Redo);
            assert_eq!(read(&dir, ".env").as_deref(), Some("theirs\n"), "{kind}");
            assert_eq!(sh(&dir, &["stash", "list"]).lines().count(), usize::from(kind == 2), "{kind}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// Ops that replay commits (rebase onto, rebase <b> onto, amend into an older commit,
    /// revert, drop) write files the commits they name never hold: one commit adds `.env`, a
    /// later one removes it. The user's ignored `.env` is backed up and ⌘Z brings it back.
    #[test]
    fn x3_replays_back_up_what_their_commits_overwrite() {
        for kind in 0..5 {
            let dir = repo(&format!("x3-replay-{kind}"));
            commit(&dir, ".gitignore", ".env\n");
            commit(&dir, "a", "1\n");
            let x = rev(&dir, "HEAD");
            let feat = kind < 3;
            if feat {
                sh(&dir, &["checkout", "-q", "-b", "feat"]);
            }
            std::fs::write(dir.join(".env"), "committed\n").unwrap();
            sh(&dir, &["add", "-f", ".env"]);
            sh(&dir, &["commit", "-q", "-m", "add env"]);
            sh(&dir, &["rm", "-q", "--cached", ".env"]);
            std::fs::remove_file(dir.join(".env")).unwrap();
            sh(&dir, &["commit", "-q", "-m", "drop env"]);
            let drop_env = rev(&dir, "HEAD");
            if feat {
                sh(&dir, &["checkout", "-q", "main"]);
                commit(&dir, "m", "m\n");
                sh(&dir, &["checkout", "-q", if kind == 1 { "main" } else { "feat" }]);
            } else {
                commit(&dir, "z", "z\n");
            }
            std::fs::write(dir.join(".env"), "MINE\n").unwrap();
            let (ctx, mut t) = tab(&dir);
            let act = match kind {
                0 => Action::Git(Op::new("Rebase onto main", &["rebase", "main"]).undo(Undo::Keep)),
                1 => Action::Git(Op::new("Rebase feat onto main", &["rebase", "main", "feat"]).undo(Undo::Keep)),
                2 => {
                    std::fs::write(dir.join("a"), "2\n").unwrap();
                    sh(&dir, &["add", "a"]);
                    Action::AmendInto(x.clone())
                }
                3 => Action::Git(Op::new("Revert", &["revert", "--no-edit", &drop_env]).undo(Undo::Keep)),
                _ => Action::Rewrite { commit: drop_env.clone(), act: super::super::rebase::Act::Drop, message: None },
            };
            let m = step(&ctx, &mut t, act);
            if let Some(op) = t.confirm.take() {
                let m = step(&ctx, &mut t, Action::Git(Op { confirm: None, ..op }));
                assert!(t.choice.is_none(), "{m:?}");
            }
            assert!(read(&dir, ".env").as_deref() != Some("MINE\n"), "{kind}: the op ran: {m:?}");
            let m = step(&ctx, &mut t, Action::Undo);
            assert_eq!(read(&dir, ".env").as_deref(), Some("MINE\n"), "{kind}: {m:?} {:?}", labels(&t));
            if kind == 2 {
                assert_eq!(sh(&dir, &["show", ":a"]), "2\n");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// "Restore conflict markers" over a half-done resolution: ⌘Z brings the resolution back.
    #[test]
    fn x3_restore_conflict_markers_is_undoable() {
        let dir = repo("x3-markers");
        commit(&dir, "f", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "f", "theirs\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "f", "ours\n");
        let _ = git::run(&dir, &["merge", "feat"]);
        std::fs::write(dir.join("f"), "MY RESOLUTION\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, Action::Git(super::super::tools::restore_markers_op("f")));
        assert!(read(&dir, "f").unwrap().contains("<<<<<<<"));
        step(&ctx, &mut t, Action::Undo);
        assert_eq!(read(&dir, "f").as_deref(), Some("MY RESOLUTION\n"));
        assert!(sh(&dir, &["ls-files", "-u"]).contains("\tf"), "still conflicted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Ref moves that add or remove an unpopulated submodule (an empty folder) never stick.
    #[test]
    fn x3_submodule_ref_moves_complete() {
        for kind in 0..3 {
            let root = scratch(&format!("x3-sub-{kind}"));
            let (sub, dir) = (root.join("sub"), root.join("main"));
            for d in [&sub, &dir] {
                std::fs::create_dir_all(d).unwrap();
                sh(d, &["init", "-q", "-b", "main"]);
                identity(d);
                commit(d, "keep", "k\n");
            }
            sh(&dir, &["checkout", "-q", "-b", "feat"]);
            sh(&dir, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", sub.to_str().unwrap(), "lib"]);
            sh(&dir, &["commit", "-q", "-m", "sub"]);
            sh(&dir, &["checkout", "-q", if kind == 2 { "feat" } else { "main" }]);
            if kind == 1 {
                commit(&dir, "m", "m\n");
            }
            let _ = std::fs::remove_dir_all(dir.join("lib"));
            let at = rev(&dir, "HEAD");
            let (ctx, mut t) = tab(&dir);
            let op = match kind {
                0 => Op::new("Checkout feat", &["checkout", "feat", "--"]).undo(Undo::Keep),
                1 => super::super::merge_opts::merge_op("feat", "feat", None),
                _ => Op::new("Checkout main", &["checkout", "main", "--"]).undo(Undo::Keep),
            };
            step(&ctx, &mut t, Action::Git(op));
            assert_ne!(rev(&dir, "HEAD"), at);
            for (a, want) in [(Action::Undo, at.clone()), (Action::Redo, rev(&dir, "HEAD")), (Action::Undo, at.clone())] {
                let m = step(&ctx, &mut t, a);
                assert!(t.choice.is_none(), "{kind}: {m:?}");
                assert_eq!((rev(&dir, "HEAD"), sh(&dir, &["status", "--porcelain"])), (want, String::new()), "{kind}: {m:?}");
            }
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// ⌘Z of "Create branch y" after a commit made in a terminal leaves HEAD's files alone.
    #[test]
    fn x3_ref_undo_keeps_files_of_a_branch_it_does_not_move() {
        let dir = repo("x3-branch-files");
        commit(&dir, "a", "1\n");
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, branch("y"));
        commit(&dir, "a", "TERMINAL\n");
        step(&ctx, &mut t, Action::Reload);
        step(&ctx, &mut t, Action::Undo);
        assert_eq!((sh(&dir, &["status", "--porcelain"]), sh(&dir, &["branch", "--list", "y"])), (String::new(), String::new()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A ref move writes files as git checks them out: through a smudge filter.
    #[test]
    fn x3_ref_moves_check_files_out_through_filters() {
        let dir = repo("x3-filter");
        sh(&dir, &["config", "filter.up.smudge", "tr a-z A-Z"]);
        sh(&dir, &["config", "filter.up.clean", "tr A-Z a-z"]);
        commit(&dir, ".gitattributes", "*.txt filter=up\n");
        commit(&dir, "f.txt", "hello\n");
        sh(&dir, &["checkout", "-q", "-b", "b"]);
        commit(&dir, "f.txt", "world\n");
        sh(&dir, &["checkout", "-q", "main"]);
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, Action::Git(Op::new("Checkout b", &["checkout", "b", "--"]).undo(Undo::Keep)));
        step(&ctx, &mut t, Action::Undo);
        assert_eq!((read(&dir, "f.txt").as_deref(), sh(&dir, &["status", "--porcelain"])), (Some("HELLO\n"), String::new()));
        let _ = std::fs::remove_dir_all(&dir);
    }


    fn checkout(b: &str) -> Action {
        Action::Git(Op::new(format!("Checkout {b}"), &["checkout", b, "--"]).undo(Undo::Keep))
    }

    /// `want` at `p` on disk, in the stash list or in a backup (files, index, untracked files).
    fn kept(dir: &Path, p: &str, want: &str) -> bool {
        let refs = sh(dir, &["for-each-ref", "--format=%(objectname)", "refs/glasir/backups/"]);
        safe(dir, p, want) || refs.lines().any(|h| ["", "^2", "^3"].iter().any(|s| git::run(dir, &["show", &format!("{h}{s}:{p}")]).is_ok_and(|s| s == want)))
    }

    /// A ref move's undo never writes a path over a modified or staged tracked file at, under or
    /// above it (a file that is a folder on the other side, or the other way round).
    #[test]
    fn ref_undo_keeps_tracked_changes_in_a_paths_way() {
        // (a) a checkout made file d a folder; d/f edited.
        let dir = repo("uf-move-a");
        commit(&dir, "d", "B\n");
        sh(&dir, &["branch", "b"]);
        sh(&dir, &["rm", "-q", "d"]);
        std::fs::create_dir(dir.join("d")).unwrap();
        commit(&dir, "d/f", "A\n");
        sh(&dir, &["checkout", "-q", "b"]);
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, checkout("main"));
        std::fs::write(dir.join("d/f"), "PRECIOUS\n").unwrap();
        let m = step(&ctx, &mut t, Action::Undo);
        assert!(kept(&dir, "d/f", "PRECIOUS\n"), "(a) {m:?} d={:?}", read(&dir, "d"));
        let _ = std::fs::remove_dir_all(&dir);
        // (b) a hard reset made file d a folder; d/f edited.
        let dir = repo("uf-move-b");
        std::fs::create_dir(dir.join("d")).unwrap();
        commit(&dir, "d/f", "B\n");
        let x = rev(&dir, "HEAD");
        sh(&dir, &["rm", "-rq", "d"]);
        commit(&dir, "d", "A\n");
        let (ctx, mut t) = tab(&dir);
        hard_reset(&ctx, &mut t, &x);
        std::fs::write(dir.join("d/f"), "PRECIOUS\n").unwrap();
        let m = step(&ctx, &mut t, Action::Undo);
        assert!(kept(&dir, "d/f", "PRECIOUS\n"), "(b) {m:?} d={:?}", read(&dir, "d"));
        let _ = std::fs::remove_dir_all(&dir);
        // (c) a checkout made folder d a file; d edited.
        let dir = repo("uf-move-c");
        std::fs::create_dir(dir.join("d")).unwrap();
        commit(&dir, "d/f", "B\n");
        sh(&dir, &["branch", "b"]);
        sh(&dir, &["rm", "-rq", "d"]);
        commit(&dir, "d", "A\n");
        sh(&dir, &["checkout", "-q", "b"]);
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, checkout("main"));
        std::fs::write(dir.join("d"), "PRECIOUS\n").unwrap();
        let m = step(&ctx, &mut t, Action::Undo);
        assert!(kept(&dir, "d", "PRECIOUS\n"), "(c) {m:?} d/f={:?}", read(&dir, "d/f"));
        let _ = std::fs::remove_dir_all(&dir);
        // (d) a checkout deleted file d; a new d/f staged, then edited: both versions stay.
        let dir = repo("uf-move-d");
        commit(&dir, "d", "A\n");
        sh(&dir, &["branch", "a"]);
        sh(&dir, &["rm", "-q", "d"]);
        commit(&dir, "other", "o\n");
        sh(&dir, &["checkout", "-q", "a"]);
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, checkout("main"));
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/f"), "STAGED\n").unwrap();
        sh(&dir, &["add", "d/f"]);
        std::fs::write(dir.join("d/f"), "WORK\n").unwrap();
        let m = step(&ctx, &mut t, Action::Undo);
        assert!(read(&dir, "d/f").as_deref() == Some("WORK\n") && git::run(&dir, &["show", ":d/f"]).is_ok_and(|s| s == "STAGED\n"), "(d) {m:?} d={:?} index={:?}", read(&dir, "d"), git::run(&dir, &["show", ":d/f"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A nested git repository (ignored or untracked) where the target tracks a file: a hard
    /// reset or checkout would delete it, .git and all, and no backup can hold it: refused.
    #[test]
    fn an_op_that_would_delete_a_nested_repo_is_refused() {
        for how in ["reset", "checkout", "untracked-reset", "discard-all"] {
            let dir = repo(&format!("uf-nested-{how}"));
            commit(&dir, "vendor", "v\n");
            let a = rev(&dir, "HEAD");
            sh(&dir, &["branch", "old"]);
            if how != "discard-all" {
                sh(&dir, &["rm", "-q", "vendor"]);
                sh(&dir, &["commit", "-q", "-m", "rm vendor"]);
            } else {
                std::fs::remove_file(dir.join("vendor")).unwrap();
            }
            let n = dir.join("vendor/x");
            std::fs::create_dir_all(&n).unwrap();
            sh(&n, &["init", "-q"]);
            identity(&n);
            commit(&n, "p", "COMMITTED\n");
            std::fs::write(n.join("w"), "WIP\n").unwrap();
            if how != "untracked-reset" {
                std::fs::write(dir.join(".git/info/exclude"), "vendor/\n").unwrap();
            }
            let (ctx, mut t) = tab(&dir);
            let m = match how {
                "checkout" => step(&ctx, &mut t, checkout("old")),
                "discard-all" => step(&ctx, &mut t, discard_all()),
                _ => hard_reset(&ctx, &mut t, &a),
            };
            let m = [m, step(&ctx, &mut t, Action::Undo)].concat();
            assert!(n.join(".git").is_dir() && read(&n, "w").as_deref() == Some("WIP\n"), "{how}: {m:?}");
            assert!(m.iter().any(|m| m.contains("vendor/x")), "{how}: {m:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// With submodule.recurse on, Discard all and a hard reset reset a submodule's work tree
    /// too, which no backup can hold: refused while it has uncommitted changes.
    #[test]
    fn a_recursing_reset_over_submodule_edits_is_refused() {
        for how in ["discard-all", "reset"] {
            let root = scratch(&format!("uf-sub-{how}"));
            let sub = root.join("subsrc");
            std::fs::create_dir_all(&sub).unwrap();
            sh(&sub, &["init", "-q", "-b", "main"]);
            identity(&sub);
            commit(&sub, "f", "sub0\n");
            let dir = root.join("sup");
            std::fs::create_dir_all(&dir).unwrap();
            sh(&dir, &["init", "-q", "-b", "main"]);
            identity(&dir);
            commit(&dir, "keep", "k\n");
            sh(&dir, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", sub.to_str().unwrap(), "sub"]);
            sh(&dir, &["commit", "-q", "-m", "add sub"]);
            commit(&dir, "keep", "k2\n");
            sh(&dir, &["config", "submodule.recurse", "true"]);
            std::fs::write(dir.join("sub/f"), "SUB-WIP\n").unwrap();
            std::fs::write(dir.join("keep"), "K-WIP\n").unwrap();
            let (ctx, mut t) = tab(&dir);
            let m = if how == "reset" { hard_reset(&ctx, &mut t, &rev(&dir, "HEAD~1")) } else { step(&ctx, &mut t, discard_all()) };
            let m = [m, step(&ctx, &mut t, Action::Undo)].concat();
            assert_eq!((read(&dir, "sub/f").as_deref(), read(&dir, "keep").as_deref()), (Some("SUB-WIP\n"), Some("K-WIP\n")), "{how}: {m:?}");
            assert!(m.iter().any(|m| m.contains("submodule")), "{how}: {m:?}");
            // Nothing to lose in the submodule: it runs.
            std::fs::write(dir.join("sub/f"), "sub0\n").unwrap();
            let m = if how == "reset" { hard_reset(&ctx, &mut t, &rev(&dir, "HEAD~1")) } else { step(&ctx, &mut t, discard_all()) };
            assert_eq!(labels(&t).len(), 1, "{how}: {m:?}");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn a_step_keeps_only_the_refs_it_moved() {
        let dir = repo("undo-delta");
        commit(&dir, "keep", "k2\n");
        for i in 0..20 {
            sh(&dir, &["branch", &format!("b{i}")]);
            sh(&dir, &["tag", &format!("t{i}")]);
        }
        let start = git::snapshot(&dir).unwrap();
        let (ctx, mut t) = tab(&dir);
        step(&ctx, &mut t, branch("new"));
        hard_reset(&ctx, &mut t, &rev(&dir, "HEAD~1"));
        let done = git::snapshot(&dir).unwrap();
        let kept: Vec<(usize, usize)> = t.undo.iter().map(|e| e.refs.as_ref().map_or((9, 9), |r| (r.before.refs.len(), r.after.refs.len()))).collect();
        assert_eq!(kept, [(0, 1), (1, 1)]);
        sh(&dir, &["branch", "-f", "b3", "HEAD"]);
        let moved_b3 = git::snapshot(&dir).unwrap();
        step(&ctx, &mut t, Action::Undo);
        step(&ctx, &mut t, Action::Undo);
        let mut want = start.clone();
        want.refs.insert("refs/heads/b3".into(), moved_b3.refs["refs/heads/b3"].clone());
        assert_eq!(git::snapshot(&dir).unwrap(), want);
        sh(&dir, &["branch", "-f", "b3", &start.refs["refs/heads/b3"]]);
        step(&ctx, &mut t, Action::Redo);
        step(&ctx, &mut t, Action::Redo);
        assert_eq!(git::snapshot(&dir).unwrap(), done);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
