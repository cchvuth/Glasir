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
            && matches!(d.src, DiffSrc::Worktree { .. })
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
    let o = DiffOpts { ignore_ws: false, context: context.parse().unwrap_or(3), all: true };
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
pub fn patch_op(label: &str, src: &DiffSrc, opts: DiffOpts, f: &FileDiff, patch: String, args: &[&str]) -> Op {
    let DiffSrc::Worktree { change: c, staged } = src else { unreachable!("patches apply to worktree diffs") };
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
        commit(&dir, "f", &text("a"));
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
}
