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

/// `exec` pseudo-command: `DIFF_IS staged context code path old_path hunks` fails unless
/// the file's diff still has exactly these hunks.
pub(super) const DIFF_IS: &str = "!diff-is";

pub(super) fn diff_is(dir: &std::path::Path, a: &[&str]) -> Res<String> {
    let [_, staged, context, code, path, old, want] = a else { return Err("bad diff guard".into()) };
    let c = git::Change { path: path.to_string(), old_path: (!old.is_empty()).then(|| old.to_string()), code: code.chars().next().unwrap_or('M') };
    let o = DiffOpts { ignore_ws: false, context: context.parse().unwrap_or(3) };
    match format!("{:?}", git::worktree_diff(dir, &c, *staged == "1", o)?.hunks) == *want {
        true => Ok(String::new()),
        false => Err("The file changed since this diff was shown, so nothing was applied. Check the updated diff and try again.".into()),
    }
}

/// `git apply` of a partial patch built from `f`, the diff of `src` loaded with `opts`
/// (`--cached`/`--reverse` in `args`). Re-checked when it runs: a confirm dialog or stale diff may outlive the file's content,
/// and `--recount` would land the hunk on an identical block elsewhere.
pub fn patch_op(label: &str, src: &DiffSrc, opts: DiffOpts, f: &FileDiff, patch: String, args: &[&str]) -> Op {
    let DiffSrc::Worktree { change: c, staged } = src else { unreachable!("patches apply to worktree diffs") };
    let (context, code, hunks) = (opts.context.to_string(), c.code.to_string(), format!("{:?}", f.hunks));
    let guard = [DIFF_IS, if *staged { "1" } else { "0" }, &context, &code, &c.path, c.old_path.as_deref().unwrap_or(""), &hunks];
    let mut a = vec!["apply", "--recount", "--whitespace=nowarn"];
    a.extend_from_slice(args);
    a.push("-");
    let op = Op::new(label, &guard).then(&a).input(patch).light();
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
}
