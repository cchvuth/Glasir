//! The open diff: loads a file diff (with syntax colors and image sides) in the background.

use super::action::DiffSrc;
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
