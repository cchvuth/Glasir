//! Undo/redo: ref snapshots for ops that move refs, backups of discarded
//! work so a discard can be undone, and reverting ops for config changes.

use super::op::{Backup, Done, Op, Undo};
use super::task::Task;
use super::RepoTab;
use crate::git::{self, Res, Snapshot};
use std::path::Path;

/// Discarded work: a `git stash create` commit plus copies of deleted untracked files.
#[derive(Clone)]
pub(super) struct Saved {
    /// `git stash create` commit holding the tracked files' contents.
    pub(super) stash: Option<String>,
    pub(super) tracked: Vec<String>,
    /// Paths whose index differs from HEAD (some only there: "MM" back to HEAD, "AD"); `Backup::All`: every one.
    pub(super) staged: Vec<String>,
    /// `Backup::Paths`: paths that were conflicted; undo brings the conflict back.
    pub(super) unmerged: Vec<String>,
    /// Untracked files' bytes, and tracked files' exact bytes (line endings) when they fit.
    pub(super) files: Vec<(String, Vec<u8>)>,
    /// `Backup::Overwrite`: put `files` back even if the path exists.
    pub(super) overwrite: bool,
    /// Untracked paths that didn't exist (once sealed: that the op created), removed on undo.
    pub(super) absent: Vec<String>,
    /// A real `stash create` commit: undo also restores the `staged` paths from its index (`^2`).
    pub(super) index: bool,
    /// What each path held right after the op (`seal`): undo refuses once one changed.
    /// Shared with the undo job, which re-seals what it restored so a half-done undo can be retried.
    pub(super) after: std::sync::Arc<std::sync::Mutex<Vec<(String, Option<u64>)>>>,
}

impl Saved {
    /// Records the paths' state after the discard ran.
    pub(super) fn seal(&mut self, dir: &Path) {
        self.absent.retain(|p| std::fs::symlink_metadata(dir.join(p)).is_ok());
        *self.after.lock().unwrap() = self.paths().into_iter().map(|p| (p.clone(), fingerprint(dir, p))).collect();
    }

    /// Every path the backup restores.
    pub(super) fn paths(&self) -> Vec<&String> {
        let mut paths: Vec<&String> = self.tracked.iter().chain(&self.staged).chain(&self.absent).chain(self.files.iter().map(|(p, _)| p)).collect();
        paths.sort_unstable();
        paths.dedup();
        paths
    }
}

/// A hash of what's at `p` (None = nothing), to tell whether it changed. A file (or symlink)
/// where a folder above `p` goes counts as `p`'s: restoring `p` would replace it.
pub(super) fn fingerprint(dir: &Path, p: &str) -> Option<u64> {
    let mut above = dir.to_path_buf();
    for part in Path::new(p).parent().into_iter().flat_map(Path::components) {
        above.push(part);
        if std::fs::symlink_metadata(&above).is_ok_and(|m| !m.is_dir()) {
            return content(&above).map(|h| !h);
        }
    }
    content(&dir.join(p))
}

fn content(path: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path).ok()?;
    let mut h = std::hash::DefaultHasher::new();
    (meta.is_symlink(), std::fs::read_link(path).ok(), meta.is_file()).hash(&mut h);
    // Streamed: a multi-GB file must not be read into memory.
    if let (true, Ok(mut f)) = (meta.is_file(), std::fs::File::open(path)) {
        let mut buf = vec![0; 1 << 16];
        loop {
            match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => h.write(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Some(0),
            }
        }
    }
    Some(h.finish())
}

pub(super) enum UndoKind {
    /// `autostash`: the op carried uncommitted changes along; undo/redo carries them back.
    /// `saved`: backups (a hard reset's, a split's), oldest first: restored newest first once the
    /// refs are back (undo only).
    /// `on_remote`: what [`on_remotes`] said when the op finished (a pull's commits); more later = pushed since.
    Refs { before: Snapshot, after: Snapshot, mode: Undo, autostash: Option<String>, saved: Vec<Saved>, on_remote: usize },
    Discard(Saved),
    /// A config-only op (remotes) and the op that reverts it.
    Op { undo: Box<super::Op>, redo: Box<super::Op> },
}

pub(super) struct UndoEntry {
    pub(super) label: String,
    pub(super) kind: UndoKind,
    /// Unique, increasing: a toast's Undo button stays live while its step is on top.
    id: u64,
}

impl UndoEntry {
    pub(super) fn new(label: String, kind: UndoKind) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        UndoEntry { label, kind, id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) }
    }
}

/// Backups outlive their ref step (dropped, or folded into one): each a discard of its own.
pub(super) fn keep_backups(steps: Vec<UndoEntry>) -> impl Iterator<Item = UndoEntry> {
    steps.into_iter().flat_map(|e| match e.kind {
        UndoKind::Refs { saved, .. } if !saved.is_empty() => saved.into_iter().map(|s| UndoEntry::new(e.label.clone(), UndoKind::Discard(s))).collect(),
        _ => vec![e],
    })
}

/// How many of the commits that moving the branches from `after` back to `before` takes off
/// a remote-tracking branch has.
pub(super) fn on_remotes(dir: &Path, before: &Snapshot, after: &Snapshot) -> usize {
    let tips = |s: &Snapshot, not: &str| {
        let heads = s.refs.iter().filter(|(r, _)| r.starts_with("refs/heads/")).map(|(_, o)| o);
        heads.chain(Some(&s.head).filter(|h| !h.starts_with("refs/"))).map(|o| format!("{not}{o}\n")).collect::<String>()
    };
    let input = tips(after, "") + &tips(before, "^");
    let count = |extra: &[&str]| {
        let args: Vec<&str> = ["rev-list", "--count", "--stdin"].into_iter().chain(extra.iter().copied()).collect();
        git::run_input(dir, &args, Some(input.as_bytes())).ok().and_then(|n| n.trim().parse::<usize>().ok()).unwrap_or(0)
    };
    count(&[]).saturating_sub(count(&["--not", "--remotes"]))
}

/// Pseudo-command of a confirmed undo: the step's id, so it only undoes the step that was asked about.
pub(super) const UNDO_PUSHED: &str = "__undo_pushed__";

pub(super) fn split_z(s: String) -> Vec<String> {
    s.split('\0').filter(|p| !p.is_empty()).map(String::from).collect()
}

/// Back up everything a discard will destroy. Any failure aborts the discard.
pub(super) fn save_worktree(dir: &Path, b: &Backup) -> Res<Saved> {
    let refuse = |why: String| format!("Nothing was discarded: couldn't back it up first ({why}).");
    // Checked first: a refused reset leaves no backup behind as a bogus undo step.
    if let Backup::Reset(_, want) = b {
        if super::rebase::head_state(dir) != *want {
            return Err("The branch changed since this was prepared, so nothing was reset. Try again.".into());
        }
    }
    let (tracked, untracked): (Vec<String>, Vec<String>) = match b {
        Backup::All | Backup::Reset(..) => (
            // No HEAD yet (unborn branch): every index entry is a staged new file.
            split_z(git::run(dir, &["diff", "--name-only", "-z", "HEAD"]).or_else(|_| git::run(dir, &["ls-files", "-z"])).map_err(refuse)?),
            // `reset --hard` overwrites ignored files in its way too (below).
            split_z(git::run(dir, &["ls-files", "-z", "--others", "--exclude-standard"]).map_err(refuse)?),
        ),
        Backup::Paths(ps) | Backup::Overwrite(ps) => ps.iter().cloned().partition(|p| {
            git::run(dir, &["--literal-pathspecs", "ls-files", "-z", "--", p]).is_ok_and(|o| !o.is_empty())
        }),
    };
    let untracked = match b {
        Backup::Reset(rev, _) => in_the_way(dir, &[rev.as_str()]).map_err(refuse)?,
        // Ignored files where the reset (HEAD, or a rebase abort's branch) puts a file are lost too.
        Backup::All => {
            let head = git::run(dir, &["rev-parse", "-q", "--verify", "HEAD"]).ok();
            let revs: Vec<String> = head.into_iter().chain(rebase_orig(dir)).map(|r| r.trim().to_owned()).collect();
            let mut all = untracked;
            all.extend(in_the_way(dir, &revs.iter().map(String::as_str).collect::<Vec<_>>()).map_err(refuse)?);
            all.sort_unstable();
            all.dedup();
            all
        }
        _ => untracked,
    };
    // Unborn branch: `tracked` already lists every index entry.
    let mut staged = git::run(dir, &["diff", "--cached", "--name-only", "-z", "--no-renames", "HEAD"]).map(split_z).unwrap_or_default();
    if let Backup::Paths(_) | Backup::Overwrite(_) = b {
        staged.retain(|p| tracked.contains(p));
    }
    let unmerged = match b {
        Backup::All | Backup::Reset(..) => vec![],
        _ => split_z(git::run(dir, &["diff", "--name-only", "-z", "--diff-filter=U"]).map_err(refuse)?).into_iter().filter(|p| tracked.contains(p)).collect(),
    };
    // git prints raw bytes; a name that isn't UTF-8 decodes lossily and would match no file.
    if let Some(p) = tracked.iter().chain(&staged).chain(&untracked).find(|p| p.contains('\u{FFFD}')) {
        return Err(refuse(format!("{p}: file name isn't valid UTF-8")));
    }
    // A name git won't index can't go in a stash entry, so an undo couldn't keep it safe.
    if let Some(p) = unindexable(dir, &untracked).map_err(refuse)? {
        return Err(refuse(format!("{p}: git can't store this file name")));
    }
    // `stash create` fails before the first commit and while the index has conflicts:
    // then the tracked files' bytes are copied like untracked ones (`stash` = None).
    let created = (!tracked.is_empty() || !staged.is_empty()).then(|| git::run(dir, &["stash", "create"]).ok()).flatten();
    let index = created.as_ref().is_some_and(|s| !s.trim().is_empty());
    let stash = created.and_then(|s| {
        // No tracked file changed yet: HEAD already holds their contents (e.g. a
        // restore from history about to overwrite a clean file).
        Some(s.trim().to_owned()).filter(|s| !s.is_empty()).or_else(|| git::run(dir, &["rev-parse", "-q", "--verify", "HEAD"]).ok().map(|h| h.trim().to_owned()))
    });
    // Copies live in memory, capped so a stray huge file can't exhaust RAM.
    let mut budget: u64 = 256 << 20;
    let (mut files, mut absent) = (vec![], vec![]);
    let copied: std::collections::HashSet<&String> = untracked.iter().collect();
    // With a stash, tracked copies are optional: they only keep the exact bytes, which the
    // stash's clean/smudge round trip may change (autocrlf, eol, filters).
    let optional = tracked.iter().filter(|p| !copied.contains(p)).map(|p| (p, stash.is_some()));
    for (p, optional) in untracked.iter().map(|p| (p, false)).chain(optional) {
        let path = dir.join(p);
        let meta = match std::fs::symlink_metadata(&path) {
            // Not there yet (a file restored from history): nothing to lose.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                absent.extend(copied.contains(p).then(|| p.clone()));
                continue;
            }
            m => m.map_err(|e| refuse(format!("{p}: {e}")))?,
        };
        if optional && (!meta.is_file() || meta.len() > budget) {
            continue;
        }
        if !meta.is_file() {
            return Err(refuse(format!("{p} is not a regular file")));
        }
        budget = budget.checked_sub(meta.len()).ok_or_else(|| refuse(format!("{p} is too large")))?;
        files.push((p.clone(), std::fs::read(&path).map_err(|e| refuse(format!("{p}: {e}")))?));
    }
    Ok(Saved { stash, tracked, staged, unmerged, files, overwrite: matches!(b, Backup::Overwrite(_)), absent, index, after: Default::default() })
}

/// The branch tip a stopped rebase's `--abort` goes back to.
pub(super) fn rebase_orig(dir: &Path) -> Option<String> {
    let g = git::git_dir(dir).ok()?;
    ["rebase-merge", "rebase-apply"].iter().find_map(|d| std::fs::read_to_string(g.join(d).join("orig-head")).ok())
}

/// Untracked files, ignored ones too, that `reset --hard` to one of `revs` overwrites: where a
/// rev has an entry at the path, or a file above it. Matched ignoring case: a case-insensitive
/// disk has `Readme` at `README` (on another disk it only backs up a few more files).
pub(super) fn in_the_way(dir: &Path, revs: &[&str]) -> Res<Vec<String>> {
    let untracked = split_z(git::run(dir, &["ls-files", "-z", "--others", "--"])?);
    // Lowercased path -> is a file.
    let mut tree = std::collections::HashMap::new();
    for rev in revs.iter().filter(|_| !untracked.is_empty()) {
        for l in split_z(git::run(dir, &["ls-tree", "-r", "-t", "-z", rev])?) {
            if let Some((m, p)) = l.split_once('\t') {
                *tree.entry(p.to_lowercase()).or_insert(false) |= !m.contains(" tree ");
            }
        }
    }
    let hit = |p: &str| {
        let p = p.to_lowercase();
        tree.contains_key(&p) || p.match_indices('/').any(|(i, _)| tree.get(&p[..i]) == Some(&true))
    };
    Ok(untracked.into_iter().filter(|p| hit(p)).collect())
}

/// Same path, ignoring case: a case-insensitive disk (macOS, Windows) has `notes` at `Notes`.
/// Applied everywhere: on a case-sensitive disk it only keeps a few more backups in the stash list.
fn same_path(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Backed-up paths that hold work made since their discard, which restoring the backups would
/// overwrite. Fine only when gone, or tracked under that exact name and clean (committed,
/// like a split's parts): anything else there (untracked, another case, a folder) is kept.
fn overwrites(dir: &Path, saved: &[Saved]) -> Res<Vec<String>> {
    let mut changed: Vec<String> = saved.iter().flat_map(|s| s.after.lock().unwrap().iter().filter(|(p, f)| fingerprint(dir, p) != *f).map(|(p, _)| p.clone()).collect::<Vec<_>>()).collect();
    // Gone is fine; a file where its folder goes is not.
    changed.retain(|p| fingerprint(dir, p).is_some());
    if changed.is_empty() {
        return Ok(changed);
    }
    changed.sort_unstable();
    changed.dedup();
    // ponytail: paths on the command line (`status` takes no pathspec file): only those changed since,
    // so few; chunk them if a huge set ever hits the argument limit.
    let with = |cmd: &[&'static str]| {
        let mut args = vec!["--literal-pathspecs"];
        args.extend(cmd);
        args.push("--");
        args.extend(changed.iter().map(String::as_str));
        git::run(dir, &args).map(split_z)
    };
    let tracked = with(&["ls-files", "-z"])?;
    let dirty = with(&["status", "--porcelain", "-z", "--no-renames", "--untracked-files=all", "--ignored"])?;
    let clean = |p: &str| tracked.iter().any(|t| t == p) && !dirty.iter().filter_map(|d| d.get(3..)).any(|d| same_path(d, p) || d.to_lowercase().starts_with(&format!("{}/", p.to_lowercase())));
    Ok(changed.into_iter().filter(|p| !clean(p)).collect())
}

/// Runs git on the scratch index `idx`; input through stdin: thousands of paths don't fit on a command line.
fn on_index(dir: &Path, idx: &Path, args: &[&str], input: &str) -> Res<String> {
    let mut c = git::cmd(dir);
    c.env("GIT_INDEX_FILE", idx);
    git::exec(c, dir, args, Some(input.as_bytes())).map(|o| String::from_utf8_lossy(&o).trim().to_owned())
}

/// Adds index entries `<mode> <oid>\t<path>\0` to `idx`; returns the first path it skipped:
/// `--index-info` won't index some names (`git~1`, `.git.`) and only says "Ignoring path".
fn add_entries(dir: &Path, idx: &Path, info: &str) -> Res<Option<String>> {
    on_index(dir, idx, &["update-index", "-z", "--index-info"], info)?;
    let have: std::collections::HashSet<String> = split_z(on_index(dir, idx, &["ls-files", "-z"], "")?).into_iter().collect();
    Ok(split_z(info.into()).into_iter().filter_map(|e| e.split_once('\t').map(|(_, p)| p.to_owned())).find(|p| !have.contains(p)))
}

/// The first of `paths` git won't put in an index.
fn unindexable(dir: &Path, paths: &[String]) -> Res<Option<String>> {
    if paths.is_empty() {
        return Ok(None);
    }
    let idx = git::git_dir(dir)?.join("glasir-check-index");
    let _ = std::fs::remove_file(&idx);
    let blob = git::run_input(dir, &["hash-object", "-w", "--stdin"], Some(b""))?;
    let info: String = paths.iter().map(|p| format!("100644 {}\t{p}\0", blob.trim())).collect();
    let res = add_entries(dir, &idx, &info);
    let _ = std::fs::remove_file(&idx);
    res
}

/// Keeps a backup as a stash entry: commit `base` with the backed-up paths as the discard
/// found them, its index (`^2`) with the staged ones. Returns the entry's commit.
fn stash_backup(dir: &Path, s: &Saved, msg: &str, base: &str) -> Res<String> {
    let idx = git::git_dir(dir)?.join("glasir-backup-index");
    let git = |args: &[&str], input: &str| on_index(dir, &idx, args, input);
    // `base`'s tree with `paths` as `src` has them (gone where it hasn't), then `files` on top.
    let tree = |src: Option<&str>, paths: &[String], files: &[(String, Vec<u8>)]| -> Res<String> {
        git(&["read-tree", base], "")?;
        let want: std::collections::HashSet<&str> = paths.iter().map(String::as_str).collect();
        // `<mode> <oid>\t<path>` entries, later ones winning.
        let (mut info, mut have) = (String::new(), std::collections::HashSet::new());
        if let (Some(src), false) = (src, paths.is_empty()) {
            for l in split_z(git::run(dir, &["ls-tree", "-r", "-z", src])?) {
                if let Some((meta, p)) = l.split_once('\t')
                    && want.contains(p)
                    && let [mode, _, oid] = meta.split(' ').collect::<Vec<_>>()[..]
                {
                    info += &format!("{mode} {oid}\t{p}\0");
                    have.insert(p.to_owned());
                }
            }
        }
        // Copies of tracked files only keep exact bytes: the stash has their contents already.
        let files: Vec<&(String, Vec<u8>)> = files.iter().filter(|(p, _)| !have.contains(p)).collect();
        if !files.is_empty() {
            // One `hash-object` for all: written out as numbered files first.
            let tmp = idx.with_extension("blobs");
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
            let list = files.iter().enumerate().map(|(i, (_, bytes))| std::fs::write(tmp.join(i.to_string()), bytes).map(|_| format!("{}\n", tmp.join(i.to_string()).display()))).collect::<Result<String, _>>();
            let oids = list.map_err(|e| e.to_string()).and_then(|l| git::run_input(dir, &["hash-object", "-w", "--no-filters", "--stdin-paths"], Some(l.as_bytes())));
            let _ = std::fs::remove_dir_all(&tmp);
            for ((p, _), oid) in files.iter().zip(oids?.lines()) {
                info += &format!("100644 {oid}\t{p}\0");
                have.insert(p.clone());
            }
        }
        let gone: String = paths.iter().filter(|p| !have.contains(*p)).map(|p| format!("{p}\0")).collect();
        if !gone.is_empty() {
            git(&["update-index", "--force-remove", "-z", "--stdin"], &gone)?;
        }
        // `--index-info` replaces a folder where a file goes (or the reverse) on its own.
        if let Some(p) = add_entries(dir, &idx, &info)? {
            return Err(format!("git can't store the file name {p}"));
        }
        git(&["write-tree"], "")
    };
    let res = (|| {
        // Without a stash, a tracked path missing from the copies was absent. A staged one
        // whose file matched HEAD is in the stash's files too.
        let mut paths = s.tracked.clone();
        if s.index {
            paths.extend(s.staged.iter().filter(|p| !s.tracked.contains(p)).cloned());
        }
        let w = tree(s.stash.as_deref(), &paths, &s.files)?;
        let index = match (&s.stash, s.index) {
            (Some(sha), true) => tree(Some(&format!("{sha}^2")), &s.staged, &[])?,
            _ => format!("{base}^{{tree}}"),
        };
        let index = git::run(dir, &["commit-tree", &index, "-p", base, "-m", "index"])?;
        let w = git::run(dir, &["commit-tree", &w, "-p", base, "-p", index.trim(), "-m", msg])?;
        git::run(dir, &["stash", "store", "-m", msg, w.trim()]).map(|_| w.trim().to_owned())
    })();
    let _ = std::fs::remove_file(&idx);
    res
}

/// Undo of a ref step with backups: `move_refs` moves its refs, then the backups go back newest
/// first. Each is a stash entry before anything moves, so none lives only in memory while a
/// restore can fail, skip or overwrite. An entry is dropped once its backup is back, and stays
/// when that failed, when an older backup restores over the same path, or when it holds one of
/// `keep` (it would overwrite work made since). `way`: untracked files the move itself overwrites
/// (git's checkout and `reset --keep` treat ignored ones as expendable), kept the same way unless
/// still there as they were. Never fails once the refs are back: returns one note naming what
/// stayed. Err's flag: nothing moved (the step holds, its entries are dropped); without it the
/// move failed partway and every backup stays in the stash list.
fn restore_backups(dir: &Path, saved: &mut [Saved], label: &str, keep: &[String], base: &str, way: Vec<String>, move_refs: impl FnOnce() -> Result<(), (String, bool)>) -> Result<Option<String>, (String, bool)> {
    let msg = format!("Glasir: discarded during {label}");
    let way = match way.is_empty() {
        true => None,
        false => Some(save_worktree(dir, &Backup::Paths(way)).map_err(|e| (format!("Nothing was undone: the files in its way couldn't be kept ({e})."), true))?),
    };
    let drop_entries = |shas: &[String]| {
        let list = git::stashes(dir).unwrap_or_default();
        // Deepest first: dropping one renumbers the entries above it.
        for s in list.iter().rev().filter(|s| shas.contains(&s.id)) {
            let _ = git::run(dir, &["stash", "drop", "-q", &s.name]);
        }
    };
    let mut shas = vec![];
    for s in way.iter().chain(saved.iter()) {
        match stash_backup(dir, s, &msg, base) {
            Ok(sha) => shas.push(sha),
            Err(e) => {
                drop_entries(&shas);
                return Err((format!("Nothing was undone: couldn't keep the discarded files in the stash list first ({e})."), true));
            }
        }
    }
    let kept = |n: usize, why: String| format!("{why}: {} in the stash list as \"{msg}\".", if n == 1 { "it's" } else { "they're" });
    if let Err((e, still)) = move_refs() {
        if still {
            drop_entries(&shas);
            return Err((e, true));
        }
        return Err((if shas.is_empty() { e } else { format!("{e}\n\n{}", kept(shas.len(), "The discarded files weren't put back".into())) }, false));
    }
    let (mut done, mut why) = (vec![], vec![]);
    let off = way.is_some() as usize;
    if let Some(w) = &way {
        match w.files.iter().find(|(p, b)| std::fs::read(dir.join(p)).ok().as_ref() != Some(b)) {
            Some((p, _)) => why.push(format!("{p} was in the way of the undo")),
            None => done.push(shas[0].clone()),
        }
    }
    let (mut failed, mut twice) = (None, 0);
    for i in (0..saved.len()).rev() {
        let paths = |s: &Saved| s.after.lock().unwrap().iter().map(|(p, _)| p.clone()).collect::<Vec<_>>();
        if let Some(p) = paths(&saved[i]).into_iter().find(|p| keep.iter().any(|k| same_path(k, p))) {
            why.push(format!("{p} changed since it was discarded, so its backup wasn't put back"));
            continue;
        }
        let older: Vec<String> = saved[..i].iter().flat_map(paths).collect();
        saved[i].seal(dir);
        match restore_worktree(dir, &saved[i]) {
            Err(e) => failed = failed.or(Some(e)),
            Ok(()) if paths(&saved[i]).iter().any(|p| older.iter().any(|o| same_path(o, p))) => twice += 1,
            Ok(()) => done.push(shas[off + i].clone()),
        }
    }
    drop_entries(&done);
    why.extend(failed.map(|e| format!("Some discarded files couldn't be put back ({e})")));
    why.extend((twice > 0).then(|| "A file was discarded more than once, so the newer copy wasn't kept in place".to_owned()));
    Ok((shas.len() > done.len()).then(|| kept(shas.len() - done.len(), why.join("; "))))
}

/// Err if a backed-up path changed since the discard: restoring would overwrite that.
fn unchanged(dir: &Path, s: &Saved) -> Res<()> {
    match s.after.lock().unwrap().iter().find(|(p, f)| fingerprint(dir, p) != *f) {
        Some((p, _)) => Err(format!("{p} changed since the discard, and undoing it would overwrite that. Move the file aside, then undo again.")),
        None => Ok(()),
    }
}

pub(super) fn restore_worktree(dir: &Path, s: &Saved) -> Res<()> {
    unchanged(dir, s)?;
    let mut after = s.after.lock().unwrap();
    // What this undo itself changed is no "change since" for a retry after a later step fails.
    let mut reseal = |ps: &[&String]| after.iter_mut().filter(|(p, _)| ps.contains(&p)).for_each(|(p, f)| *f = fingerprint(dir, p));
    let tracked: std::collections::HashSet<&String> = s.tracked.iter().collect();
    let mut staged_since = None;
    // Only the discarded paths, so other edits made since don't block the restore.
    if let Some(sha) = &s.stash {
        let restore = |source: &str, what: &str, paths: &[String]| {
            git::run_input(dir, &["--literal-pathspecs", "restore", "--source", source, what, "--pathspec-from-file=-", "--pathspec-file-nul"], Some(paths.join("\0").as_bytes()))
        };
        if !s.tracked.is_empty() {
            let res = restore(sha, "--worktree", &s.tracked);
            reseal(&s.tracked.iter().collect::<Vec<_>>());
            res?;
        }
        // The index too, unless one of these paths has something else staged now (that staging
        // would be lost; a half-done earlier undo's is fine): then the rest goes back and this
        // fails at the end, so the caller keeps the backup.
        if s.index && !s.staged.is_empty() {
            let diff = |rev: &str| git::run(dir, &["diff", "--cached", "--name-only", "-z", "--no-renames", rev]).map(split_z);
            let (now, saved) = (diff("HEAD")?, diff(&format!("{sha}^2"))?);
            match now.iter().find(|p| saved.contains(p) && s.staged.iter().any(|q| same_path(p, q))) {
                Some(p) => staged_since = Some(format!("{p} has other changes staged now, so its staged version wasn't put back")),
                None => drop(restore(&format!("{sha}^2"), "--staged", &s.staged)?),
            }
        }
    }
    for (p, bytes) in &s.files {
        let path = dir.join(p);
        let meta = std::fs::symlink_metadata(&path);
        // Something else where an untracked copy goes: fail (the caller keeps the backup), never skip it.
        let same = |m: &std::fs::Metadata| m.is_file() && m.len() == bytes.len() as u64 && std::fs::read(&path).ok().as_ref() == Some(bytes);
        if meta.as_ref().is_ok_and(|m| !s.overwrite && !tracked.contains(p) && !same(m)) {
            return Err(format!("{p}: something else is there now"));
        }
        // Copied tracked files (and `Backup::Overwrite` copies) overwrite what's there now.
        if s.overwrite || meta.is_err() || tracked.contains(p) {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            // Never write through a symlink the op left there: replace the link itself.
            if meta.is_ok_and(|m| m.is_symlink()) {
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            }
            let res = std::fs::write(&path, bytes);
            reseal(&[p]);
            res.map_err(|e| e.to_string())?;
        }
    }
    // A resolution the op made (take a side, delete) is undone too: git keeps the stages (resolve-undo).
    // Only while that merge (rebase, pick) is still going: the stages outlive its commit.
    if !s.unmerged.is_empty() && !matches!(git::state(&git::git_dir(dir)?), git::State::Clean | git::State::Bisecting) {
        let mut a = vec!["update-index", "--unresolve", "--"];
        a.extend(s.unmerged.iter().map(String::as_str));
        git::run(dir, &a)?;
    }
    for p in &s.absent {
        let res = std::fs::remove_file(dir.join(p));
        reseal(&[p]);
        match res {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("{p}: {e}")),
            _ => {}
        }
    }
    // Without a stash every tracked path present then was copied: one without a copy was
    // absent before the op (a discarded rename's old path), so it goes again.
    if s.stash.is_none() {
        for p in s.tracked.iter().filter(|p| !s.files.iter().any(|(f, _)| f == *p)) {
            let res = std::fs::remove_file(dir.join(p));
            reseal(&[p]);
            match res {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("{p}: {e}")),
                _ => {}
            }
        }
    }
    staged_since.map_or(Ok(()), Err)
}

/// Undo/redo: move refs from `expect` back to `target`, carrying uncommitted
/// changes along when the op did (`autostash`). Err's flag: refused before anything moved
/// (a branch in use in another worktree), so the step can be retried.
pub(super) fn move_refs(dir: &Path, expect: &Snapshot, target: &Snapshot, mode: &str, autostash: Option<&str>) -> Result<(), (String, bool)> {
    let cur = git::snapshot(dir).map_err(|e| (e, false))?;
    let (mut expect, mut target) = (expect.clone(), target.clone());
    // Tags a Fetch brought in since are unrelated to this entry: keep them, they're no change.
    for (r, o) in &cur.refs {
        if !r.starts_with("refs/tags/") || expect.refs.contains_key(r) || target.refs.contains_key(r) {
            continue;
        }
        expect.refs.insert(r.clone(), o.clone());
        target.refs.insert(r.clone(), o.clone());
    }
    if cur != expect {
        return Err(("The repository changed outside Glasir since this action; undo history cleared.".into(), false));
    }
    let restore = || git::restore(dir, &target, mode).map(|_| String::new());
    let res = match autostash {
        Some(msg) => git::with_autostash(dir, msg, restore).map(drop),
        None => restore().map(drop),
    };
    res.map_err(|e| (e, git::snapshot(dir).is_ok_and(|s| s == cur)))
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

    /// `sure`: the user confirmed dropping commits that were pushed since the step.
    pub(super) fn undo_redo(&mut self, ctx: &egui::Context, undo: bool, sure: bool) {
        let stack = if undo { &self.undo } else { &self.redo };
        let Some(e) = stack.last() else { return };
        if self.running.is_some() {
            return;
        }
        let touched = |p: &str| match &e.kind {
            UndoKind::Discard(s) => s.paths().iter().any(|q| *q == p),
            UndoKind::Op { undo: u, redo } => super::tools::touches(if undo { u } else { redo }, p),
            UndoKind::Refs { .. } => true,
        };
        if let Some(open) = self.ext_tools.iter().find(|t| t.kind == "merge" && touched(&t.path)).map(|t| t.path.clone()) {
            let label = format!("{} {}", if undo { "Undo" } else { "Redo" }, e.label);
            self.tool_blocks(&label, |p| p == open);
            return;
        }
        if let (true, false, UndoKind::Refs { before, after, on_remote, .. }) = (undo, sure, &e.kind)
            && let n = on_remotes(&self.dir, before, after)
            && n > *on_remote
        {
            let n = n - on_remote;
            let mut op = Op::new(format!("Undo {}", e.label), &[UNDO_PUSHED, &e.id.to_string()]);
            let (what, it) = if n == 1 { ("The commit this undoes is".to_owned(), "it") } else { (format!("{n} commits this undoes are"), "them") };
            op.confirm = Some(format!("{what} already pushed. Undo only moves your local branch: the remote keeps {it}, so dropping {it} there needs a force push (otherwise a pull brings {it} back)."));
            self.confirm = Some(op);
            return;
        }
        // Moving refs mid merge/rebase would corrupt it; a guided split's own steps are fine.
        let busy = self.data.as_ref().is_some_and(|d| !matches!(git::state(&d.git_dir), git::State::Clean | git::State::Bisecting));
        if busy && self.split.as_ref().is_none_or(|sp| sp.replaying) && matches!(e.kind, UndoKind::Refs { .. }) {
            return self.notes.push(super::Msg { text: "Finish or abort the operation in progress first, then undo.".into(), error: true });
        }
        let dir = self.dir.clone();
        let label = format!("{} {}", if undo { "Undo" } else { "Redo" }, e.label);
        // Err's flag: the refs didn't move, so the ref steps still hold.
        // Ok: a redone op's fresh undo, and a note for the user.
        let job: Box<dyn FnOnce() -> Result<(Option<Box<super::Op>>, Option<String>), (String, bool)> + Send> = match &e.kind {
            UndoKind::Refs { before, after, mode, autostash, saved, .. } => {
                let (expect, target) = if undo { (after.clone(), before.clone()) } else { (before.clone(), after.clone()) };
                let (mode, autostash, mut saved) = (mode.mode(), autostash.clone(), if undo { saved.clone() } else { vec![] });
                let step = e.label.clone();
                // The stash entries' base: where the refs go, so applying one later only adds the files.
                let base = target.refs.get(&target.head).unwrap_or(&target.head).clone();
                // None: an unborn branch, no files in its way.
                let tip = target.refs.get(&target.head).or(Some(&target.head).filter(|h| !h.starts_with("refs/"))).cloned();
                Box::new(move || {
                    // Checked before the refs move, which changes the files: then the restore can't tell.
                    let keep = overwrites(&dir, &saved).map_err(|e| (e, true))?;
                    // Only a move of HEAD's commit writes files.
                    let head = git::run(&dir, &["rev-parse", "-q", "--verify", "HEAD"]).unwrap_or_default();
                    let way = tip.filter(|t| *t != head.trim()).map_or(Ok(vec![]), |t| in_the_way(&dir, &[&t])).map_err(|e| (e, true))?;
                    // ponytail: regular files only; a symlink in the way loses just its link.
                    let way = way.into_iter().filter(|p| std::fs::symlink_metadata(dir.join(p)).is_ok_and(|m| m.is_file())).collect();
                    let mv = || move_refs(&dir, &expect, &target, mode, autostash.as_deref());
                    restore_backups(&dir, &mut saved, &step, &keep, &base, way, mv).map(|n| (None, n))
                })
            }
            UndoKind::Op { undo: u, redo } => {
                let mut op = if undo { u.clone() } else { redo.clone() };
                Box::new(move || {
                    // Failed: every step holds. A retry of this one finishes what it started (a
                    // branch restore skips the branches back already); a ref step checks the refs itself.
                    let res = match undo {
                        true => super::op::exec(&dir, &op).map(|_| (None, None)),
                        // A redone stash save made a new stash: its undo pops that one.
                        false => super::op::exec_stash(&dir, &mut op).map(|_| (op.revert.take(), None)),
                    };
                    res.map_err(|e| (e, true))
                })
            }
            // Restoring discarded work has no meaningful redo.
            UndoKind::Discard(_) if !undo => return,
            UndoKind::Discard(saved) => {
                let (mut saved, step) = (vec![saved.clone()], e.label.clone());
                // Moves no refs: a failure leaves the ref steps below valid.
                Box::new(move || {
                    // Refused before anything changed: the step stays for a retry.
                    unchanged(&dir, &saved[0]).map_err(|e| (e, true))?;
                    match git::run(&dir, &["rev-parse", "-q", "--verify", "HEAD"]) {
                        // Like a ref step's: a restore that fails partway keeps the backup in the stash list.
                        Ok(head) => restore_backups(&dir, &mut saved, &step, &[], head.trim(), vec![], || Ok(())).map(|n| (None, n)),
                        // No commit to base a stash entry on.
                        Err(_) => restore_worktree(&dir, &saved[0]).map(|_| (None, None)).map_err(|e| (e, true)),
                    }
                })
            }
        };
        self.running = Some((
            label,
            Task::spawn(ctx, move || match job() {
                Ok(fresh) => Done::Undo(undo, Ok(fresh), false),
                Err((e, kept)) => Done::Undo(undo, Err(e), !kept),
            }),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discard_is_restorable() {
        let dir = std::env::temp_dir().join(format!("glasir-discard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| git::run(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "T"]);
        g(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        g(&["add", "a.txt"]);
        g(&["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("a.txt"), "edited\n").unwrap();
        std::fs::create_dir_all(dir.join("new")).unwrap();
        std::fs::write(dir.join("new/b.txt"), "fresh\n").unwrap();

        let saved = save_worktree(&dir, &Backup::All).unwrap();
        assert!(saved.stash.is_some());
        g(&["reset", "--hard", "-q"]);
        g(&["clean", "-fdq"]);
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
        assert!(!dir.join("new/b.txt").exists());

        restore_worktree(&dir, &saved).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "edited\n");
        assert_eq!(std::fs::read_to_string(dir.join("new/b.txt")).unwrap(), "fresh\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `stash create` fails before the first commit and with an unmerged index.
    #[test]
    fn discard_backs_up_without_stash() {
        use crate::repo::testutil::{commit, identity, scratch, sh};
        let dir = scratch("discard-nostash");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        std::fs::write(dir.join("u"), "u\n").unwrap();
        assert_eq!(save_worktree(&dir, &Backup::Paths(vec!["u".into()])).unwrap().files.len(), 1);
        std::fs::write(dir.join("s"), "staged\n").unwrap();
        sh(&dir, &["add", "s"]);
        let saved = save_worktree(&dir, &Backup::All).unwrap();
        sh(&dir, &["reset", "--hard", "-q"]);
        sh(&dir, &["clean", "-fdq"]);
        assert!(!dir.join("s").exists() && !dir.join("u").exists());
        restore_worktree(&dir, &saved).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("s")).unwrap(), "staged\n");
        assert_eq!(std::fs::read_to_string(dir.join("u")).unwrap(), "u\n");
        std::fs::remove_file(dir.join("s")).unwrap();

        commit(&dir, "f", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "f", "theirs\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "f", "ours\n");
        assert!(git::run(&dir, &["merge", "-q", "feat"]).is_err());
        let conflicted = std::fs::read_to_string(dir.join("f")).unwrap();
        let saved = save_worktree(&dir, &Backup::All).unwrap();
        sh(&dir, &["reset", "--hard", "-q"]);
        sh(&dir, &["clean", "-fdq"]);
        assert!(!dir.join("u").exists());
        restore_worktree(&dir, &saved).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), conflicted);
        assert_eq!(std::fs::read_to_string(dir.join("u")).unwrap(), "u\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tag fetched since an undo entry doesn't block undoing it, and stays.
    /// A rebase stopped on a conflict keeps its entry, but undo waits until it's finished.
    #[test]
    fn undo_survives_new_tags_and_waits_for_stopped_rebases() {
        use super::super::testutil::{commit, identity, scratch, settle, sh};
        use super::super::{Action, Op, RepoTab, Undo, View};
        let dir = scratch("undo-tags");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "one\n");
        let before = git::snapshot(&dir).unwrap();
        commit(&dir, "a.txt", "two\n");
        let after = git::snapshot(&dir).unwrap();
        sh(&dir, &["update-ref", "refs/tags/v2", "HEAD"]);
        move_refs(&dir, &after, &before, "--keep", None).unwrap();
        assert!(sh(&dir, &["tag"]).contains("v2"));
        assert_eq!(git::snapshot(&dir).unwrap().refs.get("refs/heads/main"), before.refs.get("refs/heads/main"));

        sh(&dir, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, "a.txt", "feat\n");
        let feat_tip = sh(&dir, &["rev-parse", "feat"]);
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "a.txt", "main\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Rebase feat onto main", &["rebase", "main", "feat"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        assert_eq!(t.data.as_ref().unwrap().state, git::State::Rebasing);
        assert_eq!(t.undo_label(), Some("Rebase feat onto main"));
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(t.data.as_ref().unwrap().state, git::State::Rebasing);
        assert_eq!(t.undo_label(), Some("Rebase feat onto main"));
        assert!(t.notes.pop().is_some_and(|m| m.error));

        std::fs::write(dir.join("a.txt"), "both\n").unwrap();
        sh(&dir, &["add", "a.txt"]);
        t.apply(&ctx, Action::Git(Op::new("Continue rebase", &["-c", "core.editor=true", "rebase", "--continue"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        for _ in 0..2 {
            t.apply(&ctx, Action::Undo);
            settle(&ctx, &mut t);
        }
        assert_eq!(t.redo_label(), Some("Rebase feat onto main"));
        assert_eq!(sh(&dir, &["rev-parse", "feat"]), feat_tip);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_step_at_the_same_depth_has_a_new_id() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, View};
        let dir = scratch("undo-top");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let step = |t: &mut RepoTab, a: Action| {
            t.apply(&ctx, a);
            settle(&ctx, t);
            t.undo_top()
        };
        let first = step(&mut t, Action::Git(Op::new("Branch", &["branch", "x"]).undo(Undo::Keep)));
        assert!(first.is_some());
        assert_eq!(step(&mut t, Action::Undo), None);
        let second = step(&mut t, Action::Git(Op::new("Branch", &["branch", "y"]).undo(Undo::Keep)));
        assert!(second.is_some() && second != first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo of a discard refuses (and keeps its backup) when the path changed since.
    #[test]
    fn undo_discard_refuses_over_newer_edits() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, View};
        let dir = scratch("undo-newer");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let step = |t: &mut RepoTab, a: Action| {
            t.apply(&ctx, a);
            settle(&ctx, t)
        };
        let discard = |args: &[&str], p: &str| Action::Git(Op::new("Discard", args).light().backup(Backup::Paths(vec![p.into()])));
        let read = |p: &str| std::fs::read_to_string(dir.join(p)).unwrap();
        // A refused discard undo moved no refs: the ref step below it stays.
        step(&mut t, Action::Git(Op::new("Create branch x", &["branch", "x"]).undo(super::Undo::Keep)));

        std::fs::write(dir.join("a"), "E1\n").unwrap();
        step(&mut t, discard(&["checkout", "--", "a"], "a"));
        std::fs::write(dir.join("a"), "E2\n").unwrap();
        let msgs = step(&mut t, Action::Undo);
        assert_eq!(read("a"), "E2\n");
        assert!(msgs.iter().any(|m| m.error && m.text.contains("changed since")) && t.undo_label() == Some("Discard"));
        std::fs::write(dir.join("a"), "base\n").unwrap();
        step(&mut t, Action::Undo);
        assert_eq!(read("a"), "E1\n");
        sh(&dir, &["checkout", "--", "a"]);

        std::fs::write(dir.join("notes.txt"), "A\n").unwrap();
        step(&mut t, discard(&["clean", "-fq", "--", "notes.txt"], "notes.txt"));
        std::fs::write(dir.join("notes.txt"), "B\n").unwrap();
        let msgs = step(&mut t, Action::Undo);
        assert_eq!(read("notes.txt"), "B\n");
        assert!(msgs.iter().any(|m| m.error && m.text.contains("changed since")) && t.undo_label() == Some("Discard"));
        std::fs::remove_file(dir.join("notes.txt")).unwrap();
        step(&mut t, Action::Undo);
        assert_eq!(read("notes.txt"), "A\n");
        assert_eq!(t.undo_label(), Some("Create branch x"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed ref undo drops ref snapshots only (and only once refs moved or changed outside):
    /// a force-deleted branch stays restorable.
    #[test]
    fn failed_ref_undo_keeps_op_entries() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, Undo, View};
        let dir = scratch("undo-keep-op");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "main\n");
        sh(&dir, &["checkout", "-q", "-b", "x"]);
        commit(&dir, "x", "only on x\n");
        let x = sh(&dir, &["rev-parse", "x"]).trim().to_owned();
        sh(&dir, &["checkout", "-q", "-b", "y", "main"]);
        commit(&dir, "a", "y\n");
        sh(&dir, &["checkout", "-q", "main"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let step = |t: &mut RepoTab, a: Action| {
            t.apply(&ctx, a);
            settle(&ctx, t);
        };
        step(&mut t, Action::Git(Op::new("Delete x", &["branch", "-D", "x"]).revert(Op::new("Restore x", &["branch", "x", &x]))));
        step(&mut t, Action::Git(Op::new("Checkout y", &["checkout", "-q", "y"]).undo(Undo::Keep)));
        // Refused before anything moved (a local edit in the way): the step stays.
        std::fs::write(dir.join("a"), "local edit\n").unwrap();
        step(&mut t, Action::Undo);
        assert_eq!(t.undo_label(), Some("Checkout y"));
        sh(&dir, &["checkout", "--", "a"]);
        sh(&dir, &["branch", "outside"]);
        step(&mut t, Action::Undo);
        assert_eq!(t.undo_label(), Some("Delete x"));
        step(&mut t, Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "x"]).trim(), x);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Thousands of long paths don't fit on a command line: they go through stdin.
    #[test]
    fn undo_discard_of_many_files() {
        use crate::repo::testutil::*;
        let dir = scratch("undo-many");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let sub = dir.join("d".repeat(190));
        std::fs::create_dir_all(&sub).unwrap();
        let write = |text: &str| (0..12000).for_each(|i| std::fs::write(sub.join(format!("f{i}")), text).unwrap());
        write("v1\n");
        sh(&dir, &["add", "-A"]);
        sh(&dir, &["commit", "-q", "-m", "init"]);
        write("v2\n");
        let saved = save_worktree(&dir, &Backup::All).unwrap();
        let entry = stash_backup(&dir, &saved, "m", "HEAD").unwrap();
        assert_eq!(sh(&dir, &["show", &format!("{entry}:{}/f11999", "d".repeat(190))]), "v2\n");
        sh(&dir, &["reset", "--hard", "-q"]);
        restore_worktree(&dir, &saved).unwrap();
        assert_eq!(std::fs::read_to_string(sub.join("f11999")).unwrap(), "v2\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo replaces a symlink the restore created instead of writing through it.
    #[cfg(unix)]
    #[test]
    fn undo_restore_does_not_write_through_a_symlink() {
        use crate::repo::testutil::*;
        let dir = scratch("undo-symlink");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "real.env", "TRACKED=1\n");
        std::os::unix::fs::symlink("real.env", dir.join(".env")).unwrap();
        sh(&dir, &["add", ".env"]);
        sh(&dir, &["commit", "-q", "-m", "link"]);
        let c = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        sh(&dir, &["rm", "-q", "--cached", ".env"]);
        std::fs::write(dir.join(".gitignore"), ".env\n").unwrap();
        sh(&dir, &["add", ".gitignore"]);
        sh(&dir, &["commit", "-q", "-m", "ignore"]);
        std::fs::remove_file(dir.join(".env")).unwrap();
        std::fs::write(dir.join(".env"), "SECRET=mine\n").unwrap();

        let saved = save_worktree(&dir, &Backup::Overwrite(vec![".env".into()])).unwrap();
        sh(&dir, &["restore", "--source", &c, "--worktree", "--", ".env"]);
        assert!(std::fs::symlink_metadata(dir.join(".env")).unwrap().is_symlink());
        restore_worktree(&dir, &saved).unwrap();
        assert!(!std::fs::symlink_metadata(dir.join(".env")).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(dir.join(".env")).unwrap(), "SECRET=mine\n");
        assert_eq!(std::fs::read_to_string(dir.join("real.env")).unwrap(), "TRACKED=1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo never moves or deletes a branch another worktree has checked out.
    #[test]
    fn undo_leaves_other_worktrees_branches_alone() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, Undo, View};
        let dir = scratch("undo-worktree");
        let wt = scratch("undo-worktree-w");
        let _ = std::fs::remove_dir_all(&wt);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Create branch", &["branch", "x"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        sh(&dir, &["worktree", "add", "-q", wt.to_str().unwrap(), "x"]);
        t.apply(&ctx, Action::Undo);
        assert!(settle(&ctx, &mut t).iter().any(|m| m.error && m.text.contains("another worktree")));
        assert!(git::run(&dir, &["rev-parse", "--verify", "x"]).is_ok());
        // Nothing moved: the step stays for a retry once that worktree lets go.
        assert_eq!(t.undo_label(), Some("Create branch"));
        sh(&dir, &["worktree", "remove", wt.to_str().unwrap()]);
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert!(git::run(&dir, &["rev-parse", "--verify", "x"]).is_err());
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A name that isn't valid UTF-8 comes back from git with U+FFFD: refuse to discard it.
    #[test]
    fn discard_refuses_undecodable_names() {
        use crate::repo::testutil::*;
        let dir = scratch("undo-fffd");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a\n");
        std::fs::write(dir.join("caf\u{FFFD}.txt"), "x\n").unwrap();
        assert!(save_worktree(&dir, &Backup::All).err().is_some_and(|e| e.contains("UTF-8")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo of "Discard all" brings back the staged changes and the exact bytes.
    #[test]
    fn undo_discard_all_restores_index_and_line_endings() {
        use crate::repo::testutil::*;
        let dir = scratch("undo-index");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
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
        let saved = save_worktree(&dir, &Backup::All).unwrap();
        sh(&dir, &["reset", "--hard", "-q"]);
        sh(&dir, &["clean", "-fdq"]);
        restore_worktree(&dir, &saved).unwrap();
        assert_eq!(std::fs::read(dir.join("b")).unwrap(), b"one\ntwo\n");
        assert_eq!(sh(&dir, &["status", "--porcelain"]), before);
        assert_eq!(sh(&dir, &["show", ":c"]).replace('\r', ""), "c1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ⌘Z on a discard after the file was edited again: refuses instead of overwriting the edit.
    #[test]
    fn undo_of_a_discard_keeps_edits_made_since() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, View};
        let dir = scratch("undo-discard-newer");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "base\n");
        std::fs::write(dir.join("f"), "first edit\n").unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Discard all", &["reset", "--hard", "-q"]).then(&["clean", "-fdq"]).backup(Backup::All)));
        settle(&ctx, &mut t);
        std::fs::write(dir.join("f"), "second edit\n").unwrap();
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "second edit\n");
        assert_eq!(t.undo_label(), Some("Discard all"));
        // Back to what the discard left: the undo goes through.
        std::fs::write(dir.join("f"), "base\n").unwrap();
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "first edit\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Changes that exist only in the index ("MM" back to HEAD, "AD") come back too.
    #[test]
    fn undo_discard_all_restores_index_only_changes() {
        use crate::repo::testutil::*;
        let dir = scratch("undo-index-only");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "v1\n");
        std::fs::write(dir.join("f"), "v2\n").unwrap();
        std::fs::write(dir.join("n"), "new\n").unwrap();
        sh(&dir, &["add", "f", "n"]);
        std::fs::write(dir.join("f"), "v1\n").unwrap();
        std::fs::remove_file(dir.join("n")).unwrap();
        let before = sh(&dir, &["status", "--porcelain"]);
        let mut saved = save_worktree(&dir, &Backup::All).unwrap();
        sh(&dir, &["reset", "--hard", "-q"]);
        saved.seal(&dir);
        restore_worktree(&dir, &saved).unwrap();
        assert_eq!(sh(&dir, &["status", "--porcelain"]), before);
        assert_eq!(sh(&dir, &["show", ":f"]), "v2\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An undo that failed halfway (here: a file where an untracked file's folder goes) can be retried.
    #[test]
    fn undo_discard_retries_after_a_half_applied_attempt() {
        use crate::repo::testutil::*;
        let dir = scratch("undo-half");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "b", "b0\n");
        commit(&dir, "c", "c0\n");
        std::fs::write(dir.join("b"), "b1\n").unwrap();
        std::fs::write(dir.join("c"), "c1\n").unwrap();
        sh(&dir, &["add", "c"]);
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/u"), "u\n").unwrap();
        let before = sh(&dir, &["status", "--porcelain", "-uall"]);
        let mut saved = save_worktree(&dir, &Backup::All).unwrap();
        sh(&dir, &["reset", "--hard", "-q"]);
        sh(&dir, &["clean", "-fdq"]);
        saved.seal(&dir);
        std::fs::write(dir.join("d"), "in the way\n").unwrap();
        assert!(restore_worktree(&dir, &saved.clone()).is_err());
        std::fs::remove_file(dir.join("d")).unwrap();
        restore_worktree(&dir, &saved).unwrap();
        assert_eq!(sh(&dir, &["status", "--porcelain", "-uall"]), before);
        assert_eq!(std::fs::read_to_string(dir.join("d/u")).unwrap(), "u\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A removal that fails partway reseals what it removed, so the retry isn't refused.
    #[cfg(unix)]
    #[test]
    fn undo_removal_retries_after_a_partial_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::repo::testutil::scratch("undo-rm-half");
        std::fs::create_dir(dir.join("z")).unwrap();
        std::fs::write(dir.join("a"), "a\n").unwrap();
        std::fs::write(dir.join("z/b"), "b\n").unwrap();
        let mut saved = Saved { stash: None, tracked: vec!["a".into(), "z/b".into()], staged: vec![], unmerged: vec![], files: vec![], overwrite: false, absent: vec![], index: false, after: Default::default() };
        saved.seal(&dir);
        let mode = |m| std::fs::set_permissions(dir.join("z"), std::fs::Permissions::from_mode(m)).unwrap();
        mode(0o555);
        assert!(restore_worktree(&dir, &saved).is_err());
        mode(0o755);
        restore_worktree(&dir, &saved).unwrap();
        assert!(!dir.join("a").exists() && !dir.join("z/b").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The streamed fingerprint still sees a change past the first read chunk.
    #[test]
    fn fingerprint_covers_the_whole_file() {
        let dir = crate::repo::testutil::scratch("undo-fp");
        let (mut a, p) = (vec![b'x'; 200_000], dir.join("f"));
        std::fs::write(&p, &a).unwrap();
        let before = fingerprint(&dir, "f");
        a[150_000] = b'y';
        std::fs::write(&p, &a).unwrap();
        assert_ne!(fingerprint(&dir, "f"), before);
        std::fs::remove_file(&p).unwrap();
        assert_eq!(fingerprint(&dir, "f"), None);
        // A file where its folder goes is something there.
        assert_eq!(fingerprint(&dir, "f/x"), None);
        std::fs::write(&p, "in the way").unwrap();
        assert!(fingerprint(&dir, "f/x").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo never moves a branch another worktree is rebasing (shown there as detached).
    #[test]
    fn undo_leaves_branches_rebased_elsewhere() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, View};
        let (dir, wt) = (scratch("undo-wt-rebase"), scratch("undo-wt-rebase-other"));
        let _ = std::fs::remove_dir_all(&wt);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "x"]);
        commit(&dir, "a", "x1\n");
        commit(&dir, "a", "x2\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "a", "main\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Move x", &["branch", "-f", "x", "x~1"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        let x1 = sh(&dir, &["rev-parse", "x"]);
        sh(&dir, &["worktree", "add", "-q", wt.to_str().unwrap(), "x"]);
        assert!(git::run(&wt, &["rebase", "main"]).is_err());
        t.apply(&ctx, Action::Undo);
        assert!(settle(&ctx, &mut t).iter().any(|m| m.error && m.text.contains("another worktree")));
        assert_eq!(sh(&dir, &["rev-parse", "x"]), x1);
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Undo never moves a branch that another worktree has checked out.
    #[test]
    fn undo_leaves_branches_checked_out_elsewhere() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, View};
        let (dir, wt) = (scratch("undo-wt"), scratch("undo-wt-other"));
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a");
        sh(&dir, &["branch", "feat"]);
        commit(&dir, "b", "b");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Move feat", &["branch", "-f", "feat", "main"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        sh(&dir, &["worktree", "add", "-q", wt.to_str().unwrap(), "feat"]);
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["rev-parse", "feat"]), sh(&dir, &["rev-parse", "main"]));
        assert_eq!(sh(&wt, &["status", "--porcelain"]), "");
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A branch another worktree is rebasing shows there as detached: still never move it,
    /// since finishing that rebase would overwrite what the undo restored.
    #[test]
    fn undo_leaves_branches_rebasing_elsewhere() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, View};
        let (dir, wt) = (scratch("undo-wt-rebasing"), scratch("undo-wt-rebasing-other"));
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a");
        sh(&dir, &["branch", "feat"]);
        commit(&dir, "b", "b");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Move feat", &["branch", "-f", "feat", "main"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        sh(&dir, &["worktree", "add", "-q", wt.to_str().unwrap(), "feat"]);
        assert!(git::run(&wt, &["rebase", "-q", "--exec", "false", "HEAD~1"]).is_err());
        t.apply(&ctx, Action::Undo);
        assert!(settle(&ctx, &mut t).iter().any(|m| m.error && m.text.contains("another worktree")));
        assert_eq!(sh(&dir, &["rev-parse", "feat"]), sh(&dir, &["rev-parse", "main"]));
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ⌘Z after Commit & push asks first: the commit is on the remote already. Undoing a
    /// pull (its commits were on the remote before it ran) doesn't.
    #[test]
    fn undo_of_a_pushed_commit_asks_first() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, View};
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
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, a.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let pull = || Action::Git(Op::new("Pull", &["merge", "-q", "--ff-only", "@{u}"]).undo(Undo::Keep));
        t.apply(&ctx, pull());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Undo);
        assert!(t.confirm.is_none());
        settle(&ctx, &mut t);
        assert_eq!(sh(&a, &["rev-parse", "HEAD"]), one);
        t.apply(&ctx, pull());
        settle(&ctx, &mut t);

        std::fs::write(a.join("f"), "three\n").unwrap();
        sh(&a, &["add", "f"]);
        (t.summary, t.push_after) = ("three".into(), true);
        t.apply(&ctx, Action::Git(t.commit_op()));
        settle(&ctx, &mut t);
        let three = sh(&a, &["rev-parse", "HEAD"]);
        assert_eq!(sh(&root.join("remote.git"), &["rev-parse", "main"]), three);
        t.apply(&ctx, Action::Undo);
        let ask = t.confirm.take().expect("asks before undoing a pushed commit");
        assert!(ask.confirm.as_deref().unwrap().contains("force push"), "{:?}", ask.confirm);
        settle(&ctx, &mut t);
        assert_eq!(sh(&a, &["rev-parse", "HEAD"]), three);
        t.apply(&ctx, Action::Git(Op { confirm: None, ..ask }));
        settle(&ctx, &mut t);
        assert_eq!(sh(&a, &["rev-parse", "HEAD"]), two);
        assert_eq!(sh(&a, &["diff", "--cached", "--name-only"]).trim(), "f");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn hard_reset(ctx: &egui::Context, t: &mut RepoTab, id: &str) -> Vec<String> {
        let mut op = super::super::reset_op(t.data.as_ref().unwrap(), id, "--hard");
        op.confirm = None;
        t.apply(ctx, crate::repo::Action::Git(op));
        crate::repo::testutil::settle(ctx, t).into_iter().map(|m| m.text).collect()
    }

    /// On a case-insensitive disk, an untracked `Readme` is in the way of a tracked
    /// `README`: the hard reset backs it up and ⌘Z brings it back.
    #[test]
    fn hard_reset_backs_up_a_file_in_the_way_in_another_case() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, View};
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
        let x = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        sh(&dir, &["rm", "-q", "README"]);
        sh(&dir, &["commit", "-q", "-m", "rm readme"]);
        std::fs::write(dir.join("Readme"), "MY NOTES\n").unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        hard_reset(&ctx, &mut t, &x);
        t.apply(&ctx, Action::Undo);
        let msgs: Vec<String> = settle(&ctx, &mut t).into_iter().map(|m| m.text).collect();
        assert_eq!(std::fs::read_to_string(dir.join("Readme")).ok().as_deref(), Some("MY NOTES\n"), "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// r2-6: a hard reset's backup kept in the stash list (the file was edited since) keeps the
    /// staged version too, in the entry's index (`^2`). Also on a detached HEAD.
    #[test]
    fn kept_hard_reset_backup_keeps_the_staged_version() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, View};
        for detached in [false, true] {
            let dir = scratch(if detached { "undo-kept-index-d" } else { "undo-kept-index" });
            sh(&dir, &["init", "-q", "-b", "main"]);
            identity(&dir);
            commit(&dir, "a", "base\n");
            commit(&dir, "b", "b1\n");
            let x = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
            commit(&dir, "b", "b2\n");
            if detached {
                sh(&dir, &["checkout", "-q", "--detach"]);
            }
            std::fs::write(dir.join("a"), "STAGED-ONLY\n").unwrap();
            sh(&dir, &["add", "a"]);
            std::fs::write(dir.join("a"), "WORKTREE\n").unwrap();
            let ctx = egui::Context::default();
            let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
            settle(&ctx, &mut t);
            let msgs = hard_reset(&ctx, &mut t, &x);
            assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "base\n", "{msgs:?}");
            std::fs::write(dir.join("a"), "NEW EDIT\n").unwrap();
            t.apply(&ctx, Action::Undo);
            let msgs: Vec<String> = settle(&ctx, &mut t).into_iter().map(|m| m.text).collect();
            assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "NEW EDIT\n");
            assert!(msgs.iter().any(|m| m.contains("stash")), "{msgs:?}");
            assert_eq!(sh(&dir, &["show", "stash@{0}^2:a"]), "STAGED-ONLY\n");
            assert_eq!(sh(&dir, &["show", "stash@{0}:a"]), "WORKTREE\n");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// r2-5: an Op step's undo that git refuses before changing anything (a stash pop over a
    /// local edit) keeps every step below it undoable.
    #[test]
    fn refused_op_undo_keeps_the_ref_steps() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, Undo, View};
        let dir = scratch("undo-op-refused");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a0\n");
        let c0 = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        commit(&dir, "a", "a1\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let step = |t: &mut RepoTab, a: Action| {
            t.apply(&ctx, a);
            settle(&ctx, t)
        };
        step(&mut t, Action::Git(Op::new("Create branch x", &["branch", "x"]).undo(Undo::Keep)));
        std::fs::write(dir.join("a"), "MINE\n").unwrap();
        hard_reset(&ctx, &mut t, &c0);
        std::fs::write(dir.join("a"), "STASHED\n").unwrap();
        step(&mut t, Action::Git(Op::new("Stash", &["stash", "push", "--include-untracked", "-m", "s"])));
        let labels = |t: &RepoTab| t.undo.iter().map(|e| e.label.clone()).collect::<Vec<_>>();
        let before = labels(&t);
        assert_eq!(before.len(), 3);
        std::fs::write(dir.join("a"), "LATER EDIT\n").unwrap();
        let msgs = step(&mut t, Action::Undo);
        assert!(msgs.iter().any(|m| m.error && m.text.contains("would be overwritten")), "{:?}", msgs.iter().map(|m| &m.text).collect::<Vec<_>>());
        assert_eq!(labels(&t), before);
        assert!(matches!(t.undo[1].kind, UndoKind::Refs { .. }));
        sh(&dir, &["checkout", "--", "a"]);
        step(&mut t, Action::Undo);
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "STASHED\n");
        sh(&dir, &["stash", "push", "-q"]);
        for _ in 0..2 {
            step(&mut t, Action::Undo);
        }
        assert_eq!(t.undo_label(), None);
        assert!(git::run(&dir, &["rev-parse", "--verify", "x"]).is_err());
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "MINE\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tab(dir: &Path) -> (egui::Context, RepoTab) {
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.to_path_buf(), 100, crate::repo::View::default());
        crate::repo::testutil::settle(&ctx, &mut t);
        (ctx, t)
    }

    fn step(ctx: &egui::Context, t: &mut RepoTab, a: crate::repo::Action) -> Vec<String> {
        t.apply(ctx, a);
        crate::repo::testutil::settle(ctx, t).into_iter().map(|m| m.text).collect()
    }

    /// Some stash entry holds `want` at `p`.
    fn in_stash(dir: &Path, p: &str, want: &str) -> bool {
        let list = git::run(dir, &["stash", "list", "--format=%H"]).unwrap_or_default();
        list.lines().any(|h| git::run(dir, &["show", &format!("{h}:{p}")]).is_ok_and(|s| s == want))
    }

    /// r3: a staged new `d/x` discarded (Discard all, or the file's own discard), then a file `d`
    /// made where its folder was: ⌘Z never replaces that file with the folder.
    #[test]
    fn undo_discard_keeps_a_file_where_a_folder_was() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op};
        for all in [true, false] {
            let dir = scratch(if all { "undo-dir-file-all" } else { "undo-dir-file-one" });
            sh(&dir, &["init", "-q", "-b", "main"]);
            identity(&dir);
            commit(&dir, "keep", "k\n");
            std::fs::create_dir(dir.join("d")).unwrap();
            std::fs::write(dir.join("d/x"), "STAGED NEW\n").unwrap();
            sh(&dir, &["add", "d/x"]);
            let (ctx, mut t) = tab(&dir);
            let op = match all {
                true => Op::new("Discard all", &["reset", "--hard", "-q"]).then(&["clean", "-fdq"]).backup(Backup::All),
                false => Op::new("Discard", &["--literal-pathspecs", "rm", "-fq", "--", "d/x"]).light().backup(Backup::Paths(vec!["d/x".into()])),
            };
            step(&ctx, &mut t, Action::Git(op));
            assert!(!dir.join("d").exists());
            std::fs::write(dir.join("d"), "NEW WORK\n").unwrap();
            let msgs = step(&ctx, &mut t, Action::Undo);
            assert_eq!(std::fs::read_to_string(dir.join("d")).ok().as_deref(), Some("NEW WORK\n"), "{msgs:?}");
            std::fs::remove_file(dir.join("d")).unwrap();
            step(&ctx, &mut t, Action::Undo);
            assert_eq!(std::fs::read_to_string(dir.join("d/x")).ok().as_deref(), Some("STAGED NEW\n"));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// r3: after a hard reset, a backed-up file's folder replaced by a file: ⌘Z keeps that file
    /// and the backup (in the stash list).
    #[test]
    fn hard_reset_undo_keeps_a_file_where_a_folder_was() {
        use crate::repo::testutil::*;
        let dir = scratch("undo-reset-dir-file");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        std::fs::create_dir(dir.join("d")).unwrap();
        commit(&dir, "d/x", "base\n");
        let c0 = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        commit(&dir, "y", "y\n");
        let c1 = sh(&dir, &["rev-parse", "HEAD"]);
        std::fs::write(dir.join("d/x"), "MINE\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        hard_reset(&ctx, &mut t, &c0);
        std::fs::remove_dir_all(dir.join("d")).unwrap();
        std::fs::write(dir.join("d"), "NEW WORK\n").unwrap();
        let msgs = step(&ctx, &mut t, crate::repo::Action::Undo);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), c1, "{msgs:?}");
        assert_eq!(std::fs::read_to_string(dir.join("d")).ok().as_deref(), Some("NEW WORK\n"), "{msgs:?}");
        assert!(in_stash(&dir, "d/x", "MINE\n"), "{msgs:?}");
        assert_eq!(msgs.iter().filter(|m| m.contains("stash")).count(), 1, "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// r3: an untracked file with a name git won't index (`git~1`) refuses its discard, so a
    /// restore that fails partway can't lose it.
    #[test]
    fn discard_refuses_a_name_git_cannot_store() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op};
        let dir = scratch("undo-unstorable");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "keep", "k\n");
        std::fs::create_dir(dir.join("a")).unwrap();
        std::fs::write(dir.join("a/b.txt"), "AB\n").unwrap();
        std::fs::write(dir.join("git~1"), "SECRET\n").unwrap();
        let (ctx, mut t) = tab(&dir);
        let paths = vec!["a/b.txt".to_owned(), "git~1".to_owned()];
        let msgs = step(&ctx, &mut t, Action::Git(Op::new("Discard 2 files", &["--literal-pathspecs", "clean", "-fq", "--", "a/b.txt", "git~1"]).light().backup(Backup::Paths(paths))));
        let _ = std::fs::remove_dir_all(dir.join("a"));
        std::fs::write(dir.join("a"), "IN THE WAY\n").unwrap();
        let msgs = [msgs, step(&ctx, &mut t, Action::Undo)].concat();
        let kept = std::fs::read_to_string(dir.join("git~1")).is_ok_and(|s| s == "SECRET\n") || in_stash(&dir, "git~1", "SECRET\n");
        assert!(kept, "{msgs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// r3: a branch restore that finds a branch already back at its commit skips it, and a
    /// failed one keeps every step: ⌘Z never sticks or wipes the steps below.
    #[test]
    fn branch_restore_never_sticks() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, Op, Undo};
        for same in [true, false] {
            let dir = scratch(if same { "undo-branch-same" } else { "undo-branch-other" });
            sh(&dir, &["init", "-q", "-b", "main"]);
            identity(&dir);
            commit(&dir, "keep", "k\n");
            sh(&dir, &["branch", "a"]);
            sh(&dir, &["branch", "b"]);
            let (ctx, mut t) = tab(&dir);
            step(&ctx, &mut t, Action::Git(Op::new("Create branch x", &["branch", "x"]).undo(Undo::Keep)));
            let op = crate::repo::delete_branches(t.data.as_ref().unwrap(), "Delete a, b".into(), &["a", "b"], true);
            step(&ctx, &mut t, Action::Git(op));
            let labels = |t: &RepoTab| t.undo.iter().map(|e| e.label.clone()).collect::<Vec<_>>();
            if same {
                sh(&dir, &["branch", "b"]);
            } else {
                commit(&dir, "other", "o\n");
                sh(&dir, &["branch", "b"]);
                sh(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
                let msgs = step(&ctx, &mut t, Action::Undo);
                assert_eq!(labels(&t), ["Create branch x", "Delete a, b"], "{msgs:?}");
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
}
