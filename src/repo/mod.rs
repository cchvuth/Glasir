//! Per-repository state (`RepoTab`): background loading, polling and the `apply` dispatch for the `Action`s the UI emits.
//! One concern per submodule (`op`, `undo`, `rebase`, `history`, ...); no drawing lives here.

pub mod absorb;
mod action;
mod ai_job;
pub mod avatars;
pub mod cleanup;
pub mod compose;
mod conflicts;
mod data;
mod diff;
pub mod doctor;
pub mod sshkey;
mod fetch;
pub mod file_edit;
pub mod files;
pub mod filetree;
pub mod history;
pub mod identity;
pub mod image_diff;
pub mod maintenance;
pub mod merge_editor;
pub mod merge_opts;
pub mod message;
mod msgedit;
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
pub mod stack;
pub mod tags;
mod task;
pub mod terminal;
pub mod tools;
#[cfg(test)]
pub(crate) mod testutil;
mod undo;
mod watch;

pub use action::{Action, Choice, DiffSrc, Msg, MultiOp, Prompt, Sel, op, prompt};
pub use data::{RepoData, View};
pub use diff::{DiffState, patch_op};
pub use files::reveal;
pub use op::{Backup, Op, PullMode, Undo, branch_checkout, delete_branches, discard_files_op, discard_op, drop_stashes, fetch_op, apply_stash_op, pop_stash, pull_op, remove_worktree_and_branch, rename_stash, reset_op, sequencer_op, stage_all_op, stage_files_op, stash_branch, stash_paths_op, stash_prompt, unstage_all_op, unstage_files_op, unstage_op};
pub use msgedit::MsgEdit;
pub use search::{MAX_HITS, Search};
pub use task::Task;
pub use undo::Steps;

use crate::ai;
use crate::git::{self, CommitInfo, Res};
use data::load_all;
use op::Done;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use undo::UndoEntry;

/// Shown when a repo uses LFS (or an LFS action is picked) without git-lfs.
pub const LFS_MISSING: &str = "This repository uses Git LFS, but git-lfs is not installed: large files stay as small pointer files. Install it from git-lfs.com (or brew/apt/winget install git-lfs), run git lfs install, then restart Glasir.";

pub const PAGE: usize = 1000;

/// A commit selected within this long of the previous one (key repeat) waits for the selection to rest before its details load.
const PICK_REST: std::time::Duration = std::time::Duration::from_millis(120);

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
    /// A new value whenever `info` is replaced, unique across tabs: keys the cached folder tree of its files.
    pub info_gen: u64,
    info_task: Option<Task<Res<CommitInfo>>>,
    /// The selection moved while `info_task` ran (holding ↓): load the current one when it ends.
    info_next: bool,
    /// When the last commit was selected, and when a deferred lookup for the current one is due:
    /// a held ↓ would otherwise start git for every row it passes.
    last_pick: Option<std::time::Instant>,
    info_due: Option<std::time::Instant>,
    /// Line counts for `info`'s commit (its id), merged in when ready.
    stats_task: Option<Task<(String, git::Stats)>>,
    pub diff: Option<DiffState>,
    /// Whitespace / context options for every diff opened in this tab.
    pub diff_opts: git::DiffOpts,
    running: Option<(String, Task<Done>)>,
    /// External merge / diff tool windows open beside the op slot.
    pub ext_tools: Vec<tools::Open>,
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
    /// Amend's prefill: HEAD's message being fetched, and (box before, box as prefilled, that
    /// HEAD as `rebase::head_state` puts it: the amend refuses once HEAD is elsewhere).
    amend_task: Option<Task<Option<String>>>,
    amend_prev: Option<((String, String), (String, String), String)>,
    pub ai_cfg: ai::Config,
    /// Messages for the toast area, drained by the app every frame.
    pub notes: Vec<Msg>,
    /// A repository the app should open in a new tab.
    pub open_tab: Option<PathBuf>,
    pub switch_to: Option<PathBuf>,
    watcher: Option<(notify::RecommendedWatcher, Receiver<watch::Change>)>,
    watcher_task: Option<Task<Option<(notify::RecommendedWatcher, Receiver<watch::Change>)>>>,
    pub search: Option<Search>,
    /// Finding a jumped-to commit outside the loaded history: (id, its position in the log).
    locate: Option<Task<Option<(String, usize)>>>,
    /// That commit, scrolled to once a reload has brought it in.
    jump_after: Option<String>,
    /// Extra selected commits (⌘/⇧-click), newest first; empty = single selection.
    pub multi: Vec<String>,
    /// Selected files of the WIP lists (⌘/⇧-click) as (staged, path): the target of S / U / Discard.
    pub file_sel: Vec<(bool, String)>,
    pub file_sel_gen: u64,
    /// Collapsed folders of each file list in tree view ("unstaged", "staged", "conflicts", "commit"), kept while the app runs.
    pub tree_closed: HashMap<&'static str, std::collections::HashSet<String>>,
    pub rebase: Option<crate::repo::rebase::Editor>,
    parked_rebase: Option<crate::repo::rebase::Editor>,
    pub merge: Option<merge_editor::Editor>,
    /// The open diff's working-tree file being edited in place (replaces the diff while open).
    pub edit: Option<file_edit::FileEdit>,
    pub file_view: Option<history::FileView>,
    /// Pending drag-and-drop menu: (source branch, target branch, where).
    pub drop_menu: Option<(String, String, egui::Pos2)>,
    pub cleanup: Option<cleanup::Cleanup>,
    /// Settings → protected branches (`release/*`-style patterns), mirrored every frame.
    pub keep_branches: String,
    /// Pending commit drop menu: (title, entries (label, action; `None` = reason it's off), where).
    pub commit_drop: Option<(String, Vec<(String, Option<Action>)>, egui::Pos2)>,
    /// A commit drop (its menu or the drag's rewrite) being worked out off the UI thread.
    pub drop_task: Option<Task<op::Dropped>>,
    /// Predicted conflicts with the target branch: (target, files).
    pub conflicts: Option<(String, Vec<String>)>,
    conflict_task: Option<Task<Option<(String, Vec<String>)>>>,
    /// (HEAD, target) commits `conflicts` was predicted for.
    conflict_key: Option<(String, String)>,
    /// `git verify-tag` running for the tag menu, its result toasted.
    verify_task: Option<Task<Msg>>,
    /// A failed op offering fixes (autostash, diverged pull, rejected push).
    pub choice: Option<Choice>,
    /// Background fetch; Ok(true) when it moved a remote-tracking ref.
    bg_fetch: Option<Task<Res<bool>>>,
    /// The repository's common git dir: one fetch at a time runs per one (`fetch::Fetching`).
    common: PathBuf,
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
    /// The commit message being edited inline in the commit details.
    pub edit_msg: Option<MsgEdit>,
    /// Commit Composer plan; its dialog shows once Claude's commits are in.
    pub compose: Option<compose::Plan>,
    parked_compose: Option<compose::Plan>,
    compose_task: Option<Task<Res<(compose::Plan, String)>>>,
    pub remote_form: Option<remotes::Form>,
    /// The connection doctor window for one remote.
    pub doctor: Option<doctor::Doctor>,
    /// Settings → commit email rules (`host -> @domain` lines), mirrored every frame.
    pub email_rules: String,
    pub avatars: avatars::Avatars,
    pub term: terminal::Terminals,
}

impl RepoTab {
    pub fn new(ctx: &egui::Context, dir: PathBuf, max_commits: usize, view: View) -> Self {
        let name = dir.file_name().map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into_owned());
        let common = git::run(&dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).map_or_else(|_| dir.clone(), |c| PathBuf::from(c.trim()));
        let mut t = RepoTab {
            dir,
            name,
            data: None,
            error: None,
            load: None,
            queued: None,
            sel: None,
            info: None,
            info_gen: 0,
            info_task: None,
            info_next: false,
            last_pick: None,
            info_due: None,
            stats_task: None,
            diff: None,
            diff_opts: Default::default(),
            running: None,
            ext_tools: vec![],
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
            amend_task: None,
            amend_prev: None,
            ai_cfg: ai::Config::default(),
            notes: vec![],
            open_tab: None,
            switch_to: None,
            watcher: None,
            watcher_task: None,
            changed: None,
            search: None,
            locate: None,
            jump_after: None,
            multi: vec![],
            file_sel: vec![],
            file_sel_gen: 0,
            tree_closed: HashMap::new(),
            rebase: None,
            parked_rebase: None,
            merge: None,
            edit: None,
            file_view: None,
            drop_menu: None,
            commit_drop: None,
            drop_task: None,
            cleanup: None,
            conflicts: None,
            conflict_task: None,
            conflict_key: None,
            verify_task: None,
            choice: None,
            bg_fetch: None,
            common,
            last_fetch: f64::NEG_INFINITY,
            fetch_failed: None,
            reflog: None,
            patch_task: None,
            absorb: None,
            absorb_task: None,
            split: None,
            edit_msg: None,
            compose: None,
            parked_compose: None,
            compose_task: None,
            remote_form: None,
            doctor: None,
            email_rules: String::new(),
            keep_branches: String::new(),
            avatars: Default::default(),
            term: Default::default(),
        };
        // Registering watches walks the whole tree (inotify): keep it off the UI thread.
        let (c, d) = (ctx.clone(), t.dir.clone());
        t.watcher_task = Some(Task::spawn(ctx, move || watch::watch(&c, &d)));
        // Backups whose undo step went with an earlier session: kept 7 days, then pruned.
        let d = t.dir.clone();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |n| n.as_secs());
        std::thread::spawn(move || git::prune(&d, now, 7));
        t.reload(ctx, true);
        t
    }

    pub fn busy(&self) -> Option<&str> {
        self.running.as_ref().map(|r| r.0.as_str()).or(self.drop_task.as_ref().map(|_| "Preparing the rewrite…"))
    }

    /// Builds a history rewrite (its git calls read the range and the upstream) off the UI thread;
    /// [`Self::busy`] shows it meanwhile; refused while another is being prepared.
    pub(super) fn prepare(&mut self, ctx: &egui::Context, f: impl FnOnce(&std::path::Path) -> op::Dropped + Send + 'static) {
        if self.drop_task.is_some() {
            return self.notes.push(Msg { text: "Wait for the rewrite being prepared, then try again.".into(), error: true });
        }
        self.parked_rebase = None;
        let dir = self.dir.clone();
        self.drop_task = Some(Task::spawn(ctx, move || f(&dir)));
    }
    pub fn unpark_rebase(&mut self) {
        if let Some(ed) = self.parked_rebase.take() {
            (self.diff, self.merge, self.rebase) = (None, None, Some(ed));
        }
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

    /// A file watcher is running (or starting): outside changes arrive without polling.
    pub fn watching(&self) -> bool {
        self.watcher.is_some() || self.watcher_task.is_some()
    }

    /// `full`: log + refs + status; otherwise status only.
    pub fn reload(&mut self, ctx: &egui::Context, full: bool) {
        // The compared refs (or, for a working-tree compare, the files) may have changed:
        // re-query, keeping the old list until then.
        if let Some(Sel::Compare { from, to, three_dot }) = self.sel.clone()
            && (full || to == git::WORKTREE)
        {
            self.compare_query(ctx, from, to, three_dot);
        }
        if self.load.is_some() {
            self.queued = Some(self.queued.unwrap_or(false) || full);
            return;
        }
        self.changed = self.changed.filter(|&(refs, _)| refs && !full);
        let dir = self.dir.clone();
        let (limit, view) = (self.max_commits, self.view.clone());
        let cached = self.data.as_ref().filter(|d| !full && d.view == view).map(|d| {
            (d.log.clone(), d.refs.clone(), d.stashes.clone(), d.remotes.clone(), d.git_dir.clone(), d.origin_head.clone(), (d.layout.clone(), d.graph.clone(), d.layout_key), (d.worktrees.clone(), d.submodules.clone(), d.lfs.clone(), d.config.clone(), d.author.clone(), d.truncated, d.stack.clone(), d.health.clone(), d.bisect.clone()))
        });
        self.load = Some(Task::spawn(ctx, move || match cached {
            Some((log, refs, st, rm, gd, oh, prev, (wt, sm, lfs, cfg, au, tr, stk, hl, bis))) => {
                let mut d = RepoData::build_reusing(log, refs, st, rm, gd, oh, git::status(&dir)?, view, Some(prev));
                (d.worktrees, d.submodules, d.lfs, d.config, d.author, d.truncated, d.stack, d.health) = (wt, sm, lfs, cfg, au, tr, stk, hl);
                d.count_worktree_changes(&dir);
                d.load_bisect(&dir, bis.as_ref());
                d.load_conflicts(&dir);
                Ok(d)
            }
            None => load_all(&dir, limit, view),
        }));
    }

    pub fn poll(&mut self, ctx: &egui::Context, msgs: &mut Vec<Msg>) {
        self.term.poll(ctx);
        self.poll_amend();
        if let Some(w) = Task::poll(&mut self.watcher_task) {
            if w.is_none() {
                msgs.push(Msg { text: "Live refresh is off for this repo (file watching unavailable); changes show on focus or F5.".into(), error: false });
            }
            self.watcher = w;
        }
        if let Some(c) = &mut self.cleanup {
            c.poll();
        }
        if let Some((_, rx)) = &self.watcher {
            let now = ctx.input(|i| i.time);
            let ours = self.running.is_some() || self.load.is_some();
            for c in rx.try_iter() {
                // An index write during an op or its reload is our own: the op's reload already shows it.
                // A worktree edit may land after that reload's `git status`: kept for after it.
                if ours && c == watch::Change::Index {
                    continue;
                }
                self.changed = Some((self.changed.is_some_and(|c| c.0) || c == watch::Change::Refs, now));
            }
            if let Some((full, at)) = self.changed
                && now - at > 0.4
                && self.load.is_none()
                && self.running.is_none()
            {
                self.reload(ctx, full);
            }
            if let Some((_, at)) = self.changed {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64((0.4 - (now - at)).max(0.05)));
            }
        }
        if let Some(res) = Task::poll(&mut self.load) {
            match res {
                Ok(mut d) => {
                    if let Some(old) = &self.data {
                        stack::keep_stale(old, &mut d);
                    }
                    if self.sel == Some(Sel::Wip) && !d.wip {
                        self.sel = None;
                    }
                    // A popped or dropped stash would otherwise linger as a bogus merge commit.
                    if let (Some(Sel::Commit(id)), Some(old)) = (&self.sel, &self.data)
                        && old.stashes.iter().any(|s| s.id == *id)
                        && !d.stashes.iter().any(|s| s.id == *id)
                    {
                        self.sel = None;
                    }
                    let first = self.data.is_none();
                    if first && d.lfs.used && !d.lfs.installed {
                        msgs.push(Msg { text: LFS_MISSING.into(), error: true });
                    }
                    let head = d.status.head.clone();
                    // Bisect checked out the next candidate: select and centre it.
                    let head_moved = self.data.as_ref().is_some_and(|o| o.status.head != head);
                    let bisect_moved = d.state == git::State::Bisecting && head_moved;
                    // Resolved or aborted elsewhere: the editor's markers are stale.
                    if self.merge.as_ref().is_some_and(|m| !d.status.conflicted.iter().any(|c| c.path == m.path)) {
                        self.merge = None;
                    }
                    // Dropping a big log takes tens of ms: off the UI thread.
                    if let Some(old) = self.data.replace(d) {
                        std::thread::spawn(move || drop(old));
                    }
                    if let Some(fv) = &mut self.file_view {
                        fv.refresh(ctx, head_moved);
                    }
                    if first {
                        self.prefill_template();
                    }
                    if let Some(id) = &self.jump_after
                        && let Some(&row) = self.shown().and_then(|d| d.row_of.get(id))
                    {
                        if self.sel.as_ref() == Some(&Sel::Commit(id.clone())) {
                            self.scroll_to = Some(row);
                        }
                        self.jump_after = None;
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
        if let Some(Some((id, pos))) = Task::poll(&mut self.locate) {
            self.max_commits = self.max_commits.max((pos / PAGE + 1) * PAGE);
            self.jump_after = Some(id);
            self.reload(ctx, true);
        }
        match Task::poll(&mut self.drop_task) {
            Some(op::Dropped::Menu(title, items, at)) => self.commit_drop = Some((title, items, at)),
            Some(op::Dropped::Run(o)) => self.run(ctx, o),
            Some(op::Dropped::Note(e)) => self.notes.push(Msg { text: e, error: true }),
            Some(op::Dropped::Editor(ed)) => (self.diff, self.merge, self.rebase) = (None, None, Some(*ed)),
            Some(op::Dropped::Split(o, sp)) => self.begin_split(ctx, o, *sp),
            None => {}
        }
        if let Some(c) = Task::poll(&mut self.conflict_task) {
            self.conflicts = c;
        }
        msgs.extend(Task::poll(&mut self.verify_task));
        if self.info_due.is_some_and(|t| t <= std::time::Instant::now()) && self.info_task.is_none() {
            self.info_due = None;
            if let Some(Sel::Commit(id)) = self.sel.clone() {
                self.load_info(ctx, id);
            }
        }
        if let Some(r) = Task::poll(&mut self.info_task) {
            if let (true, Some(Sel::Commit(id))) = (std::mem::take(&mut self.info_next), self.sel.clone()) {
                let fast = self.picked_lately();
                self.load_info_soon(ctx, id, fast); // the selection moved on: this result is stale
            } else {
                if let Ok(i) = &r {
                    let (dir, id, base) = (self.dir.clone(), i.id.clone(), i.parents.first().cloned());
                    self.stats_task = Some(Task::spawn(ctx, move || {
                        let s = git::numstat(&dir, base.as_deref(), &id).unwrap_or_default();
                        (id, s)
                    }));
                }
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                self.info = Some(r);
                self.info_gen = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        if let Some((id, s)) = Task::poll(&mut self.stats_task)
            && let Some(Ok(i)) = &mut self.info
            && i.id == id
        {
            i.stats = s;
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
        self.poll_tools(ctx, msgs);
        self.sync_msg_edit(ctx);
    }

    pub fn select(&mut self, ctx: &egui::Context, sel: Sel) {
        self.multi.clear();
        // A repeated Compare re-queries: its ref names may point elsewhere now.
        if self.sel.as_ref() != Some(&sel) || matches!(sel, Sel::Compare { .. }) {
            match sel.clone() {
                Sel::Commit(id) => {
                    self.info = None;
                    // One lookup at a time, and none while keys repeat (see `load_info_soon`).
                    if self.info_task.is_some() {
                        self.info_next = true;
                    } else {
                        let fast = self.picked_lately();
                        self.load_info_soon(ctx, id, fast);
                    }
                    self.last_pick = Some(std::time::Instant::now());
                }
                Sel::Compare { from, to, three_dot } => {
                    self.info = None;
                    self.diff = None;
                    self.compare_query(ctx, from, to, three_dot);
                }
                Sel::Wip => self.info_due = None,
            }
        }
        self.sel = Some(sel);
    }

    /// Loads `id`'s details now, or (`fast`: the selection moved again just before, key repeat)
    /// once it has rested for `PICK_REST`: only the row the arrow stops on gets a lookup.
    fn load_info_soon(&mut self, ctx: &egui::Context, id: String, fast: bool) {
        if fast {
            self.info_due = Some(std::time::Instant::now() + PICK_REST);
            ctx.request_repaint_after(PICK_REST);
        } else {
            self.info_due = None;
            self.load_info(ctx, id);
        }
    }

    fn picked_lately(&self) -> bool {
        self.last_pick.is_some_and(|t| t.elapsed() < PICK_REST)
    }

    fn load_info(&mut self, ctx: &egui::Context, id: String) {
        let dir = self.dir.clone();
        let stash = self.data.as_ref().is_some_and(|d| d.stashes.iter().any(|s| s.id == id));
        self.info_task = Some(Task::spawn(ctx, move || if stash { git::stash_info(&dir, &id) } else { git::commit_info(&dir, &id) }));
    }

    /// The history the graph shows: the search results while "Filter graph to results" is on.
    pub fn shown(&self) -> Option<&RepoData> {
        self.search.as_ref().and_then(|s| s.filtered.as_deref()).or(self.data.as_ref())
    }

    /// Finds how deep in the (date-ordered) log `id` is, to load enough pages to show it.
    fn locate(&mut self, ctx: &egui::Context, id: String) {
        let refs = self.data.iter().flat_map(|d| d.refs.iter().map(|r| r.full.as_str()));
        let (dir, (revs, shown)) = (self.dir.clone(), self.view.revs(refs));
        self.locate = Some(Task::spawn(ctx, move || {
            let mut args = vec!["log", "--date-order", "-z", "--format=%H"];
            args.extend(revs.iter().map(String::as_str));
            args.push("--");
            let (mut n, mut found) = (0, false);
            let _ = git::stream(&dir, &args, &shown, |h| {
                found = h == id;
                n += !found as usize;
                !found
            });
            found.then_some((id, n))
        }));
    }

    fn compare_query(&mut self, ctx: &egui::Context, from: String, to: String, three_dot: bool) {
        let (dir, label) = (self.dir.clone(), if to == git::WORKTREE { format!("Working tree compared with {}", git::short_ref(&from)) } else { format!("{} compared with {}", git::short_ref(&to), git::short_ref(&from)) });
        self.info_next = false;
        self.info_due = None;
        self.info_task = Some(Task::spawn(ctx, move || git::compare_info(&dir, &from, &to, three_dot, &label)));
    }

    pub fn unsaved(&self) -> Option<String> {
        if let Some(e) = self.edit.as_ref().filter(|e| e.dirty()) {
            return Some(format!("Save or discard your edits to {}", e.path));
        }
        if let Some(m) = self.merge.as_ref().filter(|m| m.edited()) {
            return Some(format!("Save or discard your resolution of {}", m.path));
        }
        if let Some(r) = self.rebase.as_ref().or(self.parked_rebase.as_ref()).filter(|r| r.edited()) {
            return Some(format!("Start or cancel your {} plan", if r.cherry { "cherry-pick" } else { "rebase" }));
        }
        if self.compose.as_ref().is_some_and(|p| !p.commits.is_empty()) {
            return Some("Create or cancel the Commit Composer's commits".into());
        }
        self.split.as_ref().filter(|sp| !sp.replaying).map(|sp| format!("Continue or abort the split of {}", &sp.id[..7.min(sp.id.len())]))
    }

    pub fn apply(&mut self, ctx: &egui::Context, a: Action) {
        let merge = self.merge.as_ref().filter(|m| m.edited());
        let replaces = match &a {
            Action::MergeEditor(p) if merge.is_some_and(|m| m.path == *p) => return,
            Action::Diff(_) | Action::FileView(..) => merge.is_some(),
            Action::MergeEditor(_) | Action::Reflog | Action::Rebase { .. } | Action::MultiOp(MultiOp::CherryPickEdit, _) => merge.is_some() || self.rebase.as_ref().is_some_and(|r| r.edited()),
            _ => false,
        };
        if replaces && let Some(w) = self.unsaved() {
            return self.notes.push(Msg { text: format!("{w} first."), error: true });
        }
        // Anything that replaces the center pane ends the file editor; unsaved edits ask first.
        if let Some(ed) = &mut self.edit
            && matches!(a, Action::Select(_) | Action::Diff(_) | Action::CloseDiff | Action::FileView(..) | Action::MergeEditor(_) | Action::Rebase { .. } | Action::Reflog | Action::Jump(_))
        {
            if ed.dirty() {
                ed.ask = Some(Some(a));
                return;
            }
            self.edit = None;
        }
        match a {
            Action::EditFile(path) => match file_edit::FileEdit::load(&self.dir, &path) {
                Ok(ed) => (self.edit, self.file_view) = (Some(ed), None),
                Err(e) => self.notes.push(Msg { text: e, error: true }),
            },
            Action::EditSave => {
                if self.busy().is_some() {
                    if let Some(ed) = &mut self.edit {
                        ed.then = None; // the save won't run: don't leave the navigation armed
                    }
                    return self.notes.push(Msg { text: "Wait for the running operation to finish, then save.".into(), error: true });
                }
                if let Some(ed) = &mut self.edit
                    && let Some(op) = ed.save_op(&self.dir)
                {
                    ed.saving = true;
                    self.run(ctx, op);
                } else if let Some(ed) = &mut self.edit {
                    ed.then = None; // disk conflict: the banner shows, nothing saves
                }
            }
            Action::EditClose => {
                if let Some(next) = self.edit.take().and_then(|e| e.ask.flatten()) {
                    self.apply(ctx, next);
                }
            }
            Action::Select(s) => self.select(ctx, s),
            // Clicking the file that is already open closes it again.
            Action::Diff(src) if self.merge.is_none() && self.diff.as_ref().is_some_and(|d| d.src == src) => self.diff = None,
            Action::Diff(src) => {
                // "Load anyway" is for that one file.
                self.diff_opts.all = false;
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
                let hosts: Vec<String> = self.data.iter().flat_map(|d| d.remotes.iter()).filter_map(|r| git::host(&r.1)).collect();
                if git::is_web_link(&u, &hosts) {
                    let _ = open::that_detached(u);
                } else {
                    self.notes.push(Msg { text: format!("Not opened: {u} is not an http(s) page on a remote's host."), error: true });
                }
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
            Action::ToggleTerminal => self.term.toggle(ctx, &self.dir),
            Action::Custom(c, v) => {
                match tools::op(&c, &self.dir, &v) {
                    Ok(op) => self.run(ctx, op),
                    Err(e) => self.notes.push(Msg { text: e, error: true }),
                }
            }
            Action::Jump(id) => {
                self.scroll_to = self.shown().and_then(|d| d.row_of.get(&id).copied());
                if self.scroll_to.is_none() && self.shown().is_some_and(|d| d.truncated) {
                    self.locate(ctx, id.clone());
                }
                self.select(ctx, Sel::Commit(id));
            }
            Action::Ai(p) => self.start_ai(ctx, p),
            Action::AiCancel => self.cancel_ai(),
            Action::Error(e) => self.notes.push(Msg { text: e, error: true }),
            Action::Cleanup => self.open_cleanup(ctx),
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
            Action::Switch(p) => self.switch_to = Some(self.dir.join(p)),
            Action::AmendInto(commit) => {
                let is_head = self.data.as_ref().and_then(|d| d.status.head.as_deref()) == Some(commit.as_str());
                self.prepare(ctx, move |dir| op::Dropped::from(crate::repo::rebase::amend_into(dir, &commit, is_head)));
            }
            Action::Absorb => self.start_absorb(ctx),
            Action::AmendToggled(true) => self.fill_amend(ctx),
            Action::AmendToggled(false) => self.unfill_amend(),
            Action::Split(commit) => self.start_split(ctx, commit, false),
            Action::Recompose(r) => {
                if self.ai_free() {
                    self.start_compose(ctx, Some(r));
                }
            }
            Action::CommitPlan => {
                let (Some(p), Some(git_dir)) = (self.compose.clone(), self.data.as_ref().map(|d| d.git_dir.clone())) else { return };
                if self.drop_task.is_none() {
                    self.parked_compose = self.compose.take();
                }
                self.prepare(ctx, move |dir| op::Dropped::from(p.op(dir, &git_dir)));
            }
            Action::SplitEnd { abort } => self.end_split(ctx, abort),
            Action::Reword { commit, message } => {
                let Some(d) = self.data.as_ref() else { return };
                let git_dir = d.git_dir.clone();
                let at = d.reword_path(&commit).ok();
                if let (Some(e), Some(at)) = (self.edit_msg.as_mut().filter(|e| e.id == commit), &at) {
                    e.at = at.clone();
                }
                let branch = at.and_then(|at| at.0);
                let is_head = branch.is_none() && d.status.head.as_deref() == Some(commit.as_str());
                self.prepare(ctx, move |dir| match crate::repo::rebase::reword(dir, &git_dir, &commit, is_head, branch.as_deref(), message) {
                    Ok(op) => op::Dropped::Run(Op { reword: Some(commit), ..op }),
                    Err(e) => op::Dropped::Note(e),
                });
            }
            Action::Rewrite { commit, act, message } => {
                let Some(git_dir) = self.data.as_ref().map(|d| d.git_dir.clone()) else { return };
                self.prepare(ctx, move |dir| op::Dropped::from(crate::repo::rebase::quick(dir, &git_dir, &commit, act, message)));
            }
            Action::DropBranch { src, dst } => {
                let at = ctx.input(|i| i.pointer.interact_pos()).unwrap_or_default();
                self.drop_menu = Some((src, dst, at));
            }
            Action::DropCommit { src, dst, branch, squash } => self.drop_commit(ctx, src, dst, branch, squash),
            Action::DragCommit { src, dst, act } => {
                let Some(git_dir) = self.data.as_ref().map(|d| d.git_dir.clone()) else { return };
                self.prepare(ctx, move |dir| op::Dropped::from(crate::repo::rebase::drag(dir, &git_dir, &src, &dst, act)));
            }
            Action::FileView(path, mode) => {
                (self.diff, self.merge) = (None, None);
                match &mut self.file_view {
                    Some(fv) => fv.go(ctx, path, mode),
                    None => self.file_view = Some(history::FileView::open(ctx, &self.dir, &path, mode)),
                }
            }
            Action::Rebase { base, label } => {
                self.prepare(ctx, move |dir| match crate::repo::rebase::Editor::load(dir, &base, &label) {
                    Ok(ed) => op::Dropped::Editor(Box::new(ed)),
                    Err(e) => op::Dropped::Note(e),
                });
            }
            Action::RunRebase { op, onto, branch, first } => {
                let ed = self.rebase.take_if(|_| self.drop_task.is_none());
                self.prepare(ctx, move |dir| op::Dropped::Run(Op { parked: true, ..crate::repo::rebase::confirm_rebase(dir, &onto, branch.as_deref(), first.as_deref(), op) }));
                self.parked_rebase = ed.or(self.parked_rebase.take());
            }
            Action::Reflog => {
                (self.diff, self.merge, self.file_view, self.rebase) = (None, None, None, None);
                self.reflog = Some(reflog::Reflog::open(ctx, &self.dir));
            }
            Action::RemoteForm(f) => self.remote_form = Some(f),
            Action::Doctor(remote) => self.doctor = Some(doctor::Doctor::open(ctx, &self.dir, &remote)),
            Action::SshKey(key) => {
                let local = git::run(&self.dir, &["config", "--local", "--get", "core.sshCommand"]).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
                let custom = local.as_deref().filter(|c| matches!(sshkey::parse(Some(c)), sshkey::Active::Custom(_)));
                if key.is_some() || local.is_some() {
                    self.run(ctx, sshkey::set_op(key.as_deref(), local.as_deref(), custom));
                }
            }
            Action::SshTest => {
                let url = self.doctor.as_ref().and_then(|d| self.data.as_ref()?.remotes.iter().find(|r| r.0 == d.remote)).map(|r| r.1.clone());
                let key = match sshkey::parse(self.data.as_ref().and_then(|d| d.cfg("core.sshcommand"))) {
                    sshkey::Active::Ours(k) => Some(k),
                    _ => None,
                };
                if let (Some(d), Some(url)) = (&mut self.doctor, url) {
                    d.test(ctx, key, url);
                }
            }
            Action::Patch { ids, save } => self.export_patch(ctx, ids, save),
            Action::ApplyPatch => self.apply_patch(ctx),
            Action::Undo => self.undo_redo(ctx, true, false),
            Action::Redo => self.undo_redo(ctx, false, false),
            Action::Reload => self.reload(ctx, true),
            Action::LoadMore => {
                self.max_commits += crate::repo::PAGE;
                self.reload(ctx, true);
            }
            Action::CloseTool(path) => self.close_tool(&path),
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

    /// Clicking another file, its own row, File history, the reflog or a new rebase keeps an
    /// edited resolution or rebase plan (with a note); an untouched one is replaced as before.
    #[test]
    fn navigation_keeps_an_edited_resolution_or_plan() {
        let dir = scratch("nav-keeps-edits");
        conflicted_merge(&dir);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let mut m = merge_editor::Editor::open(&ctx, &dir, "f");
        crate::repo::testutil::wait_for("the merge editor", || {
            if let Some(d) = Task::poll(&mut m.task) {
                (m.output, m.data) = (d.text.clone(), Some(d));
            }
            m.data.is_some()
        });
        m.output.push('x');
        t.merge = Some(m);
        let change = git::Change { path: "other".into(), old_path: None, code: 'M' };
        let interactive = || Action::Rebase { base: "HEAD~1".into(), label: "x".into() };
        for a in [Action::Diff(DiffSrc::Worktree { change: change.clone(), staged: false }), Action::FileView("f".into(), history::Mode::History), Action::Reflog, Action::MergeEditor("other".into()), interactive()] {
            t.apply(&ctx, a);
            assert!(t.merge.as_ref().is_some_and(|m| m.output.ends_with('x')) && t.drop_task.is_none() && t.reflog.is_none() && t.file_view.is_none());
            assert!(t.notes.pop().is_some_and(|n| n.text.contains("resolution of f")));
        }
        t.apply(&ctx, Action::MergeEditor("f".into()));
        assert!(t.merge.as_ref().is_some_and(|m| m.output.ends_with('x')) && t.notes.is_empty(), "its own row: nothing happens");
        t.merge = None;
        let mut ed = rebase::Editor::load(&dir, "HEAD~1", "x").unwrap();
        ed.items[0].act = rebase::Act::Drop;
        t.rebase = Some(ed);
        for a in [Action::Reflog, Action::MergeEditor("f".into()), interactive()] {
            t.apply(&ctx, a);
            assert!(t.rebase.is_some() && t.merge.is_none() && t.drop_task.is_none() && t.reflog.is_none());
            assert!(t.notes.pop().is_some_and(|n| n.text.contains("rebase plan")));
        }
        t.rebase.as_mut().unwrap().items[0].act = rebase::Act::Pick;
        t.apply(&ctx, Action::MergeEditor("f".into()));
        assert!(t.rebase.is_none() && t.merge.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rebase plan parked behind its pushed-commits confirm and a split in progress keep the tab open.
    #[test]
    fn a_parked_plan_or_a_split_is_unsaved_work() {
        let dir = scratch("unsaved-parked");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for f in ["a", "b", "c"] {
            commit(&dir, f, f);
        }
        sh(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let mut ed = rebase::Editor::load(&dir, "HEAD~2", "a").unwrap();
        ed.items[0].act = rebase::Act::Drop;
        let start = ed.start(&t.data.as_ref().unwrap().git_dir).unwrap();
        t.rebase = Some(ed);
        t.apply(&ctx, start);
        settle(&ctx, &mut t);
        assert!(t.confirm.is_some() && t.rebase.is_none());
        assert!(t.unsaved().is_some_and(|w| w.contains("rebase plan")));
        t.confirm = None;
        t.parked_rebase = None;
        sh(&dir, &["update-ref", "-d", "refs/remotes/origin/main"]);
        t.apply(&ctx, Action::Split(sh(&dir, &["rev-parse", "HEAD"]).trim().into()));
        settle(&ctx, &mut t);
        assert!(t.split.is_some());
        assert!(t.unsaved().is_some_and(|w| w.contains("split")));
        t.split.as_mut().unwrap().replaying = true;
        assert_eq!(t.unsaved(), None, "a replaying split ends with its rebase");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A second rewrite asked for while one is being prepared is refused; the first and the plan stay.
    #[test]
    fn a_pending_prepare_is_not_replaced() {
        let dir = scratch("prepare-twice");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a");
        commit(&dir, "b", "b");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let (tx, rx) = std::sync::mpsc::channel();
        t.drop_task = Some(Task(rx));
        let mut ed = rebase::Editor::load(&dir, "HEAD~1", "a").unwrap();
        ed.items[0].act = rebase::Act::Reword;
        let start = ed.start(&t.data.as_ref().unwrap().git_dir).unwrap();
        t.rebase = Some(ed);
        t.apply(&ctx, start);
        assert!(t.rebase.is_some() && t.parked_rebase.is_none(), "the plan stays open");
        assert!(t.notes.pop().is_some_and(|n| n.text.contains("being prepared")));
        tx.send(op::Dropped::Note("first".into())).unwrap();
        t.poll(&ctx, &mut vec![]);
        assert_eq!(t.notes.pop().map(|n| n.text).as_deref(), Some("first"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Edit from a diff beside a File history shows the editor.
    #[test]
    fn editing_from_a_history_diff_closes_the_history() {
        let dir = scratch("edit-from-history");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "a\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::FileView("f".into(), history::Mode::History));
        t.apply(&ctx, Action::EditFile("f".into()));
        assert!(t.edit.is_some() && t.file_view.is_none());
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
    fn exactly_a_page_is_not_truncated_and_status_reload_keeps_the_layout() {
        let dir = scratch("exact_page");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for i in 0..3 {
            commit(&dir, "f", &format!("a\n{i}\n"));
        }
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 3, View::default());
        settle(&ctx, &mut t);
        let d = t.data.as_ref().unwrap();
        assert!((d.rows(), d.truncated) == (3, false));
        let g = d.graph.clone();
        t.reload(&ctx, false);
        settle(&ctx, &mut t);
        assert!(std::sync::Arc::ptr_eq(&g, &t.data.as_ref().unwrap().graph), "same HEAD and WIP: no new layout");
        std::fs::write(dir.join("f"), "dirty\n").unwrap();
        t.reload(&ctx, false);
        settle(&ctx, &mut t);
        assert!(!std::sync::Arc::ptr_eq(&g, &t.data.as_ref().unwrap().graph), "the WIP row appeared");
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

    /// A save landing while a reload runs (maybe after its `git status`) gets a reload of its own.
    #[test]
    fn worktree_events_during_a_reload_are_kept() {
        let dir = scratch("watch-during-load");
        sh(&dir, &["init", "-q", "-b", "main"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let (tx, rx) = std::sync::mpsc::channel();
        t.watcher = Some((notify::recommended_watcher(|_| {}).unwrap(), rx));
        t.watcher_task = None;
        t.reload(&ctx, false);
        tx.send(watch::Change::Index).unwrap();
        t.poll(&ctx, &mut vec![]);
        assert!(t.changed.is_none(), "our own index write");
        settle(&ctx, &mut t);
        t.reload(&ctx, false);
        tx.send(watch::Change::Tree).unwrap();
        t.poll(&ctx, &mut vec![]);
        assert!(t.changed.is_some_and(|c| !c.0), "an outside save was dropped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_watcher_event_reloads_while_the_app_is_idle() {
        let dir = scratch("watch-idle");
        sh(&dir, &["init", "-q", "-b", "main"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let (tx, rx) = std::sync::mpsc::channel();
        t.watcher = Some((notify::recommended_watcher(|_| {}).unwrap(), rx));
        t.watcher_task = None;
        tx.send(watch::Change::Tree).unwrap();
        let mut time = 100.0;
        for _ in 0..20 {
            let mut out = ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |ui| t.poll(ui.ctx(), &mut vec![]));
            out.textures_delta.clear();
            let delay = out.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            if t.changed.is_none() || delay == std::time::Duration::MAX {
                break;
            }
            time += delay.as_secs_f64();
        }
        assert!(t.changed.is_none(), "the debounced reload waits for input");
        settle(&ctx, &mut t);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reload_covers_the_watcher_events_before_it() {
        let dir = scratch("watch-covered");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let (tx, rx) = std::sync::mpsc::channel();
        t.watcher = Some((notify::recommended_watcher(|_| {}).unwrap(), rx));
        t.watcher_task = None;
        let event = |t: &mut RepoTab, c| {
            tx.send(c).unwrap();
            t.poll(&ctx, &mut vec![]);
        };
        event(&mut t, watch::Change::Refs);
        event(&mut t, watch::Change::Tree);
        t.reload(&ctx, false);
        assert!(t.changed.is_some_and(|c| c.0), "a status reload does not cover moved refs");
        settle(&ctx, &mut t);
        t.reload(&ctx, true);
        assert!(t.changed.is_none(), "a full reload covers every event before it");
        event(&mut t, watch::Change::Refs);
        assert!(t.changed.is_some_and(|c| c.0), "a ref move during the reload gets its own");
        settle(&ctx, &mut t);
        t.changed = Some((false, 0.0));
        t.reload(&ctx, false);
        assert!(t.changed.is_none(), "a status reload covers file events before it");
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(Op::new("Commit", &["commit", "-q", "--allow-empty", "-m", "c"]).undo(Undo::Soft)));
        event(&mut t, watch::Change::Refs);
        settle(&ctx, &mut t);
        assert!(t.changed.is_none(), "an op's reload covers the ref writes seen while it ran");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_reload_keeps_health_and_recounts_changes() {
        let dir = scratch("status-health");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "a\n");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let health = t.data.as_ref().unwrap().health.clone();
        assert!(health.loose > 0);
        assert_eq!(t.data.as_ref().unwrap().worktrees[0].changes, 0);
        std::fs::write(dir.join("f"), "dirty\n").unwrap();
        std::fs::write(dir.join("g"), "new\n").unwrap();
        t.reload(&ctx, false);
        settle(&ctx, &mut t);
        let d = t.data.as_ref().unwrap();
        assert_eq!(d.health, health);
        assert_eq!(d.worktrees[0].changes, 2);
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

    /// A search hit (or any jump) beyond the loaded pages loads enough history to show it.
    #[test]
    fn jump_loads_history_up_to_the_commit() {
        let dir = scratch("jump-load");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for i in 0..5 {
            commit(&dir, "f", &format!("{i}\n"));
        }
        let first = sh(&dir, &["rev-list", "--max-parents=0", "HEAD"]).trim().to_owned();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 2, View::default());
        settle(&ctx, &mut t);
        assert!(!t.data.as_ref().unwrap().row_of.contains_key(&first));
        t.apply(&ctx, Action::Jump(first.clone()));
        wait(&ctx, &mut t, |t| t.scroll_to.is_some());
        let d = t.data.as_ref().unwrap();
        assert_eq!(t.scroll_to, d.row_of.get(&first).copied());
        assert_eq!(t.sel, Some(Sel::Commit(first)));
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

    /// Holding ↓ passes many commits: only the one the selection rests on gets its details loaded.
    #[test]
    fn key_repeat_selection_loads_only_the_last_commit() {
        let dir = scratch("keynav");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for i in 0..12 {
            commit(&dir, "f", &format!("v{i}\n"));
        }
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let ids: Vec<String> = t.data.as_ref().unwrap().log.iter().map(|c| c.id.clone()).collect();
        let shows = || git::LOG.lock().unwrap().iter().filter(|l| l.cwd == dir && l.args.iter().any(|a| a == "show")).count();
        let before = shows();
        for id in &ids[1..] {
            t.select(&ctx, Sel::Commit(id.clone()));
            t.poll(&ctx, &mut vec![]);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let last = ids[ids.len() - 1].clone();
        wait(&ctx, &mut t, |t| t.info.as_ref().is_some_and(|i| i.as_ref().is_ok_and(|i| i.id == last)));
        assert!(shows() - before <= 3, "{} lookups for {} rows", shows() - before, ids.len() - 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
