//! Per-repository state (`RepoTab`): background loading, polling and the
//! `apply` dispatch for the `Action`s the UI emits. No drawing lives here.
//!
//! - `task`: `Task<T>` background values
//! - `data`: `RepoData`/`View` and `load_all`
//! - `action`: `Action`, `Sel`, `DiffSrc`, `Prompt`, `Choice`, `Msg`
//! - `op`: `Op` builder, running/finishing ops, shared op builders
//!   (push/pull/fetch/stash/commit, `branch_checkout`)
//! - `offer`: one-click fixes for failed ops
//! - `undo`: undo/redo and discard backups
//! - `diff`, `ai_job`, `conflicts`, `cleanup`, `fetch`, `watch`, `files`: one concern each
//! - `rebase`: interactive-rebase engine (todo `Editor`, quick/multi/amend_into)
//! - `search`, `history`, `merge_editor`: state behind the graph search bar, the
//!   file history/blame view and the merge editor (drawn by the same-named `ui` files)

mod action;
mod ai_job;
mod cleanup;
mod conflicts;
mod data;
mod diff;
mod fetch;
pub mod files;
pub mod history;
pub mod merge_editor;
mod offer;
mod op;
pub mod rebase;
mod search;
mod task;
#[cfg(test)]
mod testutil;
mod undo;
mod watch;

pub use action::{Action, Choice, DiffSrc, Msg, MultiOp, Prompt, Sel, op, prompt};
pub use data::{RepoData, View};
pub use diff::DiffState;
pub use files::reveal;
pub use op::{Backup, Op, PullMode, Undo, branch_checkout, fetch_op, pull_op, stash_op};
pub use search::Search;
pub use task::Task;

use crate::ai;
use crate::git::{self, CommitInfo, Res};
use data::load_all;
use op::Done;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use undo::UndoEntry;

pub const PAGE: usize = 1000;

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
    /// Graph filter; the app persists it per repo.
    pub view: View,
    pub ai: Option<ai::Job>,
    /// The commit message being replaced by an AI suggestion, restored on failure.
    ai_draft: Option<(String, String)>,
    pub ai_cfg: ai::Config,
    /// Messages for the toast area, drained by the app every frame.
    pub notes: Vec<Msg>,
    /// A repository the app should open in a new tab.
    pub open_tab: Option<PathBuf>,
    watcher: Option<(notify::RecommendedWatcher, Receiver<bool>)>,
    watcher_task: Option<Task<Option<(notify::RecommendedWatcher, Receiver<bool>)>>>,
    pub search: Option<Search>,
    /// Extra selected commits (⌘/⇧-click), newest first; empty = single selection.
    pub multi: Vec<String>,
    pub rebase: Option<crate::repo::rebase::Editor>,
    pub merge: Option<merge_editor::Editor>,
    pub file_view: Option<history::FileView>,
    /// Pending drag-and-drop menu: (source branch, target branch, where).
    pub drop_menu: Option<(String, String, egui::Pos2)>,
    /// Branch cleanup candidates: (name, reason, selected).
    pub cleanup: Option<Vec<(String, String, bool)>>,
    /// Predicted conflicts with the target branch: (target, files).
    pub conflicts: Option<(String, Vec<String>)>,
    conflict_task: Option<Task<Option<(String, Vec<String>)>>>,
    /// A failed op offering fixes (autostash, diverged pull, rejected push).
    pub choice: Option<Choice>,
    bg_fetch: Option<Task<Res<String>>>,
    /// `ctx` time of the last background fetch.
    last_fetch: f64,
    /// Why the last background fetch failed.
    pub fetch_failed: Option<String>,
    /// Debounced filesystem change: (needs full reload, time of last event).
    changed: Option<(bool, f64)>,
}

impl RepoTab {
    pub fn new(ctx: &egui::Context, dir: PathBuf, max_commits: usize, view: View) -> Self {
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
            view,
            ai: None,
            ai_draft: None,
            ai_cfg: ai::Config::default(),
            notes: vec![],
            open_tab: None,
            watcher: None,
            watcher_task: None,
            changed: None,
            search: None,
            multi: vec![],
            rebase: None,
            merge: None,
            file_view: None,
            drop_menu: None,
            cleanup: None,
            conflicts: None,
            conflict_task: None,
            choice: None,
            bg_fetch: None,
            last_fetch: f64::NEG_INFINITY,
            fetch_failed: None,
        };
        // Registering watches walks the whole tree (inotify): keep it off the UI thread.
        let (c, d) = (ctx.clone(), t.dir.clone());
        t.watcher_task = Some(Task::spawn(ctx, move || watch::watch(&c, &d)));
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
        let (limit, view) = (self.max_commits, self.view.clone());
        let cached = self.data.as_ref().filter(|d| !full && d.view == view).map(|d| {
            (d.log.clone(), d.refs.clone(), d.stashes.clone(), d.remotes.clone(), d.git_dir.clone(), d.origin_head.clone(), d.worktrees.clone())
        });
        self.load = Some(Task::spawn(ctx, move || match cached {
            Some((log, refs, st, rm, gd, oh, wt)) => {
                let mut d = RepoData::build(log, refs, st, rm, gd, oh, git::status(&dir)?, view);
                d.worktrees = wt;
                Ok(d)
            }
            None => load_all(&dir, limit, view),
        }));
    }

    pub fn poll(&mut self, ctx: &egui::Context, msgs: &mut Vec<Msg>) {
        if let Some(w) = Task::poll(&mut self.watcher_task) {
            if w.is_none() {
                msgs.push(Msg { text: "Live refresh is off for this repo (file watching unavailable); changes show on focus or F5.".into(), error: false });
            }
            self.watcher = w;
        }
        if let Some((_, rx)) = &self.watcher {
            let now = ctx.input(|i| i.time);
            for full in rx.try_iter() {
                self.changed = Some((self.changed.is_some_and(|c| c.0) || full, now));
            }
            if let Some((full, at)) = self.changed
                && now - at > 0.4
                && self.load.is_none()
                && self.running.is_none()
            {
                self.changed = None;
                self.reload(ctx, full);
            }
        }
        if let Some(res) = Task::poll(&mut self.load) {
            match res {
                Ok(d) => {
                    if self.sel == Some(Sel::Wip) && !d.wip {
                        self.sel = None;
                    }
                    let first = self.data.is_none();
                    let head = d.status.head.clone();
                    // Resolved or aborted elsewhere: the editor's markers are stale.
                    if self.merge.as_ref().is_some_and(|m| !d.status.conflicted.iter().any(|c| c.path == m.path)) {
                        self.merge = None;
                    }
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
            } else {
                self.check_conflicts(ctx);
            }
        }
        if let Some(c) = Task::poll(&mut self.conflict_task) {
            self.conflicts = c;
        }
        if let Some(r) = Task::poll(&mut self.info_task) {
            self.info = Some(r);
        }
        self.poll_ai(ctx, msgs);
        self.poll_diff();
        self.poll_op(ctx, msgs);
    }

    pub fn select(&mut self, ctx: &egui::Context, sel: Sel) {
        self.multi.clear();
        if self.sel.as_ref() != Some(&sel) {
            let dir = self.dir.clone();
            match sel.clone() {
                Sel::Commit(id) => {
                    self.info = None;
                    let stash = self.data.as_ref().is_some_and(|d| d.stashes.iter().any(|s| s.id == id));
                    self.info_task =
                        Some(Task::spawn(ctx, move || if stash { git::stash_info(&dir, &id) } else { git::commit_info(&dir, &id) }));
                }
                Sel::Compare { from, to, three_dot } => {
                    self.info = None;
                    self.diff = None;
                    let label = format!("{to} compared with {from}");
                    self.info_task = Some(Task::spawn(ctx, move || git::compare_info(&dir, &from, &to, three_dot, &label)));
                }
                Sel::Wip => {}
            }
        }
        self.sel = Some(sel);
    }

    pub fn apply(&mut self, ctx: &egui::Context, a: Action) {
        match a {
            Action::Select(s) => self.select(ctx, s),
            Action::Diff(src) => {
                self.merge = None;
                self.open_diff(ctx, src)
            }
            Action::MergeEditor(path) => {
                (self.diff, self.file_view, self.rebase) = (None, None, None);
                self.merge = Some(merge_editor::Editor::open(ctx, &self.dir, &path));
            }
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
            Action::Ai(p) => self.start_ai(ctx, p),
            Action::Error(e) => self.notes.push(Msg { text: e, error: true }),
            Action::Cleanup => self.open_cleanup(),
            Action::Multi(ids) => {
                if let Some(last) = ids.last().cloned() {
                    self.select(ctx, Sel::Commit(last));
                }
                self.multi = if ids.len() > 1 { ids } else { vec![] };
            }
            Action::MultiOp(kind, ids) => self.multi_op(ctx, kind, ids),
            Action::View(v) => {
                self.view = v;
                self.reload(ctx, true);
            }
            Action::OpenTab(p) => self.open_tab = Some(PathBuf::from(p)),
            Action::AmendInto(commit) => {
                let is_head = self.data.as_ref().and_then(|d| d.status.head.as_deref()) == Some(commit.as_str());
                match crate::repo::rebase::amend_into(&self.dir, &commit, is_head) {
                    Ok(op) => self.run(ctx, op),
                    Err(e) => self.notes.push(Msg { text: e, error: true }),
                }
            }
            Action::Rewrite { commit, act, message } => {
                let Some(git_dir) = self.data.as_ref().map(|d| d.git_dir.clone()) else { return };
                match crate::repo::rebase::quick(&self.dir, &git_dir, &commit, act, message) {
                    Ok(op) => self.run(ctx, op),
                    Err(e) => self.notes.push(Msg { text: e, error: true }),
                }
            }
            Action::DropBranch { src, dst } => {
                let at = ctx.input(|i| i.pointer.interact_pos()).unwrap_or_default();
                self.drop_menu = Some((src, dst, at));
            }
            Action::FileView(path, mode) => {
                (self.diff, self.merge) = (None, None);
                self.file_view = Some(history::FileView::open(ctx, &self.dir, &path, mode));
            }
            Action::Rebase { base, label } => match crate::repo::rebase::Editor::load(&self.dir, &base, &label) {
                Ok(ed) => {
                    (self.diff, self.merge) = (None, None);
                    self.rebase = Some(ed);
                }
                Err(e) => self.notes.push(Msg { text: e, error: true }),
            },
            Action::Undo => self.undo_redo(ctx, true),
            Action::Redo => self.undo_redo(ctx, false),
            Action::Reload => self.reload(ctx, true),
            Action::LoadMore => {
                self.max_commits += crate::repo::PAGE;
                self.reload(ctx, true);
            }
            Action::Cancel => {
                if let Some(c) = &self.cancel {
                    c.cancel();
                }
            }
        }
    }
}
