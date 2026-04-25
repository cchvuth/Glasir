//! The open diff: loads a file diff (+ syntax colors) in the background.

use super::action::DiffSrc;
use super::task::Task;
use super::RepoTab;
use crate::git::{self, FileDiff, Res};
use std::collections::BTreeSet;

pub struct DiffState {
    pub src: DiffSrc,
    pub file: Option<Res<FileDiff>>,
    /// Syntax colors per `[hunk][line]` (empty if unknown language).
    pub hl: Vec<Vec<crate::highlight::Spans>>,
    task: Option<Task<Res<(FileDiff, Vec<Vec<crate::highlight::Spans>>)>>>,
    /// Picked change lines `(hunk, line)` for partial staging.
    pub picked: BTreeSet<(usize, usize)>,
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
        let task = Task::spawn(ctx, move || {
            let f = match &s {
                DiffSrc::Worktree { change, staged } => git::worktree_diff(&dir, change, *staged),
                DiffSrc::Commit { id, parent, change } => git::commit_diff(&dir, id, parent.as_deref(), change),
            }?;
            let hl = crate::highlight::highlight(&s.change().path, &f, dark);
            Ok((f, hl))
        });
        let (keep, hl) = match self.diff.take().filter(|d| d.src == src) {
            Some(d) => (d.file, d.hl),
            None => (None, vec![]),
        };
        self.diff = Some(DiffState { src, file: keep, hl, task: Some(task), picked: BTreeSet::new() });
    }

    pub(super) fn poll_diff(&mut self) {
        if let Some(d) = &mut self.diff
            && let Some(r) = Task::poll(&mut d.task)
        {
            d.picked.clear();
            match r {
                Ok((f, hl)) => {
                    d.file = Some(Ok(f));
                    d.hl = hl;
                }
                Err(e) => d.file = Some(Err(e)),
            }
        }
    }
}
