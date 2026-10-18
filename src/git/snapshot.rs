//! Undo snapshots: the local branches, tags and HEAD ([`Snapshot`]), and durable file
//! backups ([`Snap`]: a stash-shaped commit kept by a `refs/glasir/backups/*` ref).

use super::{Res, run, run_input};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::process::Stdio;

/// Local branches, tags and HEAD: what undo/redo restores.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// `refs/heads/x`, or a commit id when detached.
    pub head: String,
    pub refs: BTreeMap<String, String>,
}

pub fn snapshot(dir: &Path) -> Res<Snapshot> {
    let head = match run(dir, &["symbolic-ref", "-q", "HEAD"]) {
        Ok(h) => h,
        Err(_) => run(dir, &["rev-parse", "HEAD"])?,
    };
    let refs = run(dir, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads", "refs/tags"])?;
    Ok(Snapshot {
        head: head.trim().into(),
        refs: refs.lines().filter_map(|l| l.split_once(' ')).map(|(r, o)| (r.into(), o.into())).collect(),
    })
}

/// Branches other worktrees have checked out, or are rebasing or bisecting (those show as detached).
pub(crate) fn used_elsewhere(dir: &Path) -> Res<Vec<String>> {
    let wts = super::read::parse_worktrees(&run(dir, &["worktree", "list", "--porcelain", "-z"])?);
    let mut refs: Vec<String> = wts.iter().filter(|w| !w.prunable).filter_map(|w| Some(format!("refs/heads/{}", w.branch.as_ref()?))).collect();
    let out = run(dir, &["rev-parse", "--path-format=absolute", "--git-dir", "--git-common-dir"])?;
    let mut l = out.lines().map(|l| std::fs::canonicalize(l).unwrap_or_else(|_| l.into()));
    let (own, common) = (l.next().unwrap_or_default(), l.next().unwrap_or_default());
    let linked = std::fs::read_dir(common.join("worktrees")).into_iter().flatten().flatten().map(|e| e.path());
    for g in std::iter::once(common.clone()).chain(linked).filter(|g| std::fs::canonicalize(g).ok().as_ref() != Some(&own)) {
        let read = |f: &str| std::fs::read_to_string(g.join(f)).ok().map(|s| s.trim().to_owned());
        refs.extend(read("rebase-merge/head-name").into_iter().chain(read("rebase-apply/head-name")));
        refs.extend(read("BISECT_START").map(|b| format!("refs/heads/{b}")));
    }
    Ok(refs)
}

/// One path's state. `disk`: raw (mode, oid), None = absent. `idx`: Some only when the index
/// differs from that side's HEAD (staged; mode 0 = a staged deletion), None = follows HEAD.
/// A [`FOREIGN`] oid can't be read or stored (a nested repo, an unreadable file): it equals nothing.
#[derive(Clone, PartialEq, Debug)]
pub struct Entry {
    pub name: String,
    pub disk: Option<(u32, String)>,
    pub idx: Option<(u32, String)>,
    pub unmerged: bool,
}

/// Path key (lowercased when `core.ignorecase`) -> its state; a missing key is absent.
pub type View = BTreeMap<String, Entry>;
pub const FOREIGN: &str = "?";
const DIR: u32 = 0o40000;
const PREFIX: &str = "refs/glasir/backups/";
type Blobs = HashMap<String, (u32, String)>;

/// A backup: a stash-shaped commit (the files; `^2` the index; `^3` untracked files) that
/// `refname` keeps. `paths`: what it covers; `unmerged`: those that were conflicted.
#[derive(Clone, Debug)]
pub struct Snap {
    pub commit: String,
    pub refname: String,
    pub paths: Vec<String>,
    pub unmerged: Vec<String>,
}

fn z(s: String) -> Vec<String> {
    s.split('\0').filter(|p| !p.is_empty()).map(String::from).collect()
}

fn key(p: &str, icase: bool) -> String {
    if icase { p.to_lowercase() } else { p.to_owned() }
}

fn icase(dir: &Path) -> bool {
    run(dir, &["config", "--type=bool", "core.ignorecase"]).is_ok_and(|s| s.trim() == "true")
}

pub fn ancestors(p: &str) -> impl DoubleEndedIterator<Item = &str> {
    p.match_indices('/').map(|(i, _)| &p[..i]).collect::<Vec<_>>().into_iter()
}

/// Whether a path is one of `keys`, under one, or a folder above one.
pub fn related<'a>(keys: impl IntoIterator<Item = &'a String>) -> impl Fn(&str) -> bool + 'a {
    let keys: HashSet<&str> = keys.into_iter().map(String::as_str).collect();
    let above: HashSet<&str> = keys.iter().copied().flat_map(ancestors).collect();
    move |k| keys.contains(k) || above.contains(k) || ancestors(k).any(|a| keys.contains(a))
}

fn lstat(dir: &Path, p: &str) -> std::io::Result<std::fs::Metadata> {
    std::fs::symlink_metadata(dir.join(p.trim_end_matches('/')))
}

fn gone(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory)
}

pub fn empty_tree(dir: &Path) -> Res<String> {
    run_input(dir, &["mktree"], Some(b"")).map(|t| t.trim().to_owned())
}

pub fn index_tree(dir: &Path) -> Res<String> {
    if !run(dir, &["ls-files", "-u"])?.trim().is_empty() {
        return Err("Resolve the conflicts first.".into());
    }
    let index = super::git_dir(dir)?.join("index");
    scratch(dir, |tmp| {
        let _ = std::fs::copy(&index, tmp);
        on_index(dir, tmp, &["write-tree"], "").map(|t| t.trim().to_owned())
    })
}

/// HEAD's commit, or None on an unborn branch.
pub fn head(dir: &Path) -> Option<String> {
    run(dir, &["rev-parse", "-q", "--verify", "HEAD^{commit}"]).ok().map(|h| h.trim().to_owned())
}

/// Runs git on the scratch index `idx`.
fn on_index(dir: &Path, idx: &Path, args: &[&str], input: &str) -> Res<String> {
    let mut c = super::cmd(dir);
    c.env("GIT_INDEX_FILE", idx);
    super::exec(c, dir, args, Some(input.as_bytes())).map(|o| String::from_utf8_lossy(&o).into_owned())
}

/// Runs `f` on a scratch file of its own in the git dir, removed afterwards; as an index: no
/// shared lock, and a missing file is an empty index.
pub(crate) fn scratch<T>(dir: &Path, f: impl FnOnce(&Path) -> Res<T>) -> Res<T> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let idx = super::git_dir(dir)?.join(format!("glasir-{}-{}.index", std::process::id(), SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let res = f(&idx);
    let _ = std::fs::remove_file(&idx);
    res
}

/// `<mode> <oid>\t<path>` records of `ls-tree -r` (`-s` = `ls-files -s`: with a stage) -> path ->
/// (mode, oid); with the unmerged paths (stage > 0) apart.
fn blobs(out: String) -> (Blobs, BTreeSet<String>) {
    let (mut b, mut unmerged) = (HashMap::new(), BTreeSet::new());
    for l in z(out) {
        let Some((meta, p)) = l.split_once('\t') else { continue };
        match meta.split(' ').collect::<Vec<_>>()[..] {
            [mode, "blob" | "commit", oid] | [mode, oid, "0"] => drop(b.insert(p.to_owned(), (u32::from_str_radix(mode, 8).unwrap_or(0), oid.to_owned()))),
            _ => drop(unmerged.insert(p.to_owned())),
        }
    }
    (b, unmerged)
}

fn tree(dir: &Path, rev: &str) -> Res<Blobs> {
    Ok(blobs(run(dir, &["ls-tree", "-r", "-z", rev])?).0)
}

/// `diff-index`/`diff-tree` raw records -> path -> (new mode, new oid); mode 0 = deleted.
fn raw(out: String) -> Blobs {
    z(out)
        .chunks(2)
        .filter_map(|c| {
            let m: Vec<&str> = c.first()?.trim_start_matches(':').split(' ').collect();
            let p = c.get(1)?.clone();
            (m.len() > 4 && !m[4].starts_with('U')).then(|| (p, (u32::from_str_radix(m[1], 8).unwrap_or(0), m[3].to_owned())))
        })
        .collect()
}

/// The files `scope` covers: the path itself, what's under it (a folder: ignored files too,
/// a nested repo as `name/`), and what's above it (a file where a folder goes, a folder where
/// an index entry is). Never walks the whole worktree.
pub fn leaves(dir: &Path, scope: &[String]) -> Res<Vec<String>> {
    let ic = icase(dir);
    let scope: Vec<&str> = scope.iter().map(|p| p.trim_end_matches('/')).filter(|p| !p.is_empty()).collect();
    let (mut out, mut dirs) = (BTreeSet::new(), vec![]);
    for p in &scope {
        match lstat(dir, p) {
            Ok(m) if m.is_dir() => dirs.push(*p),
            Err(e) if !gone(&e) => return Err(format!("{p}: {e}")),
            _ => drop(out.insert(p.to_string())),
        }
        out.extend(ancestors(p).filter(|a| lstat(dir, a).is_ok_and(|m| !m.is_dir())).map(String::from));
    }
    if !scope.is_empty() {
        let keys: Vec<String> = scope.iter().map(|p| key(p, ic)).collect();
        let near = related(&keys);
        out.extend(z(run(dir, &["ls-files", "-z"])?).into_iter().filter(|f| near(&key(f, ic))));
    }
    let magic = if ic { ":(literal,icase)" } else { ":(literal)" };
    // ponytail: a few hundred folders per command; scopes normally name a handful.
    for chunk in dirs.chunks(200) {
        let a: Vec<String> = ["ls-files", "-z", "-c", "-o", "--"].iter().map(|s| s.to_string()).chain(chunk.iter().map(|d| format!("{magic}{d}"))).collect();
        out.extend(z(run(dir, &a.iter().map(String::as_str).collect::<Vec<_>>())?));
    }
    Ok(out.into_iter().collect())
}

/// Each leaf's raw (mode, oid) on disk, None when no file is there (a tracked file that is a
/// folder now). A nested repo or submodule is `(DIR, FOREIGN)`; a file that can't be read, isn't
/// a plain file (a FIFO would block the read) or has a non-UTF-8 name `(mode, FOREIGN)`. Without
/// exec bits to read (Windows, `core.filemode=false`) a file keeps its index mode. Any size.
fn hash(dir: &Path, leaves: &[String], index: &Blobs, write: bool) -> Res<HashMap<String, Option<(u32, String)>>> {
    let filemode = cfg!(unix) && run(dir, &["config", "--type=bool", "core.filemode"]).map_or(true, |s| s.trim() != "false");
    let w: &[&str] = if write { &["-w"] } else { &[] };
    let (mut out, mut files) = (HashMap::new(), vec![]);
    for p in leaves {
        let e = match lstat(dir, p) {
            Err(e) => (!gone(&e)).then(|| (0o100644, FOREIGN.into())),
            Ok(m) if m.is_dir() && !p.ends_with('/') && index.get(p).is_none_or(|e| e.0 != 0o160000) => None,
            Ok(m) if m.is_dir() || p.ends_with('/') => Some((DIR, FOREIGN.into())),
            Ok(m) if m.is_symlink() => {
                let oid = std::fs::read_link(dir.join(p)).map_err(|e| e.to_string()).and_then(|t| {
                    #[cfg(unix)]
                    let t = std::os::unix::ffi::OsStrExt::as_bytes(t.as_os_str()).to_vec();
                    #[cfg(not(unix))]
                    let t = t.to_string_lossy().into_owned().into_bytes();
                    run_input(dir, &[&["hash-object", "--stdin"], w].concat(), Some(&t))
                });
                Some((0o120000, oid.map_or(FOREIGN.into(), |o| o.trim().to_owned())))
            }
            Ok(m) => {
                #[cfg(unix)]
                let x = filemode && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0;
                #[cfg(not(unix))]
                let x = false;
                let mode = if x || (!filemode && index.get(p).is_some_and(|e| e.0 == 0o100755)) { 0o100755 } else { 0o100644 };
                if m.is_file() && !p.contains('\u{FFFD}') {
                    files.push((p, mode));
                    continue;
                }
                Some((mode, FOREIGN.into()))
            }
        };
        out.insert(p.clone(), e);
    }
    let oids = hash_paths(dir, &[&["hash-object", "--no-filters"], w].concat(), &files.iter().map(|f| f.0.as_str()).collect::<Vec<_>>());
    out.extend(files.into_iter().zip(oids).map(|((p, mode), oid)| (p.clone(), Some((mode, oid)))));
    Ok(out)
}

/// `hash-object` (`args`) of each path, in order; [`FOREIGN`] for one it can't read. Names
/// `--stdin-paths` would rewrite (a leading `"` is C-unquoted, a trailing CR dropped) or split go
/// as arguments, as does every name once the batch failed (an unreadable file stops it).
fn hash_paths(dir: &Path, args: &[&str], ps: &[&str]) -> Vec<String> {
    let odd = |p: &&str| p.starts_with('"') || p.ends_with('\r') || p.contains('\n');
    let input: String = ps.iter().filter(|p| !odd(p)).map(|p| format!("{p}\n")).collect();
    let out = if input.is_empty() { String::new() } else { run_input(dir, &[args, &["--stdin-paths"]].concat(), Some(input.as_bytes())).unwrap_or_default() };
    let (ok, mut batch) = (out.lines().count() == input.lines().count(), out.lines());
    ps.iter().map(|p| match ok && !odd(p) {
        true => batch.next().unwrap_or_default().to_owned(),
        false => run(dir, &[args, &["--", p]].concat()).map_or(FOREIGN.into(), |o| o.trim().to_owned()),
    }).collect()
}

fn commit(dir: &Path, tree: &str, parents: &[&str], msg: &str) -> Res<String> {
    let mut a = vec!["commit-tree", "--no-gpg-sign", tree, "-m", msg];
    parents.iter().for_each(|p| a.extend(["-p", p]));
    let mut c = super::cmd(dir);
    for (k, v) in [("GIT_AUTHOR_NAME", "Glasir"), ("GIT_AUTHOR_EMAIL", "glasir@localhost"), ("GIT_COMMITTER_NAME", "Glasir"), ("GIT_COMMITTER_EMAIL", "glasir@localhost")] {
        c.env(k, v);
    }
    super::exec(c, dir, &a, None).map(|o| String::from_utf8_lossy(&o).trim().to_owned())
}

/// `--index-info` input: each path's entry, or (None) its removal.
fn info<'a>(entries: impl IntoIterator<Item = (&'a str, Option<&'a (u32, String)>)>, zero: &str) -> String {
    entries.into_iter().map(|(p, e)| e.map_or_else(|| format!("0 {zero}\t{p}\0"), |(m, o)| format!("{m:o} {o}\t{p}\0"))).collect()
}

/// Backs up everything uncommitted in `scope` as a stash-shaped commit kept by a new
/// `refs/glasir/backups/*` ref, before anything is destroyed. `like`: commits whose content needs
/// no backup (a path whose file and index entry are each in one of them is left out; None when
/// that leaves nothing); empty: the whole scope. `targets`: commits the op may check out besides.
/// Err: why a path can't be stored (the op must not run).
pub fn snap(dir: &Path, scope: &[String], like: &[String], targets: &[String], msg: &str) -> Res<Option<Snap>> {
    let base = head(dir).map_or_else(|| commit(dir, &empty_tree(dir)?, &[], "unborn"), Ok)?;
    scratch(dir, |tmp| scratch(dir, |tmp2| snap_in(dir, scope, like, targets, msg, &base, tmp, tmp2)))
}

#[allow(clippy::too_many_arguments)]
fn snap_in(dir: &Path, scope: &[String], like: &[String], targets: &[String], msg: &str, base: &str, tmp: &Path, tmp2: &Path) -> Res<Option<Snap>> {
    let zero = "0".repeat(base.len());
    let real = run(dir, &["rev-parse", "--path-format=absolute", "--git-path", "index"])?;
    if let Err(e) = std::fs::copy(real.trim(), tmp)
        && !gone(&e)
    {
        return Err(e.to_string());
    }
    let (index, unmerged) = blobs(on_index(dir, tmp, &["ls-files", "-s", "-z"], "")?);
    let mut all = leaves(dir, scope)?;
    let disk = hash(dir, &all.iter().chain(unmerged.iter().filter(|u| !all.contains(u))).cloned().collect::<Vec<_>>(), &index, true)?;
    let trees = like.iter().map(|c| tree(dir, c)).collect::<Res<Vec<_>>>()?;
    let others = targets.iter().map(|c| tree(dir, c)).collect::<Res<Vec<_>>>()?;
    // A nested repo or submodule survives a discard, unless a file goes at or above it (one the
    // index or a target tracks): that deletes it, .git and all, and git can't store one.
    let file = |p: &str| std::iter::once(&index).chain(&trees).chain(&others).any(|t| t.get(p).is_some_and(|e| e.0 != 0o160000));
    let repo = all.iter().filter(|p| disk[*p].as_ref().is_some_and(|e| e.0 == DIR)).map(|p| p.trim_end_matches('/')).find(|p| file(p) || ancestors(p).any(file));
    if let Some(p) = repo {
        return Err(format!("{p} is a git repository of its own, which this would delete; move it out of the way first"));
    }
    all.retain(|p| !disk[p].as_ref().is_some_and(|e| e.0 == DIR));
    if let Some(p) = all.iter().find(|p| disk[*p].as_ref().is_some_and(|e| e.1 == FOREIGN)) {
        return Err(format!("git can't store {p}"));
    }
    // A conflicted file at stage 0 as it is on disk, markers and all: an index tree has one stage.
    let fold = unmerged.iter().map(|u| (u.as_str(), None)).chain(unmerged.iter().filter_map(|u| Some((u.as_str(), Some(disk.get(u)?.as_ref()?)))));
    on_index(dir, tmp, &["update-index", "-z", "--index-info"], &info(fold, &zero))?;
    let i_tree = on_index(dir, tmp, &["write-tree"], "")?.trim().to_owned();
    // Compared as git would commit them too: a CRLF checkout (autocrlf) is no change.
    let differ: Vec<&String> = all.iter().filter(|p| disk[*p].as_ref().is_some_and(|d| d.0 != 0o120000 && trees.iter().any(|t| t.get(*p).is_some_and(|e| e != d)))).collect();
    let clean: HashMap<&String, String> = differ.iter().copied().zip(hash_paths(dir, &["hash-object"], &differ.iter().map(|p| p.as_str()).collect::<Vec<_>>())).collect();
    let in_like = |p: &String, e: Option<&(u32, String)>, raw: bool| trees.iter().any(|t| t.get(p) == e || (!raw && t.get(p).zip(e).is_some_and(|(a, b)| a.0 == b.0 && clean.get(p) == Some(&a.1))));
    let paths: Vec<String> = all.iter().filter(|p| like.is_empty() || unmerged.contains(*p) || !in_like(p, disk[*p].as_ref(), false) || !in_like(p, index.get(*p), true)).cloned().collect();
    if !like.is_empty() && paths.is_empty() {
        return Ok(None);
    }
    // Every leaf as it is on disk (what's left out included: a view of the rest reads them).
    let put = |idx: &Path, tracked: bool| -> Res<String> {
        let ps: Vec<(&str, Option<&(u32, String)>)> = all.iter().filter(|p| (index.contains_key(*p) || unmerged.contains(*p)) == tracked).filter(|p| tracked || disk[*p].is_some()).map(|p| (p.as_str(), disk[p].as_ref())).collect();
        on_index(dir, idx, &["update-index", "-z", "--index-info"], &info(ps.iter().copied(), &zero))?;
        // `--index-info` skips names git won't index (`git~1`, `.git.`) with only a warning.
        let have: HashSet<String> = z(on_index(dir, idx, &["ls-files", "-z"], "")?).into_iter().collect();
        match ps.iter().find(|(p, e)| e.is_some() && !have.contains(*p)) {
            Some((p, _)) => Err(format!("git can't store the name {p}")),
            None => on_index(dir, idx, &["write-tree"], "").map(|t| t.trim().to_owned()),
        }
    };
    let (w, u) = (put(tmp, true)?, put(tmp2, false)?);
    let unmerged = paths.iter().filter(|p| unmerged.contains(*p)).cloned().collect();
    kept(dir, &w, &i_tree, &u, base, msg).map(|(commit, refname)| Some(Snap { commit, refname, paths, unmerged }))
}

/// A stash-shaped commit (files `w`, index `i`, untracked files `u` on `base`) and the new
/// `refs/glasir/backups/*` ref that keeps it.
fn kept(dir: &Path, w: &str, i: &str, u: &str, base: &str, msg: &str) -> Res<(String, String)> {
    let c = commit(dir, w, &[base, &commit(dir, i, &[base], "index")?, &commit(dir, u, &[], "untracked files")?], msg)?;
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let refname = format!("{PREFIX}{ms}-{}-{}", std::process::id(), SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    run(dir, &["update-ref", &refname, &c, ""])?;
    Ok((c, refname))
}

/// The backup of a tree that was clean at commit `base`, made after the fact: files and index
/// as `base` has them, no untracked files. Covers no paths yet.
pub fn clean_snap(dir: &Path, base: &str, msg: &str) -> Res<Snap> {
    let t = format!("{base}^{{tree}}");
    let (commit, refname) = kept(dir, &t, &t, &empty_tree(dir)?, base, msg)?;
    Ok(Snap { commit, refname, paths: vec![], unmerged: vec![] })
}

/// What `scope` holds now, read without writing objects: a file that can't be read is [`FOREIGN`].
pub fn current(dir: &Path, scope: &[String]) -> Res<View> {
    let (ic, leaves) = (icase(dir), leaves(dir, scope)?);
    let (index, unmerged) = blobs(run(dir, &["ls-files", "-s", "-z"])?);
    let disk = hash(dir, &leaves, &index, false)?;
    let staged = raw(run(dir, &["diff-index", "--cached", "-z", "--no-renames", &head(dir).map_or_else(|| empty_tree(dir), Ok)?])?);
    let entry = |p: &String| {
        let (name, u) = (p.trim_end_matches('/'), unmerged.contains(p.trim_end_matches('/')));
        (key(name, ic), Entry { name: name.into(), disk: disk[p].clone(), idx: staged.get(name).cloned().filter(|_| !u), unmerged: u })
    };
    Ok(leaves.iter().map(entry).filter(|(_, e)| e.disk.is_some() || e.idx.is_some() || e.unmerged).collect())
}

/// What backup `s` holds for `scope`: paths equal to, under or above a scope path. Err: the
/// backup is gone (pruned).
pub fn view(dir: &Path, s: &Snap, scope: &[String]) -> Res<View> {
    let ic = icase(dir);
    let keys: Vec<String> = scope.iter().map(|p| key(p.trim_end_matches('/'), ic)).collect();
    let inside = related(&keys);
    let mut v = View::new();
    let absent = |p: &str| Entry { name: p.into(), disk: None, idx: None, unmerged: false };
    for (p, e) in tree(dir, &s.commit)?.into_iter().chain(tree(dir, &format!("{}^3", s.commit))?) {
        if inside(&key(&p, ic)) {
            v.insert(key(&p, ic), Entry { disk: Some(e), ..absent(&p) });
        }
    }
    for (p, e) in raw(run(dir, &["diff-tree", "-r", "-z", "--no-renames", &format!("{}^1", s.commit), &format!("{}^2", s.commit)])?) {
        if inside(&key(&p, ic)) {
            v.entry(key(&p, ic)).or_insert(absent(&p)).idx = Some(e);
        }
    }
    for u in &s.unmerged {
        let e = v.entry(key(u, ic)).or_insert(absent(u));
        (e.unmerged, e.idx) = (true, None);
    }
    Ok(v)
}

/// Puts `files` on disk: a gone entry's file is removed (with the folders that leaves empty; a
/// submodule's folder only while empty), the others written with their modes: `raw` bytes as
/// backed up, else as git checks them out (filters, line endings). Then each of `index`'s index
/// entry: its `idx`, else `head`'s (or none); conflict stages go.
pub fn write(dir: &Path, files: &[Entry], index: &[Entry], head: &str, raw: bool) -> Res<()> {
    for e in files.iter().filter(|e| e.disk.is_none()) {
        let p = dir.join(&e.name);
        match std::fs::remove_file(&p) {
            Err(_) if lstat(dir, &e.name).is_ok_and(|m| m.is_dir()) => drop(std::fs::remove_dir(&p)),
            Err(err) if !gone(&err) => return Err(format!("{}: {err}", e.name)),
            _ => ancestors(&e.name).rev().take_while(|a| std::fs::remove_dir(dir.join(a)).is_ok()).for_each(drop),
        }
    }
    let put: Vec<(&str, &(u32, String))> = files.iter().filter_map(|e| Some((e.name.as_str(), e.disk.as_ref()?))).collect();
    // Raw: no line-ending conversion, filters or encodings (links and submodules have none).
    let (bytes, put): (Vec<_>, Vec<_>) = put.into_iter().partition(|(_, (m, _))| raw && matches!(m, 0o100644 | 0o100755));
    put_raw(dir, &bytes)?;
    if !put.is_empty() {
        scratch(dir, |tmp| {
            on_index(dir, tmp, &["update-index", "-z", "--index-info"], &info(put.iter().map(|&(p, e)| (p, Some(e))), &"0".repeat(head.len())))?;
            on_index(dir, tmp, &["checkout-index", "-f", "-z", "--stdin"], &put.iter().map(|(p, _)| format!("{p}\0")).collect::<String>())
        })?;
    }
    // Each index entry, conflict stages cleared: its `idx` (mode 0: none), else `head`'s.
    if !index.is_empty() {
        let (tip, zero) = (tree(dir, head)?, "0".repeat(head.len()));
        let set = index.iter().filter_map(|e| Some((e.name.as_str(), Some(e.idx.as_ref().map_or(tip.get(&e.name), |i| Some(i).filter(|i| i.0 != 0))?))));
        let input = info(index.iter().map(|e| (e.name.as_str(), None)), &zero) + &info(set, &zero);
        run_input(dir, &["update-index", "-z", "--index-info"], Some(input.as_bytes()))?;
    }
    if !files.is_empty() || !index.is_empty() {
        let _ = run(dir, &["update-index", "-q", "--refresh"]);
    }
    Ok(())
}

fn put_raw(dir: &Path, put: &[(&str, &(u32, String))]) -> Res<()> {
    if put.is_empty() {
        return Ok(());
    }
    let (start, args) = (std::time::Instant::now(), ["cat-file", "--batch", "--buffer"]);
    let mut child = super::spawn(super::cmd(dir).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())).map_err(|e| format!("could not run git: {e}"))?;
    let (mut stdin, input) = (child.stdin.take().unwrap(), put.iter().map(|(_, (_, o))| format!("{o}\n")).collect::<String>());
    std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
    let res = put.iter().try_for_each(|&(p, e)| {
        let (mut head, oid) = (String::new(), &e.1);
        let size = out.read_line(&mut head).ok().and_then(|_| head.strip_prefix(oid.as_str())?.strip_prefix(" blob ")?.trim_end().parse::<u64>().ok());
        let size = size.ok_or_else(|| format!("{p}: git can't read its backup ({oid})"))?;
        for a in ancestors(p).map(|a| dir.join(a)) {
            if !std::fs::symlink_metadata(&a).is_ok_and(|m| m.is_dir()) {
                let _ = std::fs::remove_file(&a);
                std::fs::create_dir(&a).map_err(|e| format!("{p}: {e}"))?;
            }
        }
        let f = dir.join(p);
        let _ = std::fs::remove_file(&f).or_else(|_| std::fs::remove_dir_all(&f));
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, if e.0 == 0o100755 { 0o777 } else { 0o666 });
        let n = o.open(&f).and_then(|mut w| std::io::copy(&mut (&mut out).take(size), &mut w)).map_err(|e| format!("{p}: {e}"))?;
        (n == size && out.read_exact(&mut [0u8]).is_ok()).then_some(()).ok_or_else(|| format!("{p}: git stopped reading its backup"))
    });
    drop(out);
    let code = child.wait().ok().and_then(|s| s.code());
    super::log::record(dir, "git", &args, start, code, "");
    res
}

/// A ref move's files, per path like `reset <mode>` from commit `from` to `tip`, but only where
/// the path has no uncommitted change, none under or above it, and none at, under or above
/// `skip` (paths undo writes itself): `--mixed` moves index entries, `--keep` files too,
/// `--soft` nothing. Never refuses: returns the paths that kept local changes and so differ
/// from `tip`.
pub fn move_files(dir: &Path, from: &str, tip: &str, mode: &str, skip: &[Entry]) -> Res<Vec<String>> {
    let ic = icase(dir);
    let keys: Vec<String> = skip.iter().map(|e| key(&e.name, ic)).collect();
    let skipped = related(&keys);
    let d: Vec<String> = z(run(dir, &["diff-tree", "-r", "-z", "--name-only", "--no-renames", from, tip])?).into_iter().filter(|p| !skipped(&key(p, ic))).collect();
    if mode == "--soft" || d.is_empty() {
        return Ok(vec![]);
    }
    let (index, unmerged) = blobs(run(dir, &["ls-files", "-s", "-z"])?);
    let mut staged: HashSet<String> = z(run(dir, &["diff", "--cached", "--name-only", "-z", "--no-renames", from])?).into_iter().chain(unmerged).collect();
    let mut dirty = HashSet::new();
    if mode == "--keep" {
        dirty.extend(z(run(dir, &["diff", "--name-only", "-z", "--no-renames"])?).into_iter().chain(flagged(dir)?));
    }
    // A path under or above a staged or changed one: writing it (its index entry too) would
    // remove that one, so it's left as it is.
    let held: HashSet<String> = staged.iter().chain(&dirty).map(|p| key(p, ic)).collect();
    let near = related(&held);
    staged.extend(d.iter().filter(|p| !held.contains(&key(p, ic)) && near(&key(p, ic))).cloned());
    if mode == "--keep" {
        // Untracked or ignored files at a path, under it, or where a folder above it goes.
        let found: Vec<String> = leaves(dir, &d)?.into_iter().filter(|l| !index.contains_key(l) && lstat(dir, l).is_ok()).map(|l| key(l.trim_end_matches('/'), ic)).collect();
        let near = related(&found);
        dirty.extend(d.iter().filter(|p| near(&key(p, ic))).cloned());
    }
    let files = tree(dir, tip)?;
    let rest = d.iter().filter(|p| !staged.contains(*p));
    let full: Vec<Entry> = rest.clone().filter(|p| mode == "--keep" && !dirty.contains(*p)).map(|p| Entry { name: p.clone(), disk: files.get(p).cloned(), idx: None, unmerged: false }).collect();
    // Index entries only: the files stay as they are.
    let only: Vec<Entry> = rest.filter(|p| mode != "--keep" || dirty.contains(*p)).map(|p| Entry { name: p.clone(), disk: None, idx: None, unmerged: false }).collect();
    write(dir, &full, &[&full[..], &only].concat(), tip, false)?;
    let differ: HashSet<String> = z(run(dir, &["diff", "--name-only", "-z", "--no-renames"])?).into_iter().chain(z(run(dir, &["diff", "--cached", "--name-only", "-z", "--no-renames", tip])?)).collect();
    Ok(d.into_iter().filter(|p| (staged.contains(p) || dirty.contains(p)) && (differ.contains(p) || lstat(dir, p).is_ok_and(|m| m.is_dir()))).collect())
}

/// Index entries marked assume-unchanged or skip-worktree: `git diff` never shows their edits.
pub fn flagged(dir: &Path) -> Res<Vec<String>> {
    let v = z(run(dir, &["ls-files", "-v", "-z"])?);
    Ok(v.into_iter().filter_map(|l| l.split_once(' ').filter(|(t, _)| t.starts_with(|c: char| c.is_ascii_lowercase() || c == 'S')).map(|(_, p)| p.to_owned())).collect())
}

/// Moves the refs in `moved` (and `HEAD`) from `cur` to `to` in one transaction: all or none.
/// Never touches files; the checked-out branch's move writes HEAD's reflog too.
pub fn move_refs(dir: &Path, cur: &Snapshot, to: &Snapshot, moved: &[String], msg: &str) -> Res<()> {
    let tx: String = moved
        .iter()
        .filter_map(|r| match (cur.refs.get(r), to.refs.get(r)) {
            (Some(c), Some(t)) if c != t => Some(format!("update {r} {t} {c}\n")),
            (None, Some(t)) => Some(format!("create {r} {t}\n")),
            (Some(c), None) => Some(format!("delete {r} {c}\n")),
            _ => None,
        })
        .collect();
    if !tx.is_empty() {
        run_input(dir, &["update-ref", "-m", msg, "--stdin"], Some(format!("start\n{tx}prepare\ncommit\n").as_bytes()))?;
    }
    if moved.iter().any(|r| r == "HEAD") && cur.head != to.head {
        match to.head.starts_with("refs/") {
            true => run(dir, &["symbolic-ref", "-m", msg, "HEAD", &to.head])?,
            false => run(dir, &["update-ref", "-m", msg, "--no-deref", "HEAD", &to.head])?,
        };
    }
    Ok(())
}

/// Keeps backup `commit` in the stash list as `msg`; Glasir never drops or prunes these.
pub fn keep(dir: &Path, commit: &str, msg: &str) -> Res<()> {
    run(dir, &["stash", "store", "-m", msg, commit]).map(drop)
}

/// Deletes these backup refs (only ever `refs/glasir/backups/*`).
pub fn drop_backups(dir: &Path, refs: &[String]) {
    let tx: String = refs.iter().filter(|r| r.starts_with(PREFIX)).map(|r| format!("delete {r}\n")).collect();
    if !tx.is_empty() {
        let _ = run_input(dir, &["update-ref", "--stdin"], Some(tx.as_bytes()));
    }
}

/// Whether process `pid` still runs; when unsure, it does.
fn running(pid: u32) -> bool {
    #[cfg(unix)]
    return i32::try_from(pid).is_ok_and(|p| p > 0 && (unsafe { libc::kill(p, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)));
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError};
        let h = windows_sys::Win32::System::Threading::OpenProcess(windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        // An exited process someone still holds a handle to counts too: its backups stay longer.
        (!h.is_null() && CloseHandle(h) != 0) || GetLastError() == ERROR_ACCESS_DENIED
    }
}

/// Deletes the backup refs older than `days` at `now` (seconds since 1970) whose step is gone
/// with the session that made it (the pid in their name runs no more): never one a running
/// Glasir may still undo (another tab, a linked worktree, another window). Never touches
/// anything outside `refs/glasir/backups/`.
pub fn prune(dir: &Path, now: u64, days: u64) {
    let out = run(dir, &["for-each-ref", "--format=%(refname)%00%(committerdate:unix)", PREFIX]).unwrap_or_default();
    let gone = |r: &str| r[PREFIX.len()..].split('-').nth(1).and_then(|p| p.parse().ok()).is_some_and(|p| !running(p));
    let old: Vec<String> = out.lines().filter_map(|l| l.split_once('\0')).filter(|(r, t)| t.parse::<u64>().is_ok_and(|t| t + days * 86_400 < now) && gone(r)).map(|(r, _)| r.to_owned()).collect();
    drop_backups(dir, &old);
}
