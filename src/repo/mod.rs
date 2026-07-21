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
//! - `diff` (+ `image_diff`), `ai_job`, `conflicts`, `cleanup`, `fetch`, `watch`, `files`: one concern each
//! - `rebase`: interactive-rebase engine (todo `Editor`, quick/multi/amend_into)
//! - `search`, `history`, `merge_editor`, `reflog`: state behind the graph search bar, the
//!   file history/blame view, the merge editor and the reflog view (drawn by the same-named `ui` files)
//! - `tags`, `restore`, `spinoff`, `patch`: history tools (move/delete/push tags,
//!   restore a file from a commit, move unpushed commits to a new branch, patches)
//! - `absorb`, `split`: smart rewrites (staged hunks into their commits; guided commit split)
//! - `compose`: Commit Composer (all uncommitted changes -> an AI-planned series of commits)
//! - `remotes`, `doctor`, `identity`: remote management and pull-request checkout, the
//!   per-remote connection doctor, the commit-email guard
//! - `tools`: open in editor / terminal, custom commands (Settings → Tools)
//! - `avatars`: commit authors' GitHub / GitHub Enterprise profile pictures

pub mod absorb;
mod action;
mod ai_job;
pub mod avatars;
mod cleanup;
pub mod compose;
mod conflicts;
mod data;
mod diff;
pub mod doctor;
mod fetch;
pub mod files;
pub mod history;
pub mod identity;
pub mod image_diff;
pub mod merge_editor;
pub mod merge_opts;
pub mod message;
mod offer;
mod op;
mod patch;
pub mod rebase;
pub mod reflog;
pub mod remotes;
pub mod restore;
mod search;
pub mod spinoff;
mod split;
pub mod tags;
mod task;
pub mod tools;
#[cfg(test)]
mod testutil;
mod undo;
mod watch;

pub use action::{Action, Choice, DiffSrc, Msg, MultiOp, Prompt, Sel, op, prompt};
pub use data::{RepoData, View};
pub use diff::{DiffState, patch_op};
pub use files::reveal;
pub use op::{Backup, Op, PullMode, Undo, branch_checkout, delete_branches, discard_files_op, discard_op, drop_stashes, fetch_op, apply_stash_op, pop_stash, pull_op, remove_worktree_and_branch, rename_stash, reset_op, sequencer_op, stage_all_op, stage_files_op, stash_branch, stash_paths_op, stash_prompt, unstage_files_op, unstage_op};
pub use search::Search;
pub use task::Task;

use crate::ai;
use crate::git::{self, CommitInfo, Res};
use data::load_all;
use op::Done;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use undo::UndoEntry;

/// Shown when a repo uses LFS (or an LFS action is picked) without git-lfs.
pub const LFS_MISSING: &str = "This repository uses Git LFS, but git-lfs is not installed: large files stay as small pointer files. Install it from git-lfs.com (or brew/apt/winget install git-lfs), run git lfs install, then restart Glasir.";

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
    /// Whitespace / context options for every diff opened in this tab.
    pub diff_opts: git::DiffOpts,
    running: Option<(String, Task<Done>)>,
    /// Stops the running op's git command (not set for undo/redo).
    cancel: Option<git::Cancel>,
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    pub summary: String,
    pub body: String,
    pub amend: bool,
    /// Commit box options: skip hooks, push right after, `Co-authored-by` (name, email) trailers.
    pub no_verify: bool,
    pub push_after: bool,
    pub coauthors: Vec<(String, String)>,
    /// `commit.template` prefilled into the box: comment lines are stripped on commit.
    pub template: Option<message::Template>,
    /// ⇧⌘↩: commit once the stage-all op finished.
    pub commit_next: bool,
    /// ⇧⌘M: focus the summary field.
    pub focus_summary: bool,
    /// The commit box's "Sign" toggle; `None` follows commit.gpgSign.
    pub sign: Option<bool>,
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
    /// Selected files of the WIP lists (⌘/⇧-click) as (staged, path): the target of S / U / Discard.
    pub file_sel: Vec<(bool, String)>,
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
    /// `git verify-tag` running for the tag menu, its result toasted.
    verify_task: Option<Task<Msg>>,
    /// A failed op offering fixes (autostash, diverged pull, rejected push).
    pub choice: Option<Choice>,
    bg_fetch: Option<Task<Res<String>>>,
    /// `ctx` time of the last background fetch.
    last_fetch: f64,
    /// Why the last background fetch failed.
    pub fetch_failed: Option<String>,
    /// Debounced filesystem change: (needs full reload, time of last event).
    changed: Option<(bool, f64)>,
    /// The reflog view, replacing the graph while open.
    pub reflog: Option<reflog::Reflog>,
    /// A patch being exported: (text to copy, toast).
    patch_task: Option<Task<Res<(Option<String>, String)>>>,
    /// Absorb preview (hunk -> target commit), shown as a dialog.
    pub absorb: Option<absorb::Plan>,
    absorb_task: Option<Task<Res<absorb::Plan>>>,
    /// A guided commit split in progress (banner in the WIP panel).
    pub split: Option<split::Split>,
    /// Commit Composer plan; its dialog shows once Claude's commits are in.
    pub compose: Option<compose::Plan>,
    compose_task: Option<Task<Res<(compose::Plan, String)>>>,
    pub remote_form: Option<remotes::Form>,
    /// The connection doctor window for one remote.
    pub doctor: Option<doctor::Doctor>,
    /// Settings → commit email rules (`host -> @domain` lines), mirrored every frame.
    pub email_rules: String,
    pub avatars: avatars::Avatars,
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
            diff_opts: Default::default(),
            running: None,
            cancel: None,
            undo: vec![],
            redo: vec![],
            summary: String::new(),
            body: String::new(),
            amend: false,
            no_verify: false,
            push_after: false,
            coauthors: vec![],
            template: None,
            commit_next: false,
            focus_summary: false,
            sign: None,
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
            file_sel: vec![],
            rebase: None,
            merge: None,
            file_view: None,
            drop_menu: None,
            cleanup: None,
            conflicts: None,
            conflict_task: None,
            verify_task: None,
            choice: None,
            bg_fetch: None,
            last_fetch: f64::NEG_INFINITY,
            fetch_failed: None,
            reflog: None,
            patch_task: None,
            absorb: None,
            absorb_task: None,
            split: None,
            compose: None,
            compose_task: None,
            remote_form: None,
            doctor: None,
            email_rules: String::new(),
            avatars: Default::default(),
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
        // The compared refs may have moved: re-query, keeping the old list until then.
        if full && let Some(Sel::Compare { from, to, three_dot }) = self.sel.clone() {
            self.compare_query(ctx, from, to, three_dot);
        }
        if self.load.is_some() {
            self.queued = Some(self.queued.unwrap_or(false) || full);
            return;
        }
        let dir = self.dir.clone();
        let (limit, view) = (self.max_commits, self.view.clone());
        let cached = self.data.as_ref().filter(|d| !full && d.view == view).map(|d| {
            (d.log.clone(), d.refs.clone(), d.stashes.clone(), d.remotes.clone(), d.git_dir.clone(), d.origin_head.clone(), (d.worktrees.clone(), d.submodules.clone(), d.lfs.clone(), d.config.clone(), d.author.clone(), d.truncated))
        });
        self.load = Some(Task::spawn(ctx, move || match cached {
            Some((log, refs, st, rm, gd, oh, (wt, sm, lfs, cfg, au, tr))) => {
                let mut d = RepoData::build(log, refs, st, rm, gd, oh, git::status(&dir)?, view);
                (d.worktrees, d.submodules, d.lfs, d.config, d.author, d.truncated) = (wt, sm, lfs, cfg, au, tr);
                d.load_bisect(&dir);
                d.load_conflicts(&dir);
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
                    if first && d.lfs.used && !d.lfs.installed {
                        msgs.push(Msg { text: LFS_MISSING.into(), error: true });
                    }
                    let head = d.status.head.clone();
                    // Bisect checked out the next candidate: select and centre it.
                    let bisect_moved = d.state == git::State::Bisecting && self.data.as_ref().is_some_and(|o| o.status.head != head);
                    // Resolved or aborted elsewhere: the editor's markers are stale.
                    if self.merge.as_ref().is_some_and(|m| !d.status.conflicted.iter().any(|c| c.path == m.path)) {
                        self.merge = None;
                    }
                    self.data = Some(d);
                    if first {
                        self.prefill_template();
                    }
                    if bisect_moved && let Some(h) = &head {
                        self.apply(ctx, Action::Jump(h.clone()));
                    }
                    if self.sel.is_none() {
                        match (self.data.as_ref().is_some_and(|d| d.wip), head) {
                            (true, _) => self.sel = Some(Sel::Wip),
                            (false, Some(h)) if first => self.apply(ctx, Action::Jump(h)),
                            _ => {}
                        }
                    }
                    self.error = None;
                    // Outside edits too (watcher / focus / F5): staging from a stale diff
                    // would stage content the file no longer has.
                    self.refresh_diff(ctx);
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
        msgs.extend(Task::poll(&mut self.verify_task));
        if let Some(r) = Task::poll(&mut self.info_task) {
            self.info = Some(r);
        }
        if let Some(d) = &mut self.doctor {
            d.poll();
        }
        if let Some(d) = &self.data {
            self.avatars.sync(ctx, &self.dir, d);
        }
        self.poll_compose(ctx);
        self.poll_ai(ctx, msgs);
        self.poll_patch(ctx);
        self.poll_absorb();
        self.poll_diff();
        self.poll_op(ctx, msgs);
    }

    pub fn select(&mut self, ctx: &egui::Context, sel: Sel) {
        self.multi.clear();
        // A repeated Compare re-queries: its ref names may point elsewhere now.
        if self.sel.as_ref() != Some(&sel) || matches!(sel, Sel::Compare { .. }) {
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
                    self.compare_query(ctx, from, to, three_dot);
                }
                Sel::Wip => {}
            }
        }
        self.sel = Some(sel);
    }

    fn compare_query(&mut self, ctx: &egui::Context, from: String, to: String, three_dot: bool) {
        let (dir, label) = (self.dir.clone(), format!("{to} compared with {from}"));
        self.info_task = Some(Task::spawn(ctx, move || git::compare_info(&dir, &from, &to, three_dot, &label)));
    }

    pub fn apply(&mut self, ctx: &egui::Context, a: Action) {
        match a {
            Action::Select(s) => self.select(ctx, s),
            // Clicking the file that is already open closes it again.
            Action::Diff(src) if self.merge.is_none() && self.diff.as_ref().is_some_and(|d| d.src == src) => self.diff = None,
            Action::Diff(src) => {
                self.merge = None;
                self.open_diff(ctx, src)
            }
            Action::MergeEditor(path) => {
                (self.diff, self.file_view, self.rebase) = (None, None, None);
                self.merge = Some(merge_editor::Editor::open(ctx, &self.dir, &path));
            }
            Action::CloseDiff => self.diff = None,
            Action::DiffOpts(o) => {
                self.diff_opts = o;
                if let Some(d) = &self.diff {
                    self.open_diff(ctx, d.src.clone());
                }
            }
            Action::Git(op) => self.run(ctx, op),
            Action::Prompt(p) => self.prompt = Some(p),
            Action::VerifyTag(full) => {
                let dir = self.dir.clone();
                self.verify_task = Some(Task::spawn(ctx, move || {
                    let (ok, out) = git::verify_tag(&dir, &full);
                    let name = full.strip_prefix("refs/tags/").unwrap_or(&full);
                    Msg { text: format!("Tag {name}: {}\n{out}", if ok { "good signature" } else { "not verified" }), error: !ok }
                }));
            }
            Action::Copy(s) => ctx.copy_text(s),
            Action::Url(u) => {
                let _ = open::that_detached(u);
            }
            // The repo itself opens in the file manager; a file is revealed in its folder.
            Action::Reveal(p) if p.is_empty() => {
                let _ = open::that_detached(&self.dir);
            }
            Action::Reveal(p) => reveal(&self.dir.join(p)),
            Action::Editor(p) => {
                if let Err(e) = tools::open_editor(&self.dir, &self.dir.join(p)) {
                    self.notes.push(Msg { text: e, error: true });
                }
            }
            Action::Terminal => {
                if let Err(e) = tools::open_terminal(&self.dir) {
                    self.notes.push(Msg { text: e, error: true });
                }
            }
            Action::Custom(c, v) => {
                match tools::op(&c, &self.dir, &v) {
                    Ok(op) => self.run(ctx, op),
                    Err(e) => self.notes.push(Msg { text: e, error: true }),
                }
            }
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
            Action::OpenTab(p) => self.open_tab = Some(self.dir.join(p)),
            Action::AmendInto(commit) => {
                let is_head = self.data.as_ref().and_then(|d| d.status.head.as_deref()) == Some(commit.as_str());
                match crate::repo::rebase::amend_into(&self.dir, &commit, is_head) {
                    Ok(op) => self.run(ctx, op),
                    Err(e) => self.notes.push(Msg { text: e, error: true }),
                }
            }
            Action::Absorb => self.start_absorb(ctx),
            Action::Split(commit) => self.start_split(ctx, commit),
            Action::SplitEnd { abort } => self.end_split(ctx, abort),
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
            Action::Reflog => {
                (self.diff, self.merge, self.file_view, self.rebase) = (None, None, None, None);
                self.reflog = Some(reflog::Reflog::open(ctx, &self.dir));
            }
            Action::RemoteForm(f) => self.remote_form = Some(f),
            Action::Doctor(remote) => self.doctor = Some(doctor::Doctor::open(ctx, &self.dir, &remote)),
            Action::Patch { ids, save } => self.export_patch(ctx, ids, save),
            Action::ApplyPatch => self.apply_patch(ctx),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::testutil::{commit, identity, scratch, settle, sh};
    use std::path::Path;

    fn wait(ctx: &egui::Context, t: &mut RepoTab, done: impl Fn(&RepoTab) -> bool) {
        for _ in 0..1000 {
            t.poll(ctx, &mut vec![]);
            if done(t) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("timed out");
    }

    /// main and feat both change f; merging feat stops on a conflict.
    fn conflicted_merge(dir: &Path) {
        sh(dir, &["init", "-q", "-b", "main"]);
        identity(dir);
        commit(dir, "other", "x\n");
        commit(dir, "f", "base\n");
        sh(dir, &["checkout", "-q", "-b", "feat"]);
        commit(dir, "f", "theirs\n");
        sh(dir, &["checkout", "-q", "main"]);
        commit(dir, "f", "ours\n");
        assert!(git::run(dir, &["merge", "-q", "feat"]).is_err());
    }

    /// A merge resolved back to HEAD leaves a clean status: the WIP row (and its
    /// Continue banner) must stay, and the concluded merge must be undoable.
    #[test]
    fn clean_resolved_merge_keeps_wip_and_continue_undoes() {
        let dir = scratch("clean-merge");
        conflicted_merge(&dir);
        let before = sh(&dir, &["rev-parse", "HEAD"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Use ours", &["checkout", "--ours", "--", "f"]).then(&["add", "--", "f"]).light()));
        settle(&ctx, &mut t);
        let d = t.data.as_ref().unwrap();
        assert!(!d.status.is_dirty() && d.state == git::State::Merging);
        assert!(d.wip && t.sel == Some(Sel::Wip));
        t.apply(&ctx, Action::Git(Op::new("Continue merge", &["-c", "core.editor=true", "merge", "--continue"]).undo(Undo::Keep)));
        settle(&ctx, &mut t);
        assert_eq!(t.data.as_ref().unwrap().state, git::State::Clean);
        assert_eq!(t.undo_label(), Some("Continue merge"));
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stage_all_leaves_conflicts_alone() {
        let dir = scratch("stage-all");
        conflicted_merge(&dir);
        std::fs::write(dir.join("other"), "y\n").unwrap();
        // A glob character in the name; Windows forbids '*' in file names.
        std::fs::write(dir.join(if cfg!(windows) { "new[1]" } else { "new*" }), "n\n").unwrap();
        let s = git::status(&dir).unwrap();
        assert!(op::stage_all_op(&git::Status { unstaged: vec![], ..s.clone() }).is_none());
        let o = op::stage_all_op(&s).unwrap();
        op::exec(&dir, &o).unwrap();
        let s = git::status(&dir).unwrap();
        assert_eq!(s.conflicted.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["f"]);
        assert!(s.unstaged.is_empty());
        let staged: Vec<&str> = s.staged.iter().map(|c| c.path.as_str()).collect();
        assert!(staged.contains(&"other") && staged.contains(&if cfg!(windows) { "new[1]" } else { "new*" }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_reload_keeps_truncated_author_and_refreshes_diff() {
        let dir = scratch("truncated");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        sh(&dir, &["config", "author.email", "me@corp.example"]);
        for i in 0..3 {
            commit(&dir, "f", &format!("a\n{i}\n"));
        }
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 2, View::default());
        settle(&ctx, &mut t);
        assert!(t.data.as_ref().unwrap().truncated);
        std::fs::write(dir.join("f"), "a\nfoo\n").unwrap();
        t.reload(&ctx, false);
        settle(&ctx, &mut t);
        assert!(t.data.as_ref().unwrap().truncated);
        assert_eq!(t.data.as_ref().unwrap().author.1, "me@corp.example");
        let change = t.data.as_ref().unwrap().status.unstaged[0].clone();
        t.apply(&ctx, Action::Diff(DiffSrc::Worktree { change, staged: false }));
        let has = |t: &RepoTab, s: &str| {
            t.diff.as_ref().and_then(|d| d.file.as_ref()).and_then(|f| f.as_ref().ok()).is_some_and(|f| f.hunks.iter().flat_map(|h| &h.lines).any(|l| l.text == s))
        };
        wait(&ctx, &mut t, |t| has(t, "foo"));
        // An edit made outside Glasir (watcher / focus / F5 reload).
        std::fs::write(dir.join("f"), "a\nbar\n").unwrap();
        t.reload(&ctx, false);
        wait(&ctx, &mut t, |t| has(t, "bar"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clicking_the_open_file_again_closes_its_diff() {
        let dir = scratch("toggle-diff");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "a\n");
        std::fs::write(dir.join("f"), "b\n").unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let src = DiffSrc::Worktree { change: t.data.as_ref().unwrap().status.unstaged[0].clone(), staged: false };
        t.apply(&ctx, Action::Diff(src.clone()));
        assert!(t.diff.is_some());
        t.apply(&ctx, Action::Diff(src.clone()));
        assert!(t.diff.is_none());
        t.apply(&ctx, Action::Diff(src));
        assert!(t.diff.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Once a staging op finishes, the diff it patched is already reloading (hunk
    /// buttons hidden): re-applying the old patch could hit an identical block elsewhere.
    #[test]
    fn diff_is_stale_as_soon_as_an_op_finishes() {
        let dir = scratch("stale-diff");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "a\n");
        std::fs::write(dir.join("f"), "b\n").unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let change = t.data.as_ref().unwrap().status.unstaged[0].clone();
        t.apply(&ctx, Action::Diff(DiffSrc::Worktree { change, staged: false }));
        wait(&ctx, &mut t, |t| t.diff.as_ref().is_some_and(|d| !d.loading()));
        t.apply(&ctx, Action::Git(Op::new("Stage", &["add", "f"]).light()));
        wait(&ctx, &mut t, |t| t.running.is_none());
        assert!(t.diff.as_ref().unwrap().loading());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compare_follows_moved_refs() {
        let dir = scratch("compare");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a\n");
        sh(&dir, &["branch", "feat"]);
        commit(&dir, "b", "b\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let files = |t: &RepoTab| t.info.as_ref().and_then(|i| i.as_ref().ok()).map(|i| i.files.len());
        let sel = Sel::Compare { from: "feat".into(), to: "main".into(), three_dot: true };
        t.apply(&ctx, Action::Select(sel.clone()));
        wait(&ctx, &mut t, |t| files(t) == Some(1));
        t.apply(&ctx, Action::Git(Op::new("Commit", &["commit", "-q", "--allow-empty", "-m", "c"]).then(&["branch", "-f", "feat"]).undo(Undo::Soft)));
        wait(&ctx, &mut t, |t| files(t) == Some(0));
        commit(&dir, "c", "c\n");
        t.apply(&ctx, Action::Select(sel));
        wait(&ctx, &mut t, |t| files(t) == Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
