//! Per-repository state (`RepoTab`): background loading, polling and the
//! `apply` dispatch for the `Action`s the UI emits. No drawing lives here.
//!
//! - `task`: `Task<T>` background values
//! - `data`: `RepoData` and `load_all`
//! - `action`: `Action`, `Sel`, `DiffSrc`, `Prompt`, `Choice`, `Msg`
//! - `op`: `Op` builder, running/finishing ops, shared op builders
//!   (push/pull/fetch/stash/commit, `branch_checkout`)
//! - `offer`: one-click fixes for failed ops
//! - `undo`: undo/redo and discard backups
//! - `diff`, `files`: one concern each

mod action;
mod data;
mod diff;
pub mod files;
mod offer;
mod op;
mod task;
#[cfg(test)]
mod testutil;
mod undo;

pub use action::{Action, Choice, DiffSrc, Msg, Prompt, Sel, op, prompt};
pub use data::RepoData;
pub use diff::DiffState;
pub use files::reveal;
pub use op::{Backup, Op, PullMode, Undo, branch_checkout, fetch_op, pull_op, stash_op};
pub use task::Task;

use crate::git::{self, CommitInfo, Res};
use data::load_all;
use op::Done;
use std::path::PathBuf;
use undo::UndoEntry;

pub struct RepoTab {
    pub dir: PathBuf,
    pub name: String,
    pub data: Option<RepoData>,
    pub error: Option<String>,
    load: Option<Task<Res<RepoData>>>,
    /// A reload requested while one was running (`true` = full).
    queued: Option<bool>,
    pub sel: Option<Sel>,
    pub info: Option<Res<CommitInfo>>,
    info_task: Option<Task<Res<CommitInfo>>>,
    pub diff: Option<DiffState>,
    running: Option<(String, Task<Done>)>,
    /// Stops the running op's git command (not set for undo/redo).
    cancel: Option<git::Cancel>,
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    pub summary: String,
    pub body: String,
    pub amend: bool,
    pub filter: String,
    pub prompt: Option<Prompt>,
    pub confirm: Option<Op>,
    pub scroll_to: Option<usize>,
    pub max_commits: usize,
    /// Messages for the toast area, drained by the app every frame.
    pub notes: Vec<Msg>,
    /// Pending drag-and-drop menu: (source branch, target branch, where).
    pub drop_menu: Option<(String, String, egui::Pos2)>,
    /// A failed op offering fixes (autostash, diverged pull, rejected push).
    pub choice: Option<Choice>,
}

impl RepoTab {
    pub fn new(ctx: &egui::Context, dir: PathBuf, max_commits: usize) -> Self {
        let name = dir.file_name().map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into_owned());
        let mut t = RepoTab {
            dir,
            name,
            data: None,
            error: None,
            load: None,
            queued: None,
            sel: None,
            info: None,
            info_task: None,
            diff: None,
            running: None,
            cancel: None,
            undo: vec![],
            redo: vec![],
            summary: String::new(),
            body: String::new(),
            amend: false,
            filter: String::new(),
            prompt: None,
            confirm: None,
            scroll_to: None,
            max_commits,
            notes: vec![],
            drop_menu: None,
            choice: None,
        };
        t.reload(ctx, true);
        t
    }

    pub fn busy(&self) -> Option<&str> {
        self.running.as_ref().map(|r| r.0.as_str())
    }
    pub fn cancellable(&self) -> bool {
        self.cancel.is_some()
    }
    /// Latest `--progress` line of the running op.
    pub fn progress(&self) -> Option<String> {
        self.cancel.as_ref().map(git::Cancel::progress).filter(|p| !p.is_empty())
    }
    pub fn loading(&self) -> bool {
        self.load.is_some()
    }

    /// `full`: log + refs + status; otherwise status only.
    pub fn reload(&mut self, ctx: &egui::Context, full: bool) {
        if self.load.is_some() {
            self.queued = Some(self.queued.unwrap_or(false) || full);
            return;
        }
        let dir = self.dir.clone();
        let limit = self.max_commits;
        let cached = self.data.as_ref().filter(|_| !full).map(|d| {
            (d.log.clone(), d.refs.clone(), d.stashes.clone(), d.remotes.clone(), d.git_dir.clone())
        });
        self.load = Some(Task::spawn(ctx, move || match cached {
            Some((log, refs, st, rm, gd)) => Ok(RepoData::build(log, refs, st, rm, gd, git::status(&dir)?)),
            None => load_all(&dir, limit),
        }));
    }

    pub fn poll(&mut self, ctx: &egui::Context, msgs: &mut Vec<Msg>) {
        if let Some(res) = Task::poll(&mut self.load) {
            match res {
                Ok(d) => {
                    if self.sel == Some(Sel::Wip) && !d.wip {
                        self.sel = None;
                    }
                    let first = self.data.is_none();
                    let head = d.status.head.clone();
                    self.data = Some(d);
                    if self.sel.is_none() {
                        match (self.data.as_ref().is_some_and(|d| d.wip), head) {
                            (true, _) => self.sel = Some(Sel::Wip),
                            (false, Some(h)) if first => self.apply(ctx, Action::Jump(h)),
                            _ => {}
                        }
                    }
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
            if let Some(full) = self.queued.take() {
                self.reload(ctx, full);
            }
        }
        if let Some(r) = Task::poll(&mut self.info_task) {
            self.info = Some(r);
        }
        self.poll_diff();
        self.poll_op(ctx, msgs);
    }

    pub fn select(&mut self, ctx: &egui::Context, sel: Sel) {
        if self.sel.as_ref() != Some(&sel) {
            let dir = self.dir.clone();
            match sel.clone() {
                Sel::Commit(id) => {
                    self.info = None;
                    let stash = self.data.as_ref().is_some_and(|d| d.stashes.iter().any(|s| s.id == id));
                    self.info_task =
                        Some(Task::spawn(ctx, move || if stash { git::stash_info(&dir, &id) } else { git::commit_info(&dir, &id) }));
                }
                Sel::Wip => {}
            }
        }
        self.sel = Some(sel);
    }

    pub fn apply(&mut self, ctx: &egui::Context, a: Action) {
        match a {
            Action::Select(s) => self.select(ctx, s),
            Action::Diff(src) => self.open_diff(ctx, src),
            Action::CloseDiff => self.diff = None,
            Action::Git(op) => self.run(ctx, op),
            Action::Prompt(p) => self.prompt = Some(p),
            Action::Copy(s) => ctx.copy_text(s),
            Action::Url(u) => {
                let _ = open::that_detached(u);
            }
            Action::Reveal(p) => reveal(&self.dir.join(p)),
            Action::Jump(id) => {
                self.scroll_to = self.data.as_ref().and_then(|d| d.row_of.get(&id).copied());
                self.select(ctx, Sel::Commit(id));
            }
            Action::Error(e) => self.notes.push(Msg { text: e, error: true }),
            Action::DropBranch { src, dst } => {
                let at = ctx.input(|i| i.pointer.interact_pos()).unwrap_or_default();
                self.drop_menu = Some((src, dst, at));
            }
            Action::Undo => self.undo_redo(ctx, true),
            Action::Redo => self.undo_redo(ctx, false),
            Action::Reload => self.reload(ctx, true),
            Action::Cancel => {
                if let Some(c) = &self.cancel {
                    c.cancel();
                }
            }
        }
    }
}
