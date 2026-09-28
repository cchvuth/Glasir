//! The open diff: loads a file diff (with syntax colors and image sides) in the background.

use super::action::DiffSrc;
use super::op::{Backup, Op};
use super::image_diff::{self, ImageDiff};
use super::task::Task;
use super::RepoTab;
use crate::git::{self, DiffOpts, FileDiff, Res};
use std::collections::BTreeSet;

type Loaded = (FileDiff, Vec<Vec<crate::highlight::Spans>>, Option<ImageDiff>);

pub struct DiffState {
    pub src: DiffSrc,
    pub file: Option<Res<FileDiff>>,
    /// Syntax colors per `[hunk][line]` (empty if unknown language).
    pub hl: Vec<Vec<crate::highlight::Spans>>,
    /// Both sides of a changed image.
    pub image: Option<ImageDiff>,
    /// The options `file` was loaded with; `pending` = requested (they differ while a reload runs).
    pub opts: DiffOpts,
    pub pending: DiffOpts,
    task: Option<Task<Res<Loaded>>>,
    /// Picked change lines `(hunk, line)` for partial staging.
    pub picked: BTreeSet<(usize, usize)>,
}

impl DiffState {
    /// A (re)load is running: the shown `file` may predate the last op.
    pub fn loading(&self) -> bool {
        self.task.is_some()
    }
}

impl RepoTab {
    pub(super) fn refresh_diff(&mut self, ctx: &egui::Context) {
        // Part of an untracked file was staged: the rest is now an ordinary unstaged change.
        if let (Some(d), Some(data)) = (&mut self.diff, &self.data)
            && let DiffSrc::Worktree { change, staged: false } = &mut d.src
            && change.code == '?'
            && !data.status.unstaged.iter().any(|c| c.path == change.path && c.code == '?')
        {
            match data.status.unstaged.iter().find(|c| c.path == change.path) {
                Some(c) => *change = c.clone(),
                None => self.diff = None,
            }
        }
        if let Some(d) = &self.diff
            && matches!(&d.src, DiffSrc::Worktree { .. }) | matches!(&d.src, DiffSrc::Commit { id, .. } if id == git::WORKTREE)
        {
            self.open_diff(ctx, d.src.clone());
        }
    }

    pub(super) fn open_diff(&mut self, ctx: &egui::Context, src: DiffSrc) {
        let dir = self.dir.clone();
        let s = src.clone();
        let dark = ctx.global_style().visuals.dark_mode;
        let (c, o) = (ctx.clone(), self.diff_opts);
        let task = Task::spawn(ctx, move || {
            let f = match &s {
                DiffSrc::Worktree { change, staged } => git::worktree_diff(&dir, change, *staged, o),
                DiffSrc::Commit { id, parent, change } => git::commit_diff(&dir, id, parent.as_deref(), change, o),
            }?;
            let path = &s.change().path;
            let image = (f.binary && f.lfs.is_none() && image_diff::is_image(path)).then(|| image_diff::load(&c, &dir, &s));
            let hl = crate::highlight::highlight(path, &f, dark);
            Ok((f, hl, image))
        });
        let (keep, hl, image, opts, picked) = match self.diff.take().filter(|d| d.src == src) {
            Some(d) => (d.file, d.hl, d.image, d.opts, d.picked),
            None => (None, vec![], None, o, BTreeSet::new()),
        };
        self.diff = Some(DiffState { src, file: keep, hl, image, opts, pending: o, task: Some(task), picked });
    }

    pub(super) fn poll_diff(&mut self) {
        if let Some(d) = &mut self.diff
            && let Some(r) = Task::poll(&mut d.task)
        {
            d.opts = d.pending;
            match r {
                Ok((f, hl, image)) => {
                    // Picks are (hunk, line) indices: keep them only while they point at the same lines.
                    if !matches!(&d.file, Some(Ok(old)) if old.hunks == f.hunks) {
                        d.picked.clear();
                    }
                    d.file = Some(Ok(f));
                    d.hl = hl;
                    d.image = image;
                }
                Err(e) => {
                    d.picked.clear();
                    d.file = Some(Err(e));
                }
            }
        }
    }
}

/// `exec` pseudo-command: `DIFF_IS staged context code path old_path diff` fails unless
/// the file's diff is still exactly `diff` ([`shape`]).
pub(super) const DIFF_IS: &str = "!diff-is";

/// Header (mode changes, which a whole-hunk patch copies) and hunks.
fn shape(f: &FileDiff) -> String {
    format!("{:?}{:?}", f.header, f.hunks)
}

pub(super) fn diff_is(dir: &std::path::Path, a: &[&str]) -> Res<String> {
    let [_, staged, context, code, path, old, want] = a else { return Err("bad diff guard".into()) };
    let c = git::Change { path: path.to_string(), old_path: (!old.is_empty()).then(|| old.to_string()), code: code.chars().next().unwrap_or('M') };
    let o = DiffOpts { ignore_ws: false, context: context.parse().unwrap_or(3), all: true, no_attrs: false };
    // Untracked when shown: staged or tracked since means the patch (and its rollback) is not ours to apply.
    if c.code == '?' && !git::run(dir, &["--literal-pathspecs", "ls-files", "--cached", "--", path])?.trim().is_empty() {
        return Err("The file is no longer untracked, so nothing was applied. Check the updated diff and try again.".into());
    }
    match shape(&git::worktree_diff(dir, &c, *staged == "1", o)?) == *want {
        true => Ok(String::new()),
        false => Err("The file changed since this diff was shown, so nothing was applied. Check the updated diff and try again.".into()),
    }
}

/// `git apply` of a partial patch built from `f`, the diff of `src` loaded with `opts`
/// (`--cached`/`--reverse` in `args`). Re-checked when it runs: a confirm dialog or stale diff may outlive the file's content,
/// and `--recount` would land the hunk on an identical block elsewhere.
/// A commit's diff (`src`) goes to the working tree instead: [`hunk_op`].
pub fn patch_op(label: &str, src: &DiffSrc, opts: DiffOpts, f: &FileDiff, patch: String, args: &[&str]) -> Op {
    let DiffSrc::Worktree { change: c, staged } = src else { return hunk_op(label, src.change(), f, patch, args) };
    let (context, code, hunks) = (opts.context.to_string(), c.code.to_string(), shape(f));
    let guard = [DIFF_IS, if *staged { "1" } else { "0" }, &context, &code, &c.path, c.old_path.as_deref().unwrap_or(""), &hunks];
    let mut a = vec!["apply", "--recount", "--whitespace=nowarn"];
    a.extend_from_slice(args);
    a.push("-");
    let mut op = Op::new(label, &guard);
    if c.code == '?' {
        // Untracked: record an empty intent-to-add entry for the patch to fill in. If anything fails
        // after that, drop the entry again so the file is untracked as before.
        op = op.then(&["--literal-pathspecs", "add", "-N", "--", &c.path]);
        op.on_fail = vec![["--literal-pathspecs", "reset", "-q", "--", &c.path].map(String::from).to_vec()];
    }
    let op = op.then(&a).input(patch).light();
    match label.starts_with("Discard") {
        true => op.backup(Backup::Paths(vec![c.path.clone()])).confirm("Discard the selected changes? (Undo with ⌘Z.)"),
        false => op,
    }
}

/// A commit's hunk or lines applied to the working tree (`--reverse`: reverted), backed up.
/// Nothing to guard: a commit's diff never changes.
fn hunk_op(label: &str, c: &git::Change, f: &FileDiff, mut patch: String, args: &[&str]) -> Op {
    // A whole rename's patch would also rename the file back (or again): edit the new path only.
    if f.header.iter().any(|h| h.starts_with("rename from ") || h.starts_with("copy from "))
        && let (Some(h), Some(at)) = (git::plain_header(f), patch.find("\n@@ "))
    {
        patch.replace_range(..=at, &h);
    }
    // A partial patch has no blob ids, which `--3way` needs; its preimage is one side of the
    // commit's file (unpicked lines stay as they were there), so the commit's ids fit.
    if let (Some(ix), false) = (f.header.iter().find(|h| h.starts_with("index ")), patch.contains("\nindex ")) {
        let at = patch.find('\n').map_or(0, |i| i + 1);
        patch.insert_str(at, &format!("{ix}\n"));
    }
    // A copy's patch leaves its source alone; a rename's may edit the old path (`apply_hunk`).
    let paths: Vec<String> = c.old_path.iter().filter(|_| c.code == 'R').chain([&c.path]).cloned().collect();
    let mut a = vec![APPLY_HUNK];
    a.extend_from_slice(args);
    a.push("--");
    a.extend(paths.iter().map(String::as_str));
    Op::new(label, &a).input(patch).backup(Backup::Paths(paths))
}

/// `exec` pseudo-command: `APPLY_HUNK [apply args] -- paths`, the patch as input. If the lines
/// around it changed since its commit, a 3-way merge, which may leave conflict markers.
pub(super) const APPLY_HUNK: &str = "!apply-hunk";

pub(super) fn apply_hunk(dir: &std::path::Path, a: &[&str], patch: &[u8]) -> Res<String> {
    let split = a.iter().position(|x| *x == "--").unwrap_or(a.len());
    let (args, paths) = (&a[1..split], a.get(split + 1..).unwrap_or_default());
    // A rename's hunk picked where the file still has its old name: edit that one.
    let retargeted;
    let patch = match paths {
        [old, new] if !args.contains(&"--reverse") && !dir.join(new).exists() && dir.join(old).is_file() && !old.contains(['"', '\\', '\t', '\n']) => {
            let text = String::from_utf8_lossy(patch);
            let (head, body) = text.split_at(text.find("\n@@ ").map_or(0, |i| i + 1));
            let index = head.lines().find(|l| l.starts_with("index ")).map(|l| format!("{l}\n")).unwrap_or_default();
            retargeted = format!("diff --git a/{old} b/{old}\n{index}--- a/{old}\n+++ b/{old}\n{body}");
            retargeted.as_bytes()
        }
        _ => patch,
    };
    let apply = |more: Option<&str>| {
        let mut cmd = vec!["apply", "--recount", "--whitespace=nowarn"];
        cmd.extend(args);
        cmd.extend(more);
        cmd.push("-");
        git::run_input(dir, &cmd, Some(patch))
    };
    // Decided by exit status, not git's (possibly translated) messages.
    if apply(Some("--check")).is_ok() {
        return apply(None).map(|_| String::new());
    }
    let with = |cmd: &[&'static str]| [cmd, paths].concat();
    if !git::run(dir, &with(&["--literal-pathspecs", "status", "--porcelain", "--"]))?.is_empty() {
        return Err(format!(
            "It doesn't apply cleanly: the lines around it changed since that commit. Commit or stash your changes to {} so Glasir can merge it in (3-way).",
            paths.join(", ")
        ));
    }
    let merged = apply(Some("--3way"));
    let conflicts = merged.is_err() && !git::run(dir, &with(&["--literal-pathspecs", "diff", "--name-only", "--diff-filter=U", "--"]))?.is_empty();
    // `--3way` writes the index too; the files were clean, so HEAD has what it held.
    git::run(dir, &with(&["--literal-pathspecs", "reset", "-q", "--"]))?;
    match merged {
        Ok(_) => Ok("merged (3-way): the lines around it had changed".into()),
        Err(_) if conflicts => {
            Err("It doesn't apply cleanly, so it was merged in with conflict markers (<<<<<<<): resolve them in the file, or undo with ⌘Z.".into())
        }
        Err(e) => Err(format!("It doesn't apply cleanly:\n{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{commit, identity, scratch, settle, sh};
    use super::super::{Action, DiffSrc, RepoTab, View};
    use super::*;

    /// A Discard hunk confirmed after the file changed must not revert an identical block
    /// elsewhere (`--recount` searches for the context); same for Stage hunk on a stale diff.
    #[test]
    fn stale_patch_refused() {
        let dir = scratch("stale-patch");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let block = |x: &str| format!("ctx1\nctx2\nctx3\n{x}\nctx4\nctx5\nctx6\n");
        let file = |a: &str, b: &str| format!("top\n{}{}{}end\n", block(a), "gap\n".repeat(10), block(b));
        commit(&dir, "f", &file("x", "x"));
        std::fs::write(dir.join("f"), file("EDIT", "x")).unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let change = t.data.as_ref().unwrap().status.unstaged[0].clone();
        t.apply(&ctx, Action::Diff(DiffSrc::Worktree { change, staged: false }));
        settle(&ctx, &mut t);
        while t.diff.as_ref().unwrap().loading() {
            t.poll_diff();
        }
        let ds = t.diff.as_ref().unwrap();
        let f = ds.file.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(f.hunks.len(), 1);
        let patch = |rev| git::partial_patch(f, |_, _| true, rev).unwrap();
        let mut discard = patch_op("Discard hunk", &ds.src, ds.opts, f, patch(true), &["--reverse"]);
        let stage = patch_op("Stage hunk", &ds.src, ds.opts, f, patch(false), &["--cached"]);
        discard.confirm = None;
        // Meanwhile the edit moved to the second, identical block.
        std::fs::write(dir.join("f"), file("x", "EDIT")).unwrap();
        for op in [&discard, &stage] {
            assert!(super::super::op::exec(&dir, op).is_err(), "{}", op.label);
        }
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), file("x", "EDIT"));
        assert_eq!(sh(&dir, &["diff", "--cached"]), "");
        // Unchanged since the diff was shown: applies.
        std::fs::write(dir.join("f"), file("EDIT", "x")).unwrap();
        super::super::op::exec(&dir, &stage).unwrap();
        assert_eq!(sh(&dir, &["show", ":f"]), file("EDIT", "x"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Part of an untracked file is staged via intent-to-add; a failure puts it back untracked, a
    /// stale or no-longer-untracked file is refused, and the rest shows as an ordinary unstaged edit.
    #[test]
    fn untracked_line_staging() {
        let dir = scratch("untracked-lines");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "base", "b\n");
        let name = "new [1].txt";
        std::fs::write(dir.join(name), "one\ntwo\nthree\nfour\n").unwrap();
        let c = git::Change { path: name.into(), old_path: None, code: '?' };
        let o = DiffOpts::default();
        let f = git::worktree_diff(&dir, &c, false, o).unwrap();
        let src = DiffSrc::Worktree { change: c.clone(), staged: false };
        let op = |keep: &[usize]| patch_op("Stage lines", &src, o, &f, git::partial_patch(&f, |_, l| keep.contains(&l), false).unwrap(), &["--cached"]);

        // A patch that no longer applies (the lines changed under the guard's feet) rolls the entry back.
        let mut bad = op(&[0, 2]);
        bad.input = Some(b"not a patch".to_vec());
        assert!(super::super::op::exec(&dir, &bad).is_err());
        assert_eq!(sh(&dir, &["status", "--porcelain", "-z"]), format!("?? {name}\0"));

        // File edited meanwhile: refused, nothing recorded.
        std::fs::write(dir.join(name), "one\nTWO\nthree\nfour\n").unwrap();
        assert!(super::super::op::exec(&dir, &op(&[0, 2])).is_err());
        assert_eq!(sh(&dir, &["ls-files", "--", name]), "");
        std::fs::write(dir.join(name), "one\ntwo\nthree\nfour\n").unwrap();

        super::super::op::exec(&dir, &op(&[0, 2])).unwrap();
        assert_eq!(sh(&dir, &["show", &format!(":{name}")]), "one\nthree\n");
        assert_eq!(std::fs::read_to_string(dir.join(name)).unwrap(), "one\ntwo\nthree\nfour\n");
        // No longer untracked: the same op is refused (and its rollback must not unstage anything).
        assert!(super::super::op::exec(&dir, &op(&[1])).is_err());
        assert_eq!(sh(&dir, &["show", &format!(":{name}")]), "one\nthree\n");
        let st = git::status(&dir).unwrap();
        assert_eq!((st.staged[0].code, st.unstaged[0].code), ('A', 'M'));
        // The rest is an ordinary diff now; unstaging it all makes the file untracked again.
        let f2 = git::worktree_diff(&dir, &st.unstaged[0], false, o).unwrap();
        assert_eq!(f2.hunks[0].lines.iter().filter(|l| l.kind == b'+').count(), 2);
        sh(&dir, &["reset", "-q", "--", name]);
        assert_eq!(git::status(&dir).unwrap().unstaged[0].code, '?');
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Part of an untracked file stages its exact bytes: a missing final newline stays missing,
    /// and a file `git add` would convert (CRLF, a clean filter) stages whole files only.
    #[test]
    fn untracked_partial_keeps_bytes() {
        let dir = scratch("untracked-bytes");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        sh(&dir, &["config", "core.autocrlf", "false"]);
        commit(&dir, "base", "b\n");
        std::fs::write(dir.join(".gitattributes"), "*.dat filter=x\n").unwrap();
        let o = DiffOpts::default();
        let stage = |name: &str, body: &str, keep: &[usize]| {
            std::fs::write(dir.join(name), body).unwrap();
            let c = git::Change { path: name.into(), old_path: None, code: '?' };
            let f = git::worktree_diff(&dir, &c, false, o).unwrap();
            let src = DiffSrc::Worktree { change: c, staged: false };
            let p = git::partial_patch(&f, |_, l| keep.contains(&l), false)?;
            super::super::op::exec(&dir, &patch_op("Stage lines", &src, o, &f, p, &["--cached"])).unwrap();
            Some(sh(&dir, &["show", &format!(":{name}")]))
        };
        assert_eq!(stage("n.txt", "one\ntwo\nlast", &[0, 2]).as_deref(), Some("one\nlast"));
        assert_eq!(stage("m.txt", "one\ntwo\nlast", &[0, 1]).as_deref(), Some("one\ntwo\n"));
        assert_eq!(stage("w.txt", "one\r\ntwo\r\nthree\r\n", &[0, 2]), None);
        assert_eq!(stage("f.dat", "one\ntwo\nthree\n", &[0, 2]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The guard covers the diff's header too: a mode change undone since isn't staged.
    #[cfg(unix)]
    #[test]
    fn stale_mode_change_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("stale-mode");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "a\n");
        let chmod = |m| std::fs::set_permissions(dir.join("f"), std::fs::Permissions::from_mode(m)).unwrap();
        std::fs::write(dir.join("f"), "b\n").unwrap();
        chmod(0o755);
        let c = git::Change { path: "f".into(), old_path: None, code: 'M' };
        let o = DiffOpts::default();
        let f = git::worktree_diff(&dir, &c, false, o).unwrap();
        assert!(f.header.iter().any(|h| h == "new mode 100755"), "{:?}", f.header);
        let src = DiffSrc::Worktree { change: c, staged: false };
        let stage = patch_op("Stage hunk", &src, o, &f, git::partial_patch(&f, |_, _| true, false).unwrap(), &["--cached"]);
        chmod(0o644);
        assert!(super::super::op::exec(&dir, &stage).is_err());
        assert_eq!(sh(&dir, &["diff", "--cached"]), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn repo(name: &str) -> std::path::PathBuf {
        let dir = scratch(name);
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        dir
    }

    /// A permission change has no hunks but is described; a changed binary file shows both sizes.
    #[cfg(unix)]
    #[test]
    fn mode_and_binary_changes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = repo("mode-binary");
        commit(&dir, "f", "a\n");
        std::fs::write(dir.join("bin"), b"\0\x01\x02").unwrap();
        sh(&dir, &["add", "bin"]);
        sh(&dir, &["commit", "-q", "-m", "bin"]);
        std::fs::set_permissions(dir.join("f"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let change = |p: &str, code| git::Change { path: p.into(), old_path: None, code };
        let f = git::worktree_diff(&dir, &change("f", 'M'), false, DiffOpts::default()).unwrap();
        assert_eq!((f.hunks.len(), f.mode_change()), (0, Some(("100644", "100755"))));
        std::fs::write(dir.join("bin"), b"\0\x01\x02\x03\x04").unwrap();
        let f = git::worktree_diff(&dir, &change("bin", 'M'), false, DiffOpts::default()).unwrap();
        assert_eq!((f.binary, f.sizes), (true, Some((Some(3), Some(5)))));
        sh(&dir, &["add", "bin"]);
        sh(&dir, &["commit", "-q", "-m", "bin2"]);
        let f = git::commit_diff(&dir, "HEAD", Some("HEAD~1"), &change("bin", 'M'), DiffOpts::default()).unwrap();
        assert_eq!(f.sizes, Some((Some(3), Some(5))));
        // An untracked binary file has only a new side.
        std::fs::write(dir.join("new"), b"\0\0").unwrap();
        let f = git::worktree_diff(&dir, &change("new", '?'), false, DiffOpts::default()).unwrap();
        assert_eq!(f.sizes, Some((None, Some(2))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Over 5,000 lines the diff is withheld until loaded with `all`.
    #[test]
    fn large_diff_guard() {
        let dir = repo("large");
        let text = |x: &str| (0..6000).map(|i| format!("{x}{i}\n")).collect::<String>();
        std::fs::write(dir.join("f"), text("a")).unwrap();
        sh(&dir, &["add", "f"]);
        sh(&dir, &["commit", "-qm", "f"]); // not the text: Windows caps a command line at 32K
        std::fs::write(dir.join("f"), text("b")).unwrap();
        std::fs::write(dir.join("u"), text("c")).unwrap();
        let c = |p: &str, code| git::Change { path: p.into(), old_path: None, code };
        for (change, lines) in [(c("f", 'M'), 12_000), (c("u", '?'), 6_000)] {
            let f = git::worktree_diff(&dir, &change, false, DiffOpts::default()).unwrap();
            assert!(f.hunks.is_empty() && matches!(f.large, Some((l, _)) if l >= lines), "{:?}", f.large);
            let f = git::worktree_diff(&dir, &change, false, DiffOpts { all: true, ..Default::default() }).unwrap();
            assert!(f.large.is_none() && f.hunks.iter().map(|h| h.lines.len()).sum::<usize>() >= lines);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Compare with the working tree: committed and uncommitted changes since a ref, per file.
    #[test]
    fn compare_working_tree() {
        let dir = repo("compare-wt");
        commit(&dir, "a", "1\n");
        commit(&dir, "b", "x\n");
        sh(&dir, &["tag", "base"]);
        commit(&dir, "c", "new\n");
        std::fs::write(dir.join("a"), "1\n2\n").unwrap();
        std::fs::write(dir.join("untracked"), "u\n").unwrap();
        let info = git::compare_info(&dir, "base", git::WORKTREE, false, "wt").unwrap();
        let mut names: Vec<_> = info.files.iter().map(|c| (c.path.as_str(), c.code)).collect();
        names.sort();
        assert_eq!(names, [("a", 'M'), ("c", 'A')]);
        assert_eq!(info.id, git::WORKTREE);
        let base = info.parents[0].as_str();
        let f = git::commit_diff(&dir, &info.id, Some(base), &info.files.iter().find(|c| c.path == "a").unwrap().clone(), DiffOpts::default()).unwrap();
        assert_eq!(f.hunks[0].lines.iter().filter(|l| l.kind == b'+').count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A working-tree compare (file list and open diff) follows edits on disk, which only
    /// trigger a status reload.
    #[test]
    fn compare_working_tree_follows_edits() {
        let dir = repo("compare-wt-live");
        commit(&dir, "a", "1\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let wait = |t: &mut RepoTab| {
            super::super::testutil::wait_for("the diff", || {
                settle(&ctx, t);
                t.info_task.is_none() && !t.diff.as_ref().is_some_and(DiffState::loading)
            });
        };
        t.apply(&ctx, Action::Select(super::super::Sel::Compare { from: "HEAD".into(), to: git::WORKTREE.into(), three_dot: false }));
        wait(&mut t);
        assert!(t.info.as_ref().unwrap().as_ref().unwrap().files.is_empty());
        std::fs::write(dir.join("a"), "1\n2\n").unwrap();
        t.reload(&ctx, false);
        wait(&mut t);
        let files = &t.info.as_ref().unwrap().as_ref().unwrap().files;
        assert_eq!(files.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["a"]);
        let change = files[0].clone();
        t.apply(&ctx, Action::Diff(DiffSrc::Commit { id: git::WORKTREE.into(), parent: Some("HEAD".into()), change }));
        wait(&mut t);
        std::fs::write(dir.join("a"), "1\n2\n3\n").unwrap();
        t.reload(&ctx, false);
        wait(&mut t);
        let f = t.diff.as_ref().unwrap().file.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(f.hunks[0].lines.iter().filter(|l| l.kind == b'+').count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Revert / cherry-pick a commit's hunk or lines into the working tree, end to end: applied,
    /// undone with ⌘Z, 3-way merged when the context moved on, refused over local edits.
    #[test]
    fn hunks_from_a_commit() {
        let dir = scratch("commit-hunks");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let lines = |edits: &[(usize, &str)]| {
            let mut l: Vec<String> = (1..=20).map(|i| format!("l{i}")).collect();
            edits.iter().for_each(|(i, t)| l[i - 1] = t.to_string());
            l.join("\n") + "\n"
        };
        commit(&dir, "f", &lines(&[]));
        commit(&dir, "f", &lines(&[(2, "TWO"), (3, "THREE"), (15, "FIFTEEN")]));
        let c2 = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let change = git::Change { path: "f".into(), old_path: None, code: 'M' };
        let src = DiffSrc::Commit { id: c2.clone(), parent: Some(format!("{c2}^")), change: change.clone() };
        let f = git::commit_diff(&dir, &c2, Some(&format!("{c2}^")), &change, DiffOpts::default()).unwrap();
        assert_eq!(f.hunks.len(), 2);
        let op = |label: &str, hi: usize, li: Option<usize>, rev: bool| {
            let p = git::partial_patch(&f, |h, l| h == hi && li.is_none_or(|x| x == l), rev).unwrap();
            patch_op(label, &src, DiffOpts::default(), &f, p, if rev { &["--reverse"] } else { &[] })
        };
        let read = || std::fs::read_to_string(dir.join("f")).unwrap();
        let run = |t: &mut RepoTab, a: Action| {
            t.apply(&ctx, a);
            settle(&ctx, t)
        };

        // Revert hunk: only the working tree changes, ⌘Z restores it.
        run(&mut t, Action::Git(op("Revert hunk", 0, None, true)));
        assert_eq!(read(), lines(&[(15, "FIFTEEN")]));
        assert_eq!(sh(&dir, &["diff", "--cached"]), "");
        run(&mut t, Action::Undo);
        assert_eq!(read(), lines(&[(2, "TWO"), (3, "THREE"), (15, "FIFTEEN")]));
        // Revert lines: just `-l2 +TWO` of the first hunk (lines 1..=4 are `l1 -l2 -l3 +TWO`).
        let li = f.hunks[0].lines.iter().position(|l| l.text == "TWO").unwrap();
        let l2 = f.hunks[0].lines.iter().position(|l| l.text == "l2").unwrap();
        let p = git::partial_patch(&f, |h, l| h == 0 && (l == li || l == l2), true).unwrap();
        run(&mut t, Action::Git(patch_op("Revert lines", &src, DiffOpts::default(), &f, p, &["--reverse"])));
        assert_eq!(read(), lines(&[(3, "THREE"), (15, "FIFTEEN")]));
        run(&mut t, Action::Undo);

        sh(&dir, &["checkout", "-q", "-b", "side", &format!("{c2}^")]);
        run(&mut t, Action::Git(op("Cherry-pick hunk", 1, None, false)));
        assert_eq!(read(), lines(&[(15, "FIFTEEN")]));
        sh(&dir, &["checkout", "-q", "--", "f"]);
        sh(&dir, &["checkout", "-q", "main"]);

        // The context moved on (l6 edited since): a 3-way merge, the index left as it was.
        commit(&dir, "f", &lines(&[(2, "TWO"), (3, "THREE"), (6, "SIX"), (15, "FIFTEEN")]));
        let msgs = run(&mut t, Action::Git(op("Revert hunk", 0, None, true)));
        assert!(msgs.iter().any(|m| !m.error && m.text.contains("3-way")), "{:?}", msgs.iter().map(|m| &m.text).collect::<Vec<_>>());
        assert_eq!(read(), lines(&[(6, "SIX"), (15, "FIFTEEN")]));
        assert_eq!(sh(&dir, &["diff", "--cached"]), "");
        run(&mut t, Action::Undo);
        assert_eq!(sh(&dir, &["status", "--porcelain"]), "");
        // Lines too: the patch borrows the commit's blob ids for the merge.
        let p = git::partial_patch(&f, |h, l| h == 0 && (l == li || l == l2), true).unwrap();
        run(&mut t, Action::Git(patch_op("Revert lines", &src, DiffOpts::default(), &f, p, &["--reverse"])));
        assert_eq!(read(), lines(&[(3, "THREE"), (6, "SIX"), (15, "FIFTEEN")]));
        run(&mut t, Action::Undo);

        // A conflicting edit since: conflict markers in the file (not an unmerged index); ⌘Z undoes.
        let head = lines(&[(2, "Two!"), (3, "THREE"), (6, "SIX"), (15, "FIFTEEN")]);
        commit(&dir, "f", &head);
        let msgs = run(&mut t, Action::Git(op("Revert hunk", 0, None, true)));
        assert!(msgs.iter().any(|m| m.error && m.text.contains("conflict markers")));
        assert!(read().contains("<<<<<<<"));
        assert_eq!(sh(&dir, &["status", "--porcelain"]), " M f\n");
        run(&mut t, Action::Undo);
        assert_eq!(read(), head);

        // Local edits to the file: no 3-way over them, nothing touched.
        let mine = head.replace("l10", "mine");
        std::fs::write(dir.join("f"), &mine).unwrap();
        let msgs = run(&mut t, Action::Git(op("Revert hunk", 0, None, true)));
        assert!(msgs.iter().any(|m| m.error && m.text.contains("Commit or stash your changes to f")));
        assert_eq!(read(), mine);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A hunk op that creates a file (a cherry-picked addition, a reverted rename's old path)
    /// is its own undo step, and ⌘Z removes that file again; a rename's hunk edits only the new path,
    /// or the old one where the file wasn't renamed yet.
    #[test]
    fn hunk_ops_that_create_files_undo() {
        let dir = scratch("hunk-new-file");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let text: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        commit(&dir, "old", &text);
        sh(&dir, &["mv", "old", "new"]);
        std::fs::write(dir.join("new"), text.replace("line 5", "FIVE")).unwrap();
        sh(&dir, &["commit", "-qam", "rename"]);
        let ren = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        commit(&dir, "n", "added\n");
        let add = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let run = |t: &mut RepoTab, a: Action| {
            t.apply(&ctx, a);
            settle(&ctx, t)
        };
        let op = |id: &str, change: git::Change, label: &str, rev: bool| {
            let parent = format!("{id}^");
            let f = git::commit_diff(&dir, id, Some(&parent), &change, DiffOpts::default()).unwrap();
            assert_eq!(f.hunks.len(), 1);
            let p = git::partial_patch(&f, |_, _| true, rev).unwrap();
            Action::Git(patch_op(label, &DiffSrc::Commit { id: id.into(), parent: Some(parent), change }, DiffOpts::default(), &f, p, if rev { &["--reverse"] } else { &[] }))
        };

        let renamed = git::Change { path: "new".into(), old_path: Some("old".into()), code: 'R' };
        run(&mut t, op(&ren, renamed, "Revert hunk", true));
        assert_eq!(sh(&dir, &["status", "--porcelain"]), " M new\n");
        run(&mut t, Action::Undo);
        assert_eq!(sh(&dir, &["status", "--porcelain"]), "");

        sh(&dir, &["checkout", "-q", "-b", "side", &ren]);
        run(&mut t, Action::Git(Op::new("Create branch y", &["branch", "y"]).undo(super::super::Undo::Keep)));
        run(&mut t, op(&add, git::Change { path: "n".into(), old_path: None, code: 'A' }, "Cherry-pick hunk", false));
        assert_eq!(std::fs::read_to_string(dir.join("n")).unwrap(), "added\n");
        assert_eq!(t.undo_label(), Some("Cherry-pick hunk"));
        run(&mut t, Action::Undo);
        assert!(!dir.join("n").exists());
        assert_eq!(t.undo_label(), Some("Create branch y"));

        // Picked onto a branch that still has the old name: the edit lands there.
        sh(&dir, &["checkout", "-q", "-b", "before", &format!("{ren}^")]);
        let renamed = git::Change { path: "new".into(), old_path: Some("old".into()), code: 'R' };
        let msgs = run(&mut t, op(&ren, renamed, "Cherry-pick hunk", false));
        assert_eq!(sh(&dir, &["status", "--porcelain"]), " M old\n", "{:?}", msgs.iter().map(|m| &m.text).collect::<Vec<_>>());
        assert!(std::fs::read_to_string(dir.join("old")).unwrap().contains("FIVE"));
        run(&mut t, Action::Undo);
        assert_eq!(sh(&dir, &["status", "--porcelain"]), "");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
