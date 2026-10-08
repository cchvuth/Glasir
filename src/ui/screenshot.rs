//! Dev/CI harness: `GLASIR_SCREENSHOT=out.bmp` saves a screenshot once the repo
//! is loaded, then exits. `GLASIR_SCREENSHOT_VIEW` opens a view first (settings,
//! a flavour, dialogs, diffs, search...); extend the switch when a new view needs
//! visual verification. `GLASIR_KEYS=pc` shows the Linux / Windows shortcut labels on macOS
//! ("Ctrl+Shift+Z" for "⇧⌘Z", see `format::keys`).

use super::app::App;
use crate::git;
use crate::repo::history::Mode;
use crate::repo::{Action, Op, PullMode, Search, Undo, pull_op};
use crate::theme;
use egui::Id;

/// The `rclick:` view's pointer events, one per frame: move there, press, release.
pub(super) fn shot_input(ctx: &egui::Context, raw: &mut egui::RawInput) {
    let wid = Id::new("shot_wheel");
    if let Some((pos, delta, step)) = ctx.data(|m| m.get_temp::<(egui::Pos2, egui::Vec2, u8)>(wid)) {
        match step {
            0 => raw.events.push(egui::Event::PointerMoved(pos)),
            1 => raw.events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta, phase: egui::TouchPhase::Move, modifiers: Default::default() }),
            _ => ctx.data_mut(|m| m.remove::<(egui::Pos2, egui::Vec2, u8)>(wid)),
        }
        if step < 2 {
            ctx.data_mut(|m| m.insert_temp(wid, (pos, delta, step + 1)));
        }
    }
    let id = Id::new("shot_rclick");
    let Some((pos, step)) = ctx.data(|m| m.get_temp::<(egui::Pos2, u8)>(id)) else { return };
    raw.events.push(match step {
        0 => egui::Event::PointerMoved(pos),
        1 | 2 => egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: step == 1, modifiers: Default::default() },
        _ => return ctx.data_mut(|m| m.remove::<(egui::Pos2, u8)>(id)),
    });
    ctx.data_mut(|m| m.insert_temp(id, (pos, step + 1)));
}

impl App {
    pub(super) fn screenshot(&mut self, ctx: &egui::Context) {
        let ready = self.tabs.get(self.active).is_none_or(|t| t.data.is_some() && !t.loading());
        let Some((_, frames)) = &mut self.shot else { return };
        ctx.request_repaint();
        if let (Some(b), Some(t)) = (&mut self.bench, self.tabs.get_mut(self.active))
            && let Some(rows) = t.data.as_ref().map(|d| d.rows()).filter(|&n| n > 0)
        {
            *b = (*b + 37) % rows;
            t.scroll_to = Some(*b);
        }
        if ready {
            *frames += 1;
        }
        // Only once, even if a view makes the tab reload (not ready) on the next frame.
        if ready && *frames == 2 {
            // Several views at once, comma-separated: `flavor:daylight,shortcuts`.
            let views = std::env::var("GLASIR_SCREENSHOT_VIEW").unwrap_or_default();
            for view in views.split(',') {
                match view {
                    "settings" => self.open_settings(),
                    "quit" => self.quit_ask = Some(2),
                    "settings:openai" => {
                        self.s.ai.provider = crate::ai::Provider::OpenAi;
                        ctx.set_zoom_factor(0.6); // so the AI section at the bottom fits
                        self.open_settings();
                    }
                    "top" => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            t.scroll_to = Some(0);
                        }
                    }
                    "bottom" => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            t.scroll_to = t.data.as_ref().map(|d| d.rows().saturating_sub(1));
                        }
                    }
                    q if q.starts_with("flavor:") => {
                        let f = [theme::Flavor::DARK, theme::Flavor::LIGHT].concat().into_iter().find(|f| f.name().eq_ignore_ascii_case(&q[7..]));
                        if let Some(f) = f {
                            theme::apply(ctx, f);
                        }
                    }
                    "log" => self.show_log = true,
                    // Settings → Built-in terminal off while shells still run.
                    "terminal-off" => {
                        std::thread::sleep(std::time::Duration::from_secs(2)); // let a typed `terminal:<command>` start, or its shell ends as idle
                        self.s.terminal = false;
                    }
                    // `terminal`, or `terminal:<command>` typed into its shell.
                    q if q.starts_with("terminal") => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            t.apply(&ctx.clone(), Action::ToggleTerminal);
                            if let (Some(cmd), Some(s)) = (q.strip_prefix("terminal:"), t.term.tabs.first()) {
                                s.write(format!("{cmd}\r").into_bytes());
                            }
                        }
                    }
                    // `terms`: five shells with long titles (as oh-my-zsh sets them) and a long line.
                    "terms" => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            t.apply(&ctx.clone(), Action::ToggleTerminal);
                            for _ in 0..4 {
                                t.term.add(ctx, &t.dir.clone());
                            }
                            for s in &t.term.tabs {
                                s.write(&b"printf '\\033]0;%s\\007' \"ann@build-host: ~/src/github.com/example/very-long-repository-name/subdir\"; seq -s . 1 120\r"[..]);
                            }
                        }
                    }
                    // Network dialogs and badges: `checkout:<branch>`, `pull-ff`, `push-rejected`, `force-push`, `fetch-failed`.
                    q if q.starts_with("checkout:") => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            let b = &q["checkout:".len()..];
                            t.apply(&ctx.clone(), crate::repo::Action::Git(Op::new(format!("Checkout {b}"), &["checkout", b, "--"]).undo(Undo::Keep)));
                        }
                    }
                    "pull-ff" => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            t.apply(&ctx.clone(), crate::repo::Action::Git(pull_op(PullMode::FfOnly)));
                        }
                    }
                    "push" => {
                        if let Some(t) = self.tabs.get_mut(self.active)
                            && let Some(o) = t.push_op(false)
                        {
                            t.apply(&ctx.clone(), crate::repo::Action::Git(o));
                        }
                    }
                    "force-push" => {
                        if let Some(t) = self.tabs.get_mut(self.active)
                            && let Some(o) = t.push_op(true)
                        {
                            t.apply(&ctx.clone(), crate::repo::Action::Git(o));
                        }
                    }
                    "fetch-failed" => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            t.fetch_failed = Some("git@github.example.com: Permission denied (publickey).\nfatal: Could not read from remote repository.".into());
                        }
                    }
                    // Run an op, e.g. `git:fetch --all` (to see the running/cancel state).
                    q if q.starts_with("git:") => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            let args: Vec<&str> = q[4..].split(' ').collect();
                            t.apply(&ctx.clone(), crate::repo::Action::Git(Op::new(args[0], &args)));
                        }
                    }
                    // `mergetool:<path>`: the external merge tool open (listed in the conflict banner).
                    q if q.starts_with("mergetool:") => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            t.apply(&ctx.clone(), Action::Git(crate::repo::tools::mergetool_op(&q[10..])));
                        }
                    }
                    q if q.starts_with("multi:") => {
                        if let Some(t) = self.tabs.get_mut(self.active)
                            && let Some(d) = &t.data
                        {
                            let n: usize = q[6..].parse().unwrap_or(2);
                            let ids: Vec<String> = (0..d.rows()).filter_map(|r| d.commit(r)).take(n).map(|c| c.id.clone()).collect();
                            t.apply(&ctx.clone(), crate::repo::Action::Multi(ids));
                        }
                    }
                    // `diff:<path>` (unstaged or untracked), `diff-staged:<path>`, `diff-ws:<path>` with whitespace ignored and whole-file context, `diff-all:<path>` a large diff loaded anyway.
                    q if q.starts_with("diff:") || q.starts_with("diff-ws:") || q.starts_with("diff-all:") || q.starts_with("diff-staged:") => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            let (mode, path) = q.split_once(':').unwrap();
                            if mode == "diff-ws" {
                                t.diff_opts = git::DiffOpts { ignore_ws: true, context: git::DiffOpts::FILE, ..Default::default() };
                            }
                            let code = t.data.as_ref().and_then(|d| d.status.unstaged.iter().find(|c| c.path == path)).map_or('M', |c| c.code);
                            let change = git::Change { path: path.into(), old_path: None, code };
                            t.apply(&ctx.clone(), crate::repo::Action::Diff(crate::repo::DiffSrc::Worktree { change, staged: mode == "diff-staged" }));
                            if mode == "diff-all" {
                                t.apply(&ctx.clone(), crate::repo::Action::DiffOpts(git::DiffOpts { all: true, ..t.diff_opts }));
                            }
                        }
                    }
                    // `edit:<path>`: the in-place editor on an unstaged file (`editask:` with unsaved edits and the dialog).
                    q if q.starts_with("edit:") || q.starts_with("editask:") => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            let (mode, path) = q.split_once(':').unwrap();
                            let code = t.data.as_ref().and_then(|d| d.status.unstaged.iter().find(|c| c.path == path)).map_or('M', |c| c.code);
                            let change = git::Change { path: path.into(), old_path: None, code };
                            let ctx = ctx.clone();
                            t.apply(&ctx, crate::repo::Action::Diff(crate::repo::DiffSrc::Worktree { change, staged: false }));
                            t.apply(&ctx, crate::repo::Action::EditFile(path.into()));
                            if let (true, Some(ed)) = (mode == "editask", &mut t.edit) {
                                ed.text.insert_str(0, "// edited\n");
                                ed.ask = Some(None);
                            }
                        }
                    }
                    "wrap" => self.s.wrap = true,
                    "split" => self.s.split_diff = true,
                    "tree" => self.s.file_tree = true,
                    q if q.starts_with("zoom:") => ctx.set_zoom_factor(q[5..].parse().unwrap_or(1.0)),
                    // `compare-wt:<ref>[:<path>]`: compare the working tree with a ref, optionally opening a file's diff.
                    q if q.starts_with("compare-wt:") => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            let (rev, path) = q[11..].split_once(':').map_or((&q[11..], None), |(r, p)| (r, Some(p)));
                            t.apply(&ctx.clone(), crate::repo::Action::Select(crate::repo::Sel::Compare { from: rev.into(), to: git::WORKTREE.into(), three_dot: false }));
                            if let (Some(path), Ok(base)) = (path, git::run(&t.dir, &["rev-parse", rev])) {
                                let change = git::Change { path: path.into(), old_path: None, code: 'M' };
                                let src = crate::repo::DiffSrc::Commit { id: git::WORKTREE.into(), parent: Some(base.trim().into()), change };
                                t.apply(&ctx.clone(), crate::repo::Action::Diff(src));
                            }
                        }
                    }
                    // `commit-diff:<rev>:<path>`: a commit's diff (hunk revert / cherry-pick buttons).
                    q if q.starts_with("commit-diff:") => {
                        if let Some(t) = self.tabs.get_mut(self.active)
                            && let Some((rev, path)) = q["commit-diff:".len()..].split_once(':')
                        {
                            let id = git::run(&t.dir, &["rev-parse", rev]).unwrap_or_default().trim().to_owned();
                            let change = git::Change { path: path.into(), old_path: None, code: 'M' };
                            t.apply(&ctx.clone(), crate::repo::Action::Diff(crate::repo::DiffSrc::Commit { parent: Some(format!("{id}^")), id, change }));
                        }
                    }
                    q if q.starts_with("merge:") => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            t.apply(&ctx.clone(), crate::repo::Action::MergeEditor(q[6..].into()));
                        }
                    }
                    // `cleanup:all` also ticks "delete on remote" and "prune" (checks the remotes).
                    "cleanup" | "cleanup:all" => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            if view == "cleanup:all" {
                                let c = t.cleanup.get_or_insert_default();
                                (c.prune, c.remote) = (true, true);
                            }
                            t.apply(&ctx.clone(), crate::repo::Action::Cleanup);
                        }
                    }
                    // Smart ops: `absorb` (preview dialog), `split:<rev>`, `cherry:<branch>` (its commits not in HEAD, editor).
                    "absorb" => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            t.apply(&ctx.clone(), Action::Absorb);
                        }
                    }
                    q if q.starts_with("split:") || q.starts_with("cherry:") => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            let (kind, rev) = q.split_once(':').unwrap();
                            let a = if kind == "split" {
                                Action::Split(git::run(&t.dir, &["rev-parse", rev]).unwrap_or_default().trim().into())
                            } else {
                                let ids = git::run(&t.dir, &["rev-list", &format!("HEAD..{rev}")]).unwrap_or_default();
                                Action::MultiOp(crate::repo::MultiOp::CherryPickEdit, ids.lines().map(String::from).collect())
                            };
                            t.apply(&ctx.clone(), a);
                        }
                    }
                    // Commit Composer: `compose` (a canned plan, the last change left out; no AI),
                    // `recompose` (the same for the last two commits),
                    // `ai:compose` / `ai:explain-wip` (real requests; raise GLASIR_SCREENSHOT_FRAMES).
                    v @ ("compose" | "recompose") => {
                        let r = (v == "recompose").then(|| crate::repo::compose::Recompose { oldest: "HEAD~1".into(), picked: vec![], wip: false });
                        if let Some(t) = self.tabs.get_mut(self.active)
                            && let Ok((mut p, _)) = crate::repo::compose::Plan::load(&t.dir, r.as_ref())
                        {
                            let n = p.units.len();
                            let (a, b): (Vec<usize>, Vec<usize>) = (1..n.max(2)).partition(|i| *i <= n / 2);
                            let reply = serde_json::json!({"commits": [
                                {"message": "Add the parser", "units": a},
                                {"message": "Wire the parser into the CLI\n\nAlso documents the new flag.", "units": b},
                            ]});
                            let _ = p.fill(&reply.to_string());
                            t.compose = Some(p);
                        }
                    }
                    q if q.starts_with("ai:") => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            let p = if q == "ai:compose" { crate::ai::Purpose::Compose } else { crate::ai::Purpose::ExplainWip };
                            t.apply(&ctx.clone(), Action::Ai(p));
                        }
                    }
                    // `search:<query>`, or `searchf:<query>` with "Filter graph to results" on.
                    q if q.starts_with("search:") || q.starts_with("searchf:") => {
                        self.s.search_history = vec!["author:ann".into(), "path:src/main.rs".into()];
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            let mut s = Search::open(t.dir.clone());
                            let (kind, query) = q.split_once(':').unwrap();
                            (s.query, s.filter) = (query.into(), kind == "searchf");
                            t.search = Some(s);
                        }
                    }
                    // `blame:<path>`, `blame@<rev>:<path>`, `history:<path or dir/>`, `lines:<start>-<end>:<path>` (from HEAD).
                    q if q.starts_with("blame") || q.starts_with("history:") || q.starts_with("lines:") => {
                        let (mode, path) = q.split_once(':').unwrap();
                        let (mode, path) = match mode {
                            "lines" => {
                                let (r, path) = path.split_once(':').unwrap_or_default();
                                let (a, b) = r.split_once('-').unwrap_or_default();
                                (Mode::Lines { rev: Some("HEAD".into()), start: a.parse().unwrap_or(1), end: b.parse().unwrap_or(1) }, path)
                            }
                            "history" => (Mode::History, path),
                            m => (Mode::Blame(m.strip_prefix("blame@").map(Into::into)), path),
                        };
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            t.apply(&ctx.clone(), crate::repo::Action::FileView(path.into(), mode));
                        }
                    }
                    // `commitdrop:<src rev>:<dst rev>`: the menu after dragging one commit row onto another.
                    q if q.starts_with("commitdrop:") => {
                        let mut it = q[11..].splitn(2, ':');
                        if let (Some(src), Some(dst), Some(t)) = (it.next(), it.next(), self.tabs.get_mut(self.active)) {
                            let id = |r: &str| git::run(&t.dir, &["rev-parse", r]).map(|s| s.trim().to_owned()).unwrap_or_default();
                            let (src, dst) = (id(src), id(dst));
                            let branch = t.data.as_ref().and_then(|d| d.refs.iter().find(|r| r.head && r.target == dst)).map(|r| r.full.clone());
                            t.apply(&ctx.clone(), Action::DropCommit { src, dst: Some(dst), branch, squash: false });
                            while t.drop_task.is_some() {
                                std::thread::sleep(std::time::Duration::from_millis(5));
                                t.poll(ctx, &mut vec![]);
                            }
                            if let Some(m) = &mut t.commit_drop {
                                m.2 = egui::pos2(600.0, 300.0);
                            }
                        }
                    }
                    q if q.starts_with("drop:") => {
                        let mut it = q[5..].splitn(2, ':');
                        if let (Some(src), Some(dst), Some(t)) = (it.next(), it.next(), self.tabs.get_mut(self.active)) {
                            let full = |n: &str| t.data.as_ref().and_then(|d| d.refs.iter().find(|r| r.name == n)).map_or(n.into(), |r| r.full.clone());
                            t.drop_menu = Some((full(src), full(dst), egui::pos2(600.0, 300.0)));
                        }
                    }
                    // Graph views: `hover:<ref name>`, `view:fp`, `view:hide:<full ref>`, `view:solo:<full ref>`, `stash`.
                    q if q.starts_with("hover:") => {
                        if let Some(r) = self.tabs.get(self.active).and_then(|t| t.data.as_ref()).and_then(|d| d.refs.iter().find(|r| r.name == q[6..])) {
                            // The state `graph_view::hover_ref` records, pinned as hovered long ago and still.
                            ctx.data_mut(|m| m.insert_temp(Id::new("ref_hover"), (r.target.clone(), f64::NEG_INFINITY, u64::MAX - 1)));
                        }
                    }
                    q if q.starts_with("view:") => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            let mut v = t.view.clone();
                            match q[5..].split_once(':') {
                                Some(("hide", r)) => v.hidden = vec![r.into()],
                                Some(("solo", r)) => v.solo = vec![r.into()],
                                _ => v.first_parent = true,
                            }
                            if v != t.view {
                                t.apply(&ctx.clone(), Action::View(v));
                            }
                        }
                    }
                    // The hard-reset confirmation (to the second commit), with its list of files.
                    "reset-hard" => {
                        if let Some(t) = self.tabs.get_mut(self.active)
                            && let Some(o) = t.data.as_ref().and_then(|d| d.log.get(1).map(|c| crate::repo::reset_op(d, &c.id, "--hard")))
                        {
                            t.apply(&ctx.clone(), Action::Git(o));
                        }
                    }
                    "stash" => {
                        if let Some(t) = self.tabs.get_mut(self.active)
                            && let Some(id) = t.data.as_ref().and_then(|d| d.stashes.first()).map(|s| s.id.clone())
                        {
                            t.apply(&ctx.clone(), Action::Select(crate::repo::Sel::Commit(id)));
                        }
                    }
                    // The toolbar Stash button's message prompt.
                    "stash-prompt" => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            t.apply(&ctx.clone(), crate::repo::stash_prompt());
                        }
                    }
                    // The first and third stashes ⌘-clicked in the sidebar.
                    "stashes" => {
                        if let Some(t) = self.tabs.get_mut(self.active)
                            && let Some(ids) = t.data.as_ref().map(|d| d.stashes.iter().step_by(2).take(2).map(|s| s.id.clone()).collect::<Vec<_>>())
                            && let Some(last) = ids.last().cloned()
                        {
                            ctx.data_mut(|m| m.insert_temp(Id::new(crate::ui::sidebar::stashes::MULTI), ids));
                            t.apply(&ctx.clone(), Action::Select(crate::repo::Sel::Commit(last)));
                        }
                    }
                    // `reflog`, or `reflog:<n>` to select HEAD@{n} too.
                    q if q.starts_with("reflog") => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            t.apply(&ctx.clone(), Action::Reflog);
                            if let Some(n) = q.strip_prefix("reflog:").and_then(|n| n.parse::<usize>().ok()) {
                                t.reflog.as_mut().unwrap().sel = Some(n);
                                t.apply(&ctx.clone(), Action::Jump(format!("HEAD@{{{n}}}")));
                            }
                        }
                    }
                    // Remotes: `remote-add`, `remote-edit:<name>`, `doctor:<remote>`; `commit:<rev>` selects a
                    // commit (message autolinks); `identity:<host -> @domain>` sets a commit email rule.
                    q if q.starts_with("remote-") || q.starts_with("doctor:") || q.starts_with("commit:") => {
                        if let Some(t) = self.tabs.get_mut(self.active)
                            && let Some(d) = &t.data
                        {
                            let a = match q.split_once(':') {
                                Some(("remote-edit", r)) => Action::RemoteForm(crate::repo::remotes::Form::edit(d, r)),
                                Some(("doctor", r)) => Action::Doctor(r.into()),
                                Some((_, rev)) => Action::Jump(git::run(&t.dir, &["rev-parse", rev]).unwrap_or_default().trim().into()),
                                None => Action::RemoteForm(crate::repo::remotes::Form::add(d)),
                            };
                            t.apply(&ctx.clone(), a);
                        }
                    }
                    // `edit-msg:<rev>`: select the commit and open its inline message editor.
                    q if q.starts_with("edit-msg:") => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            let id: String = git::run(&t.dir, &["rev-parse", &q[9..]]).unwrap_or_default().trim().into();
                            let depth = t.data.as_ref().and_then(|d| d.reword_depth(&id).ok()).unwrap_or(0);
                            let message = git::run(&t.dir, &["log", "-1", "--format=%B", &id]).unwrap_or_default();
                            t.apply(&ctx.clone(), Action::Jump(id.clone()));
                            t.edit_msg = Some(crate::repo::MsgEdit::new(&id, &message, depth));
                        }
                    }
                    q if q.starts_with("identity:") => self.s.email_rules = q[9..].into(),
                    q if q.starts_with("palette") => {
                        self.palette = Some(super::palette::Palette { query: q.trim_start_matches("palette").trim_start_matches(':').into(), ..Default::default() })
                    }
                    // Ease of use: `shortcuts`, `welcome`, `toast-undo`, `drop-hint`, `deselect`, `tools` (Settings with a custom command).
                    "shortcuts" => self.shortcuts = true,
                    "welcome" => self.active = self.tabs.len(),
                    "toast-undo" => {
                        let undo = self.tabs.last().and_then(|t| Some((t.dir.clone(), t.undo_top()?)));
                        self.toast_undo(ctx, "Discard ✔ changes backed up".into(), false, undo);
                        self.toast(ctx, "Fetch ✔ From github.com:org/repo".into(), false);
                    }
                    // `row-hover:XxY`: a pointer pinned there (the WIP file rows' Stage button).
                    q if q.starts_with("row-hover:") => {
                        if let Some((x, y)) = q[10..].split_once('x').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))) {
                            ctx.data_mut(|m| m.insert_temp(Id::new("shot_pointer"), egui::pos2(x, y)));
                        }
                    }
                    // `rclick:XxY`: a right click there (a context menu), see `shot_input`.
                    q if q.starts_with("rclick:") => {
                        if let Some((x, y)) = q[7..].split_once('x').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))) {
                            ctx.data_mut(|m| m.insert_temp(Id::new("shot_rclick"), (egui::pos2(x, y), 0u8)));
                        }
                    }
                    q if q.starts_with("hscroll:") => {
                        let n: Vec<f32> = q[8..].split('x').filter_map(|v| v.parse().ok()).collect();
                        if let [x, y, dx] = n[..] {
                            ctx.data_mut(|m| m.insert_temp(Id::new("shot_wheel"), (egui::pos2(x, y), egui::vec2(-dx, 0.0), 0u8)));
                        }
                    }
                    "drop-hint" => self.drop_hint_shot = true,
                    // Keeps scrolling the graph (with GLASIR_PERF=1 and many GLASIR_SCREENSHOT_FRAMES).
                    "scroll-bench" => self.bench = Some(0),
                    // `graph-x:<px>`: the graph column scrolled sideways.
                    q if q.starts_with("graph-x:") => {
                        if let Some(d) = self.tabs.get(self.active).and_then(|t| t.data.as_ref()) {
                            let px: f32 = q[8..].parse().unwrap_or(0.0);
                            ctx.data_mut(|m| m.insert_temp(egui::Id::new(("graph_x", &d.git_dir)), px));
                        }
                    }
                    // HEAD's details (e.g. its signature badge) even with a WIP row.
                    "head" => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            if let Some(h) = t.data.as_ref().and_then(|d| d.status.head.clone()) {
                                t.apply(&ctx.clone(), Action::Select(crate::repo::Sel::Commit(h)));
                            }
                        }
                    }
                    // `tabs:<repo>`: another tab (aliased "Work"), the first stays active.
                    q if q.starts_with("tabs:") => {
                        let a = self.active;
                        self.open(ctx, std::path::Path::new(&q[5..]));
                        if let Some(t) = self.tabs.last() {
                            self.s.aliases.insert(t.dir.clone(), "Work".into());
                        }
                        self.active = a;
                    }
                    // Every unstaged file selected (the select-all cost of a huge WIP list).
                    "select-all" => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            t.file_sel = t.data.as_ref().map(|d| d.status.unstaged.iter().map(|c| (false, c.path.clone())).collect()).unwrap_or_default();
                        }
                    }
                    "deselect" => {
                        if let Some(t) = self.tabs.get_mut(self.active) {
                            t.sel = None;
                        }
                    }
                    "tools" => {
                        let c = |n: &str, cmd: &str| crate::repo::tools::Custom { name: n.into(), command: cmd.into() };
                        self.s.tools.commands = vec![c("Push for review", "git push origin HEAD:refs/for/{branch}"), c("Run tests", "cargo test")];
                        self.open_settings();
                    }
                    _ => {}
                }
            }
        }
        let Some((_, frames)) = &mut self.shot else { return };
        let wait: u32 = std::env::var("GLASIR_SCREENSHOT_FRAMES").ok().and_then(|f| f.parse().ok()).unwrap_or(20);
        if *frames == wait {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let img = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = img {
            let [w, h] = img.size;
            let mut buf = Vec::with_capacity(54 + w * h * 4);
            buf.extend(b"BM");
            buf.extend(((54 + w * h * 4) as u32).to_le_bytes());
            buf.extend([0u8; 4]);
            buf.extend(54u32.to_le_bytes());
            buf.extend(40u32.to_le_bytes());
            buf.extend((w as i32).to_le_bytes());
            buf.extend((-(h as i32)).to_le_bytes());
            buf.extend(1u16.to_le_bytes());
            buf.extend(32u16.to_le_bytes());
            buf.extend([0u8; 24]);
            for p in &img.pixels {
                buf.extend([p.b(), p.g(), p.r(), 255]);
            }
            let _ = std::fs::write(&self.shot.as_ref().unwrap().0, buf);
            std::process::exit(0);
        }
    }
}
