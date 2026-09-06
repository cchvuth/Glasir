//! Undo snapshots: the local branches, tags and HEAD ([`Snapshot`]), and durable file
//! backups ([`Snap`]: a stash-shaped commit kept by a `refs/glasir/backups/*` ref).

use super::{Res, run, run_input};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

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

/// Runs `f` on a scratch index of its own (no shared lock; a missing file is an empty index).
fn scratch<T>(dir: &Path, f: impl FnOnce(&Path) -> Res<T>) -> Res<T> {
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
/// folder now). A nested repo or submodule is `(DIR, FOREIGN)`, a file that can't be read
/// (or named, with a newline or as non-UTF-8) `(mode, FOREIGN)`. Without exec bits to read
/// (Windows, `core.filemode=false`) a file keeps its index mode. Git streams them, any size.
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
                let x = m.is_file() && filemode;
                let mode = if x || (!filemode && index.get(p).is_some_and(|e| e.0 == 0o100755)) { 0o100755 } else { 0o100644 };
                if !p.contains(['\n', '\u{FFFD}']) {
                    files.push((p, mode));
                    continue;
                }
                Some((mode, FOREIGN.into()))
            }
        };
        out.insert(p.clone(), e);
    }
    let args = [&["hash-object", "--no-filters", "--stdin-paths"], w].concat();
    let one = |ps: &[&(&String, u32)]| run_input(dir, &args, Some(ps.iter().map(|(p, _)| format!("{p}\n")).collect::<String>().as_bytes())).map(|o| o.lines().map(String::from).collect::<Vec<_>>());
    // One that can't be read stops the batch: then one at a time.
    let oids = one(&files.iter().collect::<Vec<_>>()).ok().filter(|o| o.len() == files.len());
    let oids = oids.unwrap_or_else(|| files.iter().map(|f| one(&[f]).map_or(FOREIGN.into(), |o| o.concat())).collect());
    out.extend(files.into_iter().zip(oids).map(|((p, mode), oid)| (p.clone(), Some((mode, oid)))));
    Ok(out)
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
/// that leaves nothing); empty: the whole scope. Err: why a path can't be stored (the op must not run).
pub fn snap(dir: &Path, scope: &[String], like: &[String], msg: &str) -> Res<Option<Snap>> {
    let base = head(dir).map_or_else(|| commit(dir, &empty_tree(dir)?, &[], "unborn"), Ok)?;
    scratch(dir, |tmp| scratch(dir, |tmp2| snap_in(dir, scope, like, msg, &base, tmp, tmp2)))
}

fn snap_in(dir: &Path, scope: &[String], like: &[String], msg: &str, base: &str, tmp: &Path, tmp2: &Path) -> Res<Option<Snap>> {
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
    // A nested repo or submodule is never removed by a discard: nothing to keep.
    all.retain(|p| !disk[p].as_ref().is_some_and(|e| e.0 == DIR));
    if let Some(p) = all.iter().find(|p| disk[*p].as_ref().is_some_and(|e| e.1 == FOREIGN)) {
        return Err(format!("git can't store {p}"));
    }
    // A conflicted file at stage 0 as it is on disk, markers and all: an index tree has one stage.
    let fold = unmerged.iter().map(|u| (u.as_str(), None)).chain(unmerged.iter().filter_map(|u| Some((u.as_str(), Some(disk.get(u)?.as_ref()?)))));
    on_index(dir, tmp, &["update-index", "-z", "--index-info"], &info(fold, &zero))?;
    let i_tree = on_index(dir, tmp, &["write-tree"], "")?.trim().to_owned();
    let trees = like.iter().map(|c| tree(dir, c)).collect::<Res<Vec<_>>>()?;
    // Compared as git would commit them too: a CRLF checkout (autocrlf) is no change.
    let differ: Vec<&String> = all.iter().filter(|p| disk[*p].as_ref().is_some_and(|d| d.0 != 0o120000 && trees.iter().any(|t| t.get(*p).is_some_and(|e| e != d)))).collect();
    let list: String = differ.iter().map(|p| format!("{p}\n")).collect();
    let clean: HashMap<&String, String> = match differ.is_empty() {
        true => HashMap::new(),
        false => differ.iter().copied().zip(run_input(dir, &["hash-object", "--stdin-paths"], Some(list.as_bytes())).unwrap_or_default().lines().map(String::from)).collect(),
    };
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
    let c = commit(dir, &w, &[base, &commit(dir, &i_tree, &[base], "index")?, &commit(dir, &u, &[], "untracked files")?], msg)?;
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let refname = format!("{PREFIX}{ms}-{}-{}", std::process::id(), SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    run(dir, &["update-ref", &refname, &c, ""])?;
    let unmerged = paths.iter().filter(|p| unmerged.contains(*p)).cloned().collect();
    Ok(Some(Snap { commit: c, refname, paths, unmerged }))
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

/// Puts `plan` on disk: a gone entry's file is removed (with the folders that leaves empty),
/// the others written with their raw bytes and modes. Then each one's index entry: its `idx`,
/// else `head`'s (or none); conflict stages go.
pub fn write(dir: &Path, plan: &[Entry], head: &str) -> Res<()> {
    for e in plan.iter().filter(|e| e.disk.is_none()) {
        match std::fs::remove_file(dir.join(&e.name)) {
            Err(err) if !gone(&err) => return Err(format!("{}: {err}", e.name)),
            _ => ancestors(&e.name).rev().take_while(|a| std::fs::remove_dir(dir.join(a)).is_ok()).for_each(drop),
        }
    }
    let put: Vec<(&str, Option<&(u32, String)>)> = plan.iter().filter_map(|e| Some((e.name.as_str(), Some(e.disk.as_ref()?)))).collect();
    // Raw bytes as backed up: no line-ending conversion or filters (`--attr-source`: git 2.40).
    let attr = format!("--attr-source={}", empty_tree(dir)?);
    let new = super::env::parse_version(&run(dir, &["--version"])?).is_some_and(|v| v >= (2, 40));
    let args: Vec<&str> = ["-c", "core.autocrlf=false"].into_iter().chain(new.then_some(attr.as_str())).chain(["checkout-index", "-f", "-z", "--stdin"]).collect();
    if !put.is_empty() {
        scratch(dir, |tmp| {
            on_index(dir, tmp, &["update-index", "-z", "--index-info"], &info(put.iter().copied(), &"0".repeat(head.len())))?;
            on_index(dir, tmp, &args, &put.iter().map(|(p, _)| format!("{p}\0")).collect::<String>())
        })?;
    }
    set_index(dir, plan, head)
}

/// Each entry's index entry, conflict stages cleared: its `idx` (mode 0: none), else `head`'s.
fn set_index(dir: &Path, plan: &[Entry], head: &str) -> Res<()> {
    if plan.is_empty() {
        return Ok(());
    }
    let (tip, zero) = (tree(dir, head)?, "0".repeat(head.len()));
    let set = plan.iter().filter_map(|e| Some((e.name.as_str(), Some(e.idx.as_ref().map_or(tip.get(&e.name), |i| Some(i).filter(|i| i.0 != 0))?))));
    let input = info(plan.iter().map(|e| (e.name.as_str(), None)), &zero) + &info(set, &zero);
    run_input(dir, &["update-index", "-z", "--index-info"], Some(input.as_bytes()))?;
    let _ = run(dir, &["update-index", "-q", "--refresh"]);
    Ok(())
}

/// A ref move's files, per path like `reset <mode>` from commit `from` to `tip`, but only where
/// the path has no uncommitted change, and none at, under or above `skip` (paths undo writes
/// itself): `--mixed` moves index entries, `--keep` files too, `--soft` nothing. Never refuses:
/// returns the paths that kept local changes and so differ from `tip`.
pub fn move_files(dir: &Path, from: &str, tip: &str, mode: &str, skip: &[Entry]) -> Res<Vec<String>> {
    let ic = icase(dir);
    let keys: Vec<String> = skip.iter().map(|e| key(&e.name, ic)).collect();
    let skipped = related(&keys);
    let d: Vec<String> = z(run(dir, &["diff-tree", "-r", "-z", "--name-only", "--no-renames", from, tip])?).into_iter().filter(|p| !skipped(&key(p, ic))).collect();
    if mode == "--soft" || d.is_empty() {
        return Ok(vec![]);
    }
    let (index, unmerged) = blobs(run(dir, &["ls-files", "-s", "-z"])?);
    let staged: HashSet<String> = z(run(dir, &["diff", "--cached", "--name-only", "-z", "--no-renames", from])?).into_iter().chain(unmerged).collect();
    let mut dirty = HashSet::new();
    if mode == "--keep" {
        dirty.extend(z(run(dir, &["diff", "--name-only", "-z", "--no-renames"])?));
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
    write(dir, &full, tip)?;
    set_index(dir, &only, tip)?;
    let differ: HashSet<String> = z(run(dir, &["diff", "--name-only", "-z", "--no-renames"])?).into_iter().chain(z(run(dir, &["diff", "--cached", "--name-only", "-z", "--no-renames", tip])?)).collect();
    Ok(d.into_iter().filter(|p| (staged.contains(p) || dirty.contains(p)) && (differ.contains(p) || lstat(dir, p).is_ok_and(|m| m.is_dir()))).collect())
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

/// Deletes the backup refs older than `days` at `now` (seconds since 1970): those whose undo
/// step went with the app or the history. Never touches anything outside `refs/glasir/backups/`.
pub fn prune(dir: &Path, now: u64, days: u64) {
    let out = run(dir, &["for-each-ref", "--format=%(refname)%00%(committerdate:unix)", PREFIX]).unwrap_or_default();
    let old: Vec<String> = out.lines().filter_map(|l| l.split_once('\0')).filter(|(_, t)| t.parse::<u64>().is_ok_and(|t| t + days * 86_400 < now)).map(|(r, _)| r.to_owned()).collect();
    drop_backups(dir, &old);
}
