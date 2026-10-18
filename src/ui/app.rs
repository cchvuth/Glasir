//! The main window: `App` (open tabs, settings, global dialogs), the eframe
//! frame loop, global shortcuts and window setup. Each screen area lives in
//! its own `ui` module; this file only wires them together.

use super::settings::{Settings, SettingsDlg, toggle_flavor};
use super::toasts::Toast;
use super::welcome::CloneDlg;
use super::{command_log, graph_view, palette, repo_view};
use crate::git;
use crate::repo::message::Template;
use crate::repo::{Action, Msg, RepoTab, Search, Sel, Steps, Task, fetch_op, unstage_all_op};
use crate::theme::{self, icon};
use egui::{Frame, Key, Margin, Modifiers, RichText, Ui};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

type Draft = (String, String, Option<Template>, Vec<(String, String)>);

pub struct App {
    /// `GLASIR_SCREENSHOT=out.bmp`: save a screenshot once loaded, then exit (UI checks/CI).
    pub(super) shot: Option<(String, u32)>,
    /// `GLASIR_PERF=1`: per-frame CPU times, summarised on stderr every 2 s.
    perf: Option<(std::time::Instant, Vec<f32>)>,
    /// Protected branches changed: tabs reload once the Settings dialog closes, not per keystroke.
    stacks_stale: bool,
    /// Commits to load changed: tabs reload once the Settings dialog closes.
    pub(super) commits_stale: bool,
    /// Screenshot view `scroll-bench`: scroll the graph a page per frame.
    pub(super) bench: Option<usize>,
    pub(super) tabs: Vec<RepoTab>,
    drafts: HashMap<PathBuf, Draft>,
    steps: HashMap<PathBuf, Steps>,
    /// `== tabs.len()` shows the welcome page.
    pub(super) active: usize,
    pub(super) s: Settings,
    pub(super) toasts: Vec<Toast>,
    pub(super) clone: Option<CloneDlg>,
    pub(super) settings: Option<SettingsDlg>,
    pub(super) palette: Option<palette::Palette>,
    /// Git missing or too old: what to install (dismissable banner).
    pub(super) git_warn: Option<String>,
    pub(super) git_check: Option<Task<Option<String>>>,
    pub(super) show_log: bool,
    pub(super) focused: bool,
    /// Rename-tab dialog: the repo path and the alias being typed.
    pub(super) rename: Option<(PathBuf, String)>,
    /// The OS draws glass behind the window, so the canvas stays translucent.
    pub(super) glass_backdrop: bool,
    pub(super) shortcuts: bool,
    /// Screenshot harness: paint the drop hint without a drag.
    pub(super) drop_hint_shot: bool,
    /// The window was asked to close while an op ran: close once it's done.
    quit_pending: bool,
    /// The window was asked to close while N terminal commands ran: "Quit anyway?".
    pub(super) quit_ask: Option<usize>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext, open: Option<PathBuf>) -> Self {
        let mut s: Settings = cc.storage.and_then(|st| eframe::get_value(st, "settings")).unwrap_or_default();
        theme::install_fonts(&cc.egui_ctx);
        theme::set_transparency(s.transparency);
        if std::mem::take(&mut s.light) && s.flavor == theme::Flavor::Midnight {
            s.flavor = theme::Flavor::Daylight;
        }
        theme::apply(&cc.egui_ctx, s.flavor);
        cc.egui_ctx.set_zoom_factor(s.zoom.clamp(0.5, 3.0));
        let shot = std::env::var("GLASIR_SCREENSHOT").ok().map(|p| (p, 0));
        #[cfg(target_os = "macos")]
        let glass_backdrop = s.native_glass && native_glass(cc);
        #[cfg(not(target_os = "macos"))]
        let glass_backdrop = false;
        #[cfg(all(target_os = "macos", not(test)))]
        mac_quit::install(&cc.egui_ctx);
        let mut app = App {
            shot,
            stacks_stale: false,
            commits_stale: false,
            perf: std::env::var_os("GLASIR_PERF").map(|_| (std::time::Instant::now(), vec![])),
            bench: None,
            tabs: vec![],
            drafts: HashMap::new(),
            steps: HashMap::new(),
            active: 0,
            toasts: vec![],
            clone: None,
            settings: None,
            palette: None,
            git_warn: None,
            git_check: Some(Task::spawn(&cc.egui_ctx, git::check_git)),
            show_log: false,
            focused: true,
            rename: None,
            glass_backdrop,
            shortcuts: false,
            drop_hint_shot: false,
            quit_pending: false,
            quit_ask: None,
            s,
        };
        let active = app.s.active;
        let want = app.s.open.get(active).cloned();
        for p in app.s.open.clone() {
            // Saved paths are top-levels: no `rev-parse` on the UI thread before the first frame.
            if p.join(".git").exists() {
                app.add_tab(&cc.egui_ctx, p);
            } else if p.exists() {
                let top = git::run(&p, &["rev-parse", "--show-toplevel"]).ok().map(|t| PathBuf::from(t.trim()));
                match top.filter(|t| t.canonicalize().ok() == p.canonicalize().ok()) {
                    Some(t) => app.add_tab(&cc.egui_ctx, t),
                    None => app.toast(&cc.egui_ctx, format!("{} is no longer a repository", p.display()), true),
                }
            }
        }
        // Saved repos that are gone shift the indices: find the active one by path.
        app.active = want.and_then(|w| app.tabs.iter().position(|t| t.dir == w)).unwrap_or(active.min(app.tabs.len()));
        if let Some(p) = open {
            app.open(&cc.egui_ctx, &p);
        }
        app
    }

    pub(super) fn open(&mut self, ctx: &egui::Context, path: &Path) {
        let path = if path.is_file() { path.parent().unwrap_or(path) } else { path };
        match git::run(path, &["rev-parse", "--show-toplevel"]) {
            Ok(top) => {
                let top = PathBuf::from(top.trim());
                self.add_tab(ctx, top.clone());
                self.remember(top);
            }
            Err(e) => self.toast(ctx, format!("Not a git repository: {}\n{e}", path.display()), true),
        }
    }

    /// Selects the tab of the repository at `top` (its top-level), opening it if needed.
    fn add_tab(&mut self, ctx: &egui::Context, top: PathBuf) {
        self.active = match self.tabs.iter().position(|t| t.dir == top) {
            Some(i) => i,
            None => {
                let t = self.new_tab(ctx, top);
                self.tabs.push(t);
                self.tabs.len() - 1
            }
        };
    }

    fn new_tab(&mut self, ctx: &egui::Context, top: PathBuf) -> RepoTab {
        let mut t = RepoTab::new(ctx, top.clone(), self.s.max_commits, self.s.views.get(&top).cloned().unwrap_or_default());
        if let Some(draft) = self.drafts.remove(&top) {
            (t.summary, t.body, t.template, t.coauthors) = draft;
        } else if let Some(saved) = draft_file(&top).and_then(|f| std::fs::read_to_string(f).ok()) {
            let (summary, body) = saved.split_once("\n\n").unwrap_or((&saved, ""));
            (t.summary, t.body) = (summary.to_owned(), body.to_owned());
        }
        if let Some(steps) = self.steps.remove(&top) {
            t.put_steps(steps);
        }
        t
    }

    /// Shows the worktree at `path` in tab `i` (a new tab while that one is busy or has work only it holds: unsaved edits, a split, a rebase plan, a merge, a composer plan, an amend, an open external tool, a commit message Claude is writing). A typed commit message and the undo / redo steps go with the worktree they belong to: they are back when that worktree is shown again.
    fn switch(&mut self, ctx: &egui::Context, i: usize, path: &Path) {
        let top = match git::run(path, &["rev-parse", "--show-toplevel"]) {
            Ok(top) => PathBuf::from(top.trim()),
            Err(e) => return self.toast(ctx, format!("Not a git repository: {}\n{e}", path.display()), true),
        };
        if !self.tabs.get(i).is_some_and(replaceable) || self.tabs.iter().any(|t| t.dir == top) {
            self.add_tab(ctx, top.clone());
            return self.remember(top);
        }
        match draft(&self.tabs[i]) {
            Some(draft) => _ = self.drafts.insert(self.tabs[i].dir.clone(), draft),
            None => forget_draft(&self.tabs[i].dir),
        }
        let steps = self.tabs[i].take_steps();
        self.steps.insert(self.tabs[i].dir.clone(), steps);
        drop_data_off_thread(&mut self.tabs[i]);
        self.tabs[i] = self.new_tab(ctx, top.clone());
        self.active = i;
        self.remember(top);
    }

    fn remember(&mut self, top: PathBuf) {
        self.s.recent.retain(|p| *p != top);
        self.s.recent.insert(0, top);
        self.s.recent.truncate(15);
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.memory(|m| m.focused().is_some());
        let modal = super::format::modal_open(ctx);
        let (cmd, shift) = ctx.input(|i| (i.modifiers.command, i.modifiers.shift));
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        // Consumed: off macOS a text field would also take Ctrl+K / W / U as readline edits.
        let eat = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        if !modal
            && ctx.input(|i| i.modifiers.ctrl && !i.modifiers.mac_cmd && i.key_pressed(Key::Backtick))
            && let Some(t) = self.tabs.get_mut(self.active)
        {
            t.apply(ctx, if self.s.terminal || !t.term.tabs.is_empty() { Action::ToggleTerminal } else { Action::Terminal });
        }
        // Off macOS ⌘ is Ctrl, which the shell needs (^R, ^L, ^W, ^Z...).
        if !cfg!(target_os = "macos") && ctx.memory(|m| m.has_focus(super::terminal::id())) {
            return;
        }
        if cmd && !modal && pressed(Key::O) {
            self.pick_and_open(ctx);
        }
        if cmd && !modal && pressed(Key::Comma) {
            self.open_settings();
        }
        if (!modal || self.shortcuts) && ((cmd && pressed(Key::Slash)) || (!typing && pressed(Key::Questionmark))) {
            self.shortcuts = !self.shortcuts;
        }
        if cmd && shift && pressed(Key::L) {
            self.show_log = !self.show_log;
        }
        let fetch = cmd && !shift && pressed(Key::L);
        if !modal && (eat(Modifiers::COMMAND, Key::P) | eat(Modifiers::COMMAND, Key::K)) {
            self.palette = Some(Default::default());
        }
        if modal {
            return;
        }
        if cmd && pressed(Key::T) {
            self.active = self.tabs.len();
        }
        if self.active < self.tabs.len() && eat(Modifiers::COMMAND, Key::W) {
            self.close_tab(ctx, self.active);
        }
        const NUMS: [Key; 9] = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        if cmd && !shift && let Some(n) = NUMS.iter().position(|&k| pressed(k)) {
            let n = if n == 8 { self.tabs.len().saturating_sub(1) } else { n }; // ⌘9 = last tab
            if n < self.tabs.len() {
                self.active = n;
            }
        }
        if (eat(Modifiers::CTRL, Key::Tab) | eat(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab)) && !self.tabs.is_empty() {
            let n = self.tabs.len();
            self.active = match (self.active >= n, shift) {
                (true, false) => 0,
                (true, true) => n - 1,
                (false, false) => (self.active + 1) % n,
                (false, true) => (self.active + n - 1) % n,
            };
        }
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        let mut act = None;
        if (pressed(Key::F5) && !ctx.memory(|m| m.has_focus(super::terminal::id()))) || (cmd && pressed(Key::R)) {
            act = Some(Action::Reload);
        }
        if let (true, Some(d)) = (fetch, &t.data) {
            act = Some(Action::Git(fetch_op(d)));
        }
        if !typing && cmd && pressed(Key::Z) {
            act = Some(if shift { Action::Redo } else { Action::Undo });
        }
        if !typing && cmd && pressed(Key::Y) {
            act = Some(Action::Redo);
        }
        if cmd && pressed(Key::F) && t.edit.is_none() && t.merge.is_none() && t.file_view.is_none() && t.rebase.is_none() && t.reflog.is_none() {
            t.diff = None;
            t.search.get_or_insert_with(|| Search::open(t.dir.clone())).focus = true;
        }
        if cmd && shift && pressed(Key::S) && t.edit.is_none() {
            act = t.data.as_ref().and_then(|d| crate::repo::stage_all_op(&d.status)).map(Action::Git).or(act);
        }
        if cmd && shift && pressed(Key::M) && t.data.as_ref().is_some_and(|d| d.wip) {
            t.focus_summary = true;
            act = Some(Action::Select(Sel::Wip));
        }
        if eat(Modifiers::COMMAND | Modifiers::SHIFT, Key::U) {
            act = Some(Action::Git(unstage_all_op()));
        }
        // S / U: stage / unstage the files selected in the WIP lists (not while typing or in a dialog).
        let plain = ctx.input(|i| !i.modifiers.any());
        for (key, stage) in [(Key::S, true), (Key::U, false)] {
            if !typing && plain && pressed(key) {
                act = t.file_key(stage).or(act);
            }
        }
        if let Some(a) = act {
            t.apply(ctx, a);
        }
    }

    /// Dropping a tab mid-op drops the op's result: its recovery (e.g. a failed Compose's
    /// unstage) and its undo step would never happen.
    pub(super) fn close_tab(&mut self, ctx: &egui::Context, i: usize) {
        if let Some(w) = self.tabs.get(i).and_then(RepoTab::unsaved) {
            return self.toast(ctx, format!("{w} first."), true);
        }
        if let Some(b) = self.tabs.get(i).and_then(RepoTab::busy).map(str::to_owned) {
            return self.toast(ctx, format!("Close this tab once '{b}' finishes (or cancel it)."), true);
        }
        if self.tabs.get(i).is_some_and(|t| t.term.busy()) {
            return self.toast(ctx, "A command is still running in this tab's terminal: let it finish or stop it there, then close the tab.".into(), true);
        }
        if let Some(o) = self.tabs.get(i).and_then(|t| t.ext_tools.first()) {
            return self.toast(ctx, format!("The external {} tool is still open on {}: close it, then close the tab.", o.kind, o.path), true);
        }
        match draft(&self.tabs[i]) {
            Some(draft) => _ = self.drafts.insert(self.tabs[i].dir.clone(), draft),
            None => forget_draft(&self.tabs[i].dir),
        }
        let steps = self.tabs[i].take_steps();
        self.steps.insert(self.tabs[i].dir.clone(), steps);
        drop_data_off_thread(&mut self.tabs[i]);
        self.tabs.remove(i);
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    pub(super) fn pick_and_open(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new().set_title("Open repository").pick_folder() {
            self.open(ctx, &p);
        }
    }

    fn report_perf(&mut self, frame: &eframe::Frame) {
        let Some((since, times)) = &mut self.perf else { return };
        times.extend(frame.info().cpu_usage.map(|s| s * 1000.0));
        if since.elapsed().as_secs_f32() >= 2.0 && !times.is_empty() {
            times.sort_by(f32::total_cmp);
            let avg = times.iter().sum::<f32>() / times.len() as f32;
            let p95 = times[(times.len() * 95 / 100).min(times.len() - 1)];
            eprintln!("perf: {} frames  avg {avg:.2} ms  p95 {p95:.2} ms  max {:.2} ms", times.len(), times[times.len() - 1]);
            (*since, *times) = (std::time::Instant::now(), vec![]);
        }
    }

    fn poll_tabs(&mut self, ctx: &egui::Context) {
        let mut msgs: Vec<(Msg, Option<(PathBuf, u64)>)> = vec![];
        for (i, t) in self.tabs.iter_mut().enumerate() {
            let (top, mut m, had) = (t.undo_top(), vec![], t.choice.is_some());
            t.poll(ctx, &mut m);
            if i != self.active && !had && let Some(c) = &t.choice {
                m.push(Msg { text: format!("{}: {} — open the tab to choose", t.name, c.title), error: true });
            }
            m.append(&mut t.notes);
            let undo = t.undo_top().filter(|&id| Some(id) > top).map(|id| (t.dir.clone(), id));
            let n = m.len();
            msgs.extend(m.into_iter().enumerate().map(|(i, m)| (m, undo.clone().filter(|_| i + 1 == n))));
        }
        let opens: Vec<PathBuf> = self.tabs.iter_mut().filter_map(|t| t.open_tab.take()).collect();
        for p in opens {
            self.open(ctx, &p);
        }
        let switches: Vec<(usize, PathBuf)> = self.tabs.iter_mut().enumerate().filter_map(|(i, t)| t.switch_to.take().map(|p| (i, p))).collect();
        for (i, p) in switches {
            self.switch(ctx, i, &p);
        }
        for (m, undo) in msgs {
            self.toast_undo(ctx, m.text, m.error, undo);
        }
    }

    /// `unsaved`: the first tab with work only it holds, and what to do about it.
    fn guard_close(&mut self, ctx: &egui::Context, op_running: bool, unsaved: Option<(usize, String)>) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        if let Some((i, w)) = unsaved {
            refuse_close(ctx);
            (self.active, self.quit_pending) = (i, false);
            return self.toast(ctx, format!("{w} before quitting."), true);
        }
        let n: usize = self.tabs.iter().map(|t| t.term.running() + t.ext_tools.len()).sum();
        if !op_running && n > 0 {
            refuse_close(ctx);
            self.quit_ask = Some(n);
        }
    }

    fn quit_dialog(&mut self, ctx: &egui::Context) {
        let Some(n) = self.quit_ask else { return };
        let mut done = None;
        egui::Modal::new(egui::Id::new("quit_terminal")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("Quit Glasir?");
            ui.label(if n == 1 { "A terminal command or external tool is still running. Quitting stops it.".into() } else { format!("{n} terminal commands or external tools are still running. Quitting stops them.") });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("Quit anyway").strong()).fill(theme::del().gamma_multiply(0.6))).clicked() {
                    done = Some(true);
                }
                if super::format::dismissed(ui, "Cancel") {
                    done = Some(false);
                }
            });
        });
        if let Some(quit) = done {
            self.quit_ask = None;
            if quit {
                for t in &mut self.tabs {
                    t.term = Default::default();
                    t.ext_tools.drain(..).for_each(|o| o.cancel.cancel());
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn command_palette(&mut self, ctx: &egui::Context) {
        let Some(p) = &mut self.palette else { return };
        if p.items.is_empty() {
            p.dir = self.tabs.get(self.active).map(|t| t.dir.clone());
            p.items = palette::items(self.tabs.get(self.active), &self.s.recent, self.s.pull, self.s.terminal || self.tabs.get(self.active).is_some_and(|t| !t.term.tabs.is_empty()));
        }
        let Some(choice) = palette::show(ctx, p) else { return };
        let dir = self.palette.take().and_then(|p| p.dir);
        match choice {
            None => {}
            Some(palette::Cmd::Tab(a)) => {
                if let Some(t) = self.tabs.iter_mut().find(|t| Some(&t.dir) == dir.as_ref()) {
                    t.apply(ctx, a);
                }
            }
            Some(palette::Cmd::Open(p)) => self.open(ctx, &p),
            Some(palette::Cmd::PickOpen) => self.pick_and_open(ctx),
            Some(palette::Cmd::Clone) => self.open_clone(),
            Some(palette::Cmd::Settings) => self.open_settings(),
            Some(palette::Cmd::CommandLog) => self.show_log = !self.show_log,
            Some(palette::Cmd::Shortcuts) => self.shortcuts = true,
            Some(palette::Cmd::Theme) => toggle_flavor(&mut self.s, ctx),
        }
    }
}

fn drop_hint(ctx: &egui::Context, force: bool) {
    if !force && ctx.input(|i| i.raw.hovered_files.is_empty()) {
        return;
    }
    let screen = ctx.content_rect();
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop_hint")));
    p.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(110));
    let r = egui::Rect::from_center_size(screen.center(), egui::vec2(380.0, 120.0));
    p.rect_filled(r, 18.0, ctx.global_style().visuals.window_fill);
    p.rect_stroke(r.shrink(6.0), 14.0, egui::Stroke::new(2.0, theme::accent()), egui::StrokeKind::Inside);
    let color = ctx.global_style().visuals.text_color();
    p.text(r.center() - egui::vec2(0.0, 14.0), egui::Align2::CENTER_CENTER, icon::FOLDER_OPEN, egui::FontId::proportional(30.0), theme::accent());
    p.text(r.center() + egui::vec2(0.0, 24.0), egui::Align2::CENTER_CENTER, "Drop a folder to open it as a repository", egui::FontId::proportional(15.0), color);
}

/// macOS 26+: put an NSGlassEffectView *behind* egui. window-vibrancy adds it as a
/// subview, which AppKit composites above the Metal layer egui draws into, so
/// we push its layer back with a negative zPosition.
#[cfg(target_os = "macos")]
fn native_glass(cc: &eframe::CreationContext) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use window_vibrancy::{LiquidGlassOptions, NSGlassEffectViewStyle, apply_liquid_glass};
    if apply_liquid_glass(cc, LiquidGlassOptions::new(NSGlassEffectViewStyle::Regular)).is_err() {
        return false;
    }
    let Ok(h) = cc.window_handle() else { return false };
    let RawWindowHandle::AppKit(h) = h.as_raw() else { return false };
    let view = h.ns_view.as_ptr().cast::<objc2::runtime::AnyObject>();
    // SAFETY: plain AppKit/CoreAnimation getters and setters on the main thread.
    unsafe {
        let subviews: *mut objc2::runtime::AnyObject = objc2::msg_send![view, subviews];
        let n: usize = objc2::msg_send![subviews, count];
        for i in 0..n {
            let sv: *mut objc2::runtime::AnyObject = objc2::msg_send![subviews, objectAtIndex: i];
            let _: () = objc2::msg_send![sv, setWantsLayer: true];
            let layer: *mut objc2::runtime::AnyObject = objc2::msg_send![sv, layer];
            if !layer.is_null() {
                let _: () = objc2::msg_send![layer, setZPosition: -1.0f64];
            }
        }
    }
    true
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if self.shot.is_some() {
            super::screenshot::shot_input(ctx, raw);
        }
    }
    /// Runs before every `ui`, and alone while the window is hidden (minimized, occluded,
    /// another Space): ops finish and the quit guards hold there too.
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll_tabs(ctx);
        let busy = self.tabs.iter().find_map(|t| t.busy().map(str::to_owned));
        let unsaved = self.tabs.iter().enumerate().find_map(|(i, t)| t.unsaved().map(|w| (i, w)));
        #[cfg(target_os = "macos")]
        mac_quit::BUSY.store(
            busy.is_some() || unsaved.is_some() || self.tabs.iter().any(|t| t.term.busy() || !t.ext_tools.is_empty()),
            std::sync::atomic::Ordering::SeqCst,
        );
        if hold_close(ctx, busy.as_deref().filter(|_| unsaved.is_none()), &mut self.quit_pending) {
            self.toast(ctx, format!("Quitting once '{}' finishes (or cancel it).", busy.as_deref().unwrap_or_default()), false);
        }
        self.guard_close(ctx, busy.is_some(), unsaved);
    }
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.report_perf(frame);
        crate::repo::tools::set(&self.s.tools);
        crate::repo::avatars::set_enabled(self.s.avatars);
        crate::repo::merge_opts::set_pick_x(self.s.cherry_pick_x);
        self.stacks_stale |= crate::repo::stack::set_follow(self.s.stacks, &self.s.keep_branches);
        if self.stacks_stale && self.settings.is_none() {
            self.stacks_stale = false;
            self.tabs.iter_mut().for_each(|t| t.reload(&ctx, true));
        }
        self.s.zoom = ctx.zoom_factor(); // ⌘+ / ⌘- / ⌘0 are egui's own; persisted here
        for p in ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect::<Vec<_>>()) {
            self.open(&ctx, &p);
        }
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if focused && !self.focused
            && let Some(t) = self.tabs.get_mut(self.active)
        {
            // The watcher reports outside changes while it runs; without it, focus is the cue.
            t.reload(&ctx, !t.watching());
        }
        let every = if self.shot.is_some() { 0 } else { self.s.auto_fetch };
        for (i, t) in self.tabs.iter_mut().enumerate() {
            t.auto_fetch(&ctx, every, i == self.active && focused && !self.focused);
            if !self.s.terminal {
                t.term.keep_busy();
            }
        }
        self.focused = focused;
        let menu = self.tabs.get_mut(self.active).filter(|t| t.drop_menu.is_some() || t.commit_drop.is_some());
        if (menu.is_some() || egui::Popup::is_any_open(&ctx)) && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            egui::Popup::close_all(&ctx);
            if let Some(t) = menu {
                (t.drop_menu, t.commit_drop) = (None, None);
            }
        }
        self.shortcuts(&ctx);

        theme::canvas(ui.painter(), ui.max_rect(), self.glass_backdrop);
        let left = if cfg!(target_os = "macos") { 84 } else { 12 };
        egui::Panel::top("tabs")
            .show_separator_line(false)
            .frame(Frame::NONE.inner_margin(Margin { left, right: 12, top: 7, bottom: 4 }))
            .show(ui, |ui| self.tab_bar(ui));
        if let Some(w) = Task::poll(&mut self.git_check) {
            self.git_warn = w;
        }
        if let Some(w) = &self.git_warn {
            let mut dismiss = false;
            egui::Panel::top("git_warn").show_separator_line(false).frame(Frame::NONE.inner_margin(Margin { left: 14, right: 14, top: 2, bottom: 6 })).show(ui, |ui| {
                theme::group(ui, |ui| {
                    ui.add_space(6.0);
                    ui.label(RichText::new(icon::WARNING).color(theme::changed()).size(16.0));
                    ui.add(egui::Label::new(w.as_str()).wrap());
                    dismiss = theme::pill(ui, icon::X).on_hover_text("Dismiss").clicked();
                });
            });
            if dismiss {
                self.git_warn = None;
            }
        }
        if self.show_log {
            egui::Panel::bottom("cmdlog")
                .show_separator_line(false)
                .resizable(true)
                .default_size(230.0)
                .frame(Frame::NONE.inner_margin(Margin { left: 12, right: 12, top: 0, bottom: 12 }))
                .show(ui, |ui| theme::card(ui, 150, |ui| command_log::show(ui, &mut self.show_log)));
        }
        match self.tabs.get_mut(self.active) {
            Some(t) => repo_view::show(t, ui, &mut self.s),
            None => self.welcome(ui),
        }
        self.rename_dialog(&ctx);
        self.clone_dialog(&ctx);
        self.settings_dialog(&ctx);
        self.command_palette(&ctx);
        self.quit_dialog(&ctx);
        if self.shortcuts {
            self.shortcuts = super::shortcuts::show(&ctx);
        }
        drop_hint(&ctx, self.drop_hint_shot);
        self.show_toasts(&ctx);
        self.screenshot(&ctx);
        graph_view::dnd::preview(&ctx);
        super::format::note_focus(&ctx);
    }

    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.persist_drafts();
        self.s.open = self.tabs.iter().map(|t| t.dir.clone()).collect();
        self.s.active = self.active;
        eframe::set_value(storage, "settings", &self.s);
    }
}

const DRAFT_FILE: &str = "GLASIR_DRAFT";

impl App {
    fn persist_drafts(&self) {
        let open = self.tabs.iter().filter(|t| !t.amend).map(|t| (t.dir.as_path(), draft(t)));
        let closed = self.drafts.iter().map(|(dir, d)| (dir.as_path(), Some(d.clone())));
        for (dir, d) in open.chain(closed) {
            match d {
                Some(d) => _ = draft_file(dir).map(|f| std::fs::write(f, saved_text(&d))),
                None => forget_draft(dir),
            }
        }
    }
}

fn draft_file(dir: &Path) -> Option<PathBuf> {
    let g = dir.join(".git");
    let g = if g.is_dir() { g } else { git::git_dir(dir).ok()? };
    Some(g.join(DRAFT_FILE))
}

fn forget_draft(dir: &Path) {
    if let Some(f) = draft_file(dir).filter(|f| f.exists()) {
        let _ = std::fs::remove_file(f);
    }
}

fn saved_text(d: &Draft) -> String {
    let msg = format!("{}\n\n{}", d.0, d.1);
    d.2.as_ref().map_or(msg.clone(), |t| crate::repo::message::strip_comments(&msg, t))
}

fn replaceable(t: &RepoTab) -> bool {
    t.unsaved().is_none() && t.busy().is_none() && !t.term.busy() && t.split.is_none() && t.rebase.is_none() && t.merge.is_none() && t.compose.is_none() && !t.amend
        && t.ext_tools.is_empty()
        && !t.ai_busy().is_some_and(|p| matches!(p, crate::ai::Purpose::CommitMessage { .. }))
}

fn draft(t: &RepoTab) -> Option<Draft> {
    let untouched = t.template.as_ref().is_some_and(|m| (&m.summary, &m.body) == (&t.summary, &t.body));
    ((!t.summary.is_empty() || !t.body.is_empty()) && !untouched).then(|| (t.summary.clone(), t.body.clone(), t.template.clone(), t.coauthors.clone()))
}

fn drop_data_off_thread(t: &mut RepoTab) {
    if let Some(d) = t.data.take() {
        std::thread::spawn(move || drop(d));
    }
}

/// Closing while an op runs would leave it half-applied: the rest of its commands and
/// `poll_op`'s recovery (e.g. a failed Compose's unstage) never run. Holds the window
/// open until `busy` is None, then closes it. True when the close was first held.
fn hold_close(ctx: &egui::Context, busy: Option<&str>, pending: &mut bool) -> bool {
    if busy.is_some() && ctx.input(|i| i.viewport().close_requested()) {
        refuse_close(ctx);
        return !std::mem::replace(pending, true);
    }
    if busy.is_none() && std::mem::take(pending) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    false
}

/// Keeps the window open; a hidden one comes back, so its toast or dialog says why.
fn refuse_close(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
    if ctx.input(|i| i.viewport().visible() == Some(false)) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }
}

/// ⌘Q and the Dock's Quit send `terminate:`, which winit turns into an exit with no close request,
/// so `hold_close` never sees it: while an op, terminal command or external tool runs, or work is unsaved, request a close instead.
#[cfg(target_os = "macos")]
mod mac_quit {
    use objc2::runtime::{AnyObject, Sel};
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    pub static BUSY: AtomicBool = AtomicBool::new(false);
    static CTX: OnceLock<egui::Context> = OnceLock::new();

    /// `applicationShouldTerminate:` → NSTerminateCancel (0) or NSTerminateNow (1).
    extern "C-unwind" fn should_terminate(_: *mut AnyObject, _: Sel, _: *mut AnyObject) -> usize {
        match CTX.get() {
            Some(ctx) if BUSY.load(Ordering::SeqCst) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                0
            }
            _ => 1,
        }
    }

    #[cfg(not(test))]
    pub fn install(ctx: &egui::Context) {
        use objc2::runtime::{AnyClass, Imp};
        let _ = CTX.set(ctx.clone());
        // SAFETY: main thread; winit's delegate class lacks this method, so this only adds it.
        unsafe {
            let app: *mut AnyObject = objc2::msg_send![objc2::class!(NSApplication), sharedApplication];
            let delegate: *mut AnyObject = objc2::msg_send![app, delegate];
            let Some(d) = delegate.as_ref() else { return };
            let imp: Imp = std::mem::transmute(should_terminate as extern "C-unwind" fn(_, _, _) -> usize);
            objc2::ffi::class_addMethod(d.class() as *const AnyClass as *mut AnyClass, objc2::sel!(applicationShouldTerminate:), imp, c"Q@:@".as_ptr());
            // AppKit may cache which delegate methods exist when the delegate is set.
            let _: () = objc2::msg_send![app, setDelegate: delegate];
        }
    }

    #[test]
    fn quit_mid_op_becomes_a_held_close() {
        let ctx = egui::Context::default();
        let _ = CTX.set(ctx.clone());
        let null = std::ptr::null_mut();
        BUSY.store(true, Ordering::SeqCst);
        assert_eq!(should_terminate(null, objc2::sel!(applicationShouldTerminate:), null), 0);
        let mut out = ctx.run_ui(Default::default(), |_| {});
        out.textures_delta.clear();
        assert!(out.viewport_output[&egui::ViewportId::ROOT].commands.contains(&egui::ViewportCommand::Close));
        BUSY.store(false, Ordering::SeqCst);
        assert_eq!(should_terminate(null, objc2::sel!(applicationShouldTerminate:), null), 1);
    }
}

#[cfg(test)]
impl App {
    /// One shown frame, as eframe runs it.
    fn pass(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        use eframe::App as _;
        self.logic(ui.ctx(), frame);
        self.ui(ui, frame);
    }
}

#[cfg(test)]
mod close_tests {
    use egui::{RawInput, ViewportCommand, ViewportEvent, ViewportId};

    /// Minimized, occluded or on another Space, eframe runs only `logic`: the quit guards hold
    /// there too and bring the window back, and the toast lasts until it can be seen.
    #[test]
    fn closing_a_hidden_window_still_keeps_unsaved_work() {
        use eframe::App as _;
        let dir = std::env::temp_dir().join(format!("glasir-quit-hidden-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::git::run(&dir, &["init", "-q"]).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let ctx = egui::Context::default();
        let mut app = super::App::new(&eframe::CreationContext::_new_kittest(ctx.clone()), Some(dir.clone()));
        let mut ed = crate::repo::file_edit::FileEdit::load(&dir, "a.txt").unwrap();
        ed.text.push_str("two\n");
        app.tabs[0].edit = Some(ed);
        let n = app.toasts.len();
        let mut raw = RawInput::default();
        let vp = raw.viewports.entry(ViewportId::ROOT).or_default();
        (vp.minimized, vp.occluded) = (Some(true), Some(true));
        vp.events.push(ViewportEvent::Close);
        let out = ctx.run_logic(&raw, |ctx| app.logic(ctx, &mut eframe::Frame::_new_kittest()));
        let cmds = &out.viewport_commands[&ViewportId::ROOT];
        for c in [ViewportCommand::CancelClose, ViewportCommand::Minimized(false), ViewportCommand::Focus] {
            assert!(cmds.contains(&c), "{c:?} missing from {cmds:?}");
        }
        assert_eq!(app.toasts.len(), n + 1);
        let shown = RawInput { time: Some(1000.0), ..Default::default() };
        ctx.run_ui(shown, |ui| app.show_toasts(ui.ctx())).textures_delta.clear();
        assert_eq!(app.toasts.len(), n + 1, "shown again long after: the toast says why");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_open_external_tool_keeps_its_tab_and_is_asked_about_on_quit() {
        use crate::repo::{Action, Op};
        let dir = std::env::temp_dir().join(format!("glasir-quit-tool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::git::run(&dir, &["init", "-q"]).unwrap();
        let ctx = egui::Context::default();
        let mut app = super::App::new(&eframe::CreationContext::_new_kittest(ctx.clone()), Some(dir.clone()));
        let tool = crate::repo::tools::mergetool_op("f").label;
        app.tabs[0].apply(&ctx, Action::Git(Op::new(&tool, &["-c", "alias.slow=!sleep 30", "slow"])));
        assert!(app.tabs[0].busy().is_none() && app.tabs[0].ext_tools.len() == 1);
        let n = app.toasts.len();
        app.close_tab(&ctx, 0);
        assert_eq!((app.tabs.len(), app.toasts.len()), (1, n + 1), "the tab stays, saying why");
        assert!(close(&ctx, &mut app).contains(&ViewportCommand::CancelClose));
        assert_eq!(app.quit_ask, Some(1));
        app.tabs[0].ext_tools.drain(..).for_each(|o| o.cancel.cancel());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn close_waits_for_the_running_op() {
        let ctx = egui::Context::default();
        let mut pending = false;
        let frame = |close: bool, busy: Option<&str>, pending: &mut bool| {
            let mut raw = RawInput::default();
            if close {
                raw.viewports.entry(ViewportId::ROOT).or_default().events.push(ViewportEvent::Close);
            }
            let mut held = false;
            let mut out = ctx.run_ui(raw, |ui| held = super::hold_close(ui.ctx(), busy, pending));
            out.textures_delta.clear();
            (held, out.viewport_output.get(&ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default())
        };
        let (held, cmds) = frame(true, Some("Compose 2 commits"), &mut pending);
        assert!(held && cmds.contains(&ViewportCommand::CancelClose));
        assert_eq!(frame(false, Some("Compose 2 commits"), &mut pending).1, vec![]);
        let (held, cmds) = frame(false, None, &mut pending);
        assert!(!held && cmds.contains(&ViewportCommand::Close));
        let (_, cmds) = frame(true, None, &mut pending);
        assert!(!cmds.contains(&ViewportCommand::CancelClose));
    }

    /// A shell's foreground job (vim, ssh, a dev server) may never end: asking beats holding
    /// the quit and closing the window minutes later, out of the blue.
    #[cfg(unix)]
    #[test]
    fn quit_with_a_terminal_command_asks_instead_of_waiting() {
        let dir = std::env::temp_dir().join(format!("glasir-quit-terminal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::git::run(&dir, &["init", "-q"]).unwrap();
        let ctx = egui::Context::default();
        let mut app = super::App::new(&eframe::CreationContext::_new_kittest(ctx.clone()), Some(dir.clone()));
        let mut frame = eframe::Frame::_new_kittest();
        let mut step = |app: &mut super::App, close: bool| {
            let mut raw = RawInput::default();
            if close {
                raw.viewports.entry(ViewportId::ROOT).or_default().events.push(ViewportEvent::Close);
            }
            let mut out = ctx.run_ui(raw, |ui| app.pass(ui, &mut frame));
            out.textures_delta.clear();
            out.viewport_output.get(&ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default()
        };
        let wait = |what: &str, f: &dyn Fn() -> bool| {
            let t = std::time::Instant::now();
            while !f() {
                assert!(t.elapsed().as_secs() < 120, "timed out waiting for {what}");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        app.tabs[0].term.add(&ctx, &dir);
        let s = &app.tabs[0].term.tabs[0];
        // Not just busy: login scripts and prompt helpers run in the foreground too.
        s.write(&b"echo sta''rted; sleep 30\r"[..]);
        wait("busy", &|| screen(s).contains("started") && s.busy());
        assert!(step(&mut app, true).contains(&ViewportCommand::CancelClose));
        assert!(!app.quit_pending && app.quit_ask == Some(1));
        app.tabs[0].term.tabs[0].write(&b"\x03"[..]);
        wait("idle", &|| !app.tabs[0].term.busy());
        assert!(!step(&mut app, false).contains(&ViewportCommand::Close));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    fn screen(s: &crate::repo::terminal::Session) -> String {
        s.term.lock().renderable_content().display_iter.map(|i| i.cell.c).collect()
    }

    /// egui takes focus from widgets under a modal; typing must reach the shell again after it.
    #[cfg(unix)]
    #[test]
    fn the_terminal_gets_its_focus_back_after_a_modal() {
        let dir = std::env::temp_dir().join(format!("glasir-terminal-focus-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::git::run(&dir, &["init", "-q"]).unwrap();
        let ctx = egui::Context::default();
        let mut app = super::App::new(&eframe::CreationContext::_new_kittest(ctx.clone()), Some(dir.clone()));
        let mut frame = eframe::Frame::_new_kittest();
        let mut step = |app: &mut super::App| {
            let raw = RawInput { screen_rect: Some(egui::Rect::from_min_size(Default::default(), egui::vec2(1200.0, 800.0))), ..Default::default() };
            ctx.run_ui(raw, |ui| app.pass(ui, &mut frame)).textures_delta.clear();
        };
        let t = std::time::Instant::now();
        while app.tabs[0].data.is_none() || app.tabs[0].loading() {
            assert!(t.elapsed().as_secs() < 120, "timed out loading the repo");
            step(&mut app);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        app.tabs[0].apply(&ctx, crate::repo::Action::ToggleTerminal);
        let focused = || ctx.memory(|m| m.has_focus(super::super::terminal::id()));
        for _ in 0..3 {
            step(&mut app);
        }
        assert!(focused());
        app.shortcuts = true;
        for _ in 0..3 {
            step(&mut app);
        }
        app.shortcuts = false;
        for _ in 0..3 {
            step(&mut app);
        }
        assert!(focused());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closing_a_tab_waits_for_its_op() {
        use crate::repo::{Action, Op};
        let dir = std::env::temp_dir().join(format!("glasir-close-tab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::git::run(&dir, &["init", "-q"]).unwrap();
        let ctx = egui::Context::default();
        let mut app = super::App::new(&eframe::CreationContext::_new_kittest(ctx.clone()), Some(dir.clone()));
        app.tabs[0].apply(&ctx, Action::Git(Op::new("Slow", &["-c", "alias.slow=!sleep 1", "slow"])));
        assert!(app.tabs[0].busy().is_some());
        app.close_tab(&ctx, 0);
        assert_eq!(app.tabs.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_typed_commit_message_survives_quitting() {
        let dir = std::env::temp_dir().join(format!("glasir-quit-draft-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::git::run(&dir, &["init", "-q"]).unwrap();
        let ctx = egui::Context::default();
        let mut app = super::App::new(&eframe::CreationContext::_new_kittest(ctx.clone()), Some(dir.clone()));
        (app.tabs[0].summary, app.tabs[0].body) = ("fix it".into(), "why\n\nmore".into());
        assert!(!close(&ctx, &mut app).contains(&ViewportCommand::CancelClose));
        app.persist_drafts();
        let app = super::App::new(&eframe::CreationContext::_new_kittest(ctx.clone()), Some(dir.clone()));
        assert_eq!((app.tabs[0].summary.as_str(), app.tabs[0].body.as_str()), ("fix it", "why\n\nmore"));
        let mut app = app;
        (app.tabs[0].summary, app.tabs[0].body) = (String::new(), String::new());
        app.persist_drafts();
        assert!(!dir.join(".git").join(super::DRAFT_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_committed_message_in_a_closed_tab_does_not_come_back() {
        let dir = std::env::temp_dir().join(format!("glasir-closed-draft-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::git::run(&dir, &["init", "-q"]).unwrap();
        let ctx = egui::Context::default();
        let mut app = super::App::new(&eframe::CreationContext::_new_kittest(ctx.clone()), Some(dir.clone()));
        app.tabs[0].summary = "fix it".into();
        app.persist_drafts();
        app.tabs[0].summary.clear();
        app.close_tab(&ctx, 0);
        app.persist_drafts();
        assert!(!dir.join(".git").join(super::DRAFT_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_saved_draft_drops_the_template_comments() {
        let t = crate::repo::message::Template { prefix: "#".into(), body: "# explain".into(), summary: "feat: ".into() };
        assert_eq!(super::saved_text(&("feat: add".into(), "# explain".into(), Some(t), vec![])), "feat: add");
    }

    fn close(ctx: &egui::Context, app: &mut super::App) -> Vec<ViewportCommand> {
        let mut raw = RawInput::default();
        raw.viewports.entry(ViewportId::ROOT).or_default().events.push(ViewportEvent::Close);
        let mut out = ctx.run_ui(raw, |ui| app.pass(ui, &mut eframe::Frame::_new_kittest()));
        out.textures_delta.clear();
        out.viewport_output.get(&ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default()
    }

    #[cfg(unix)]
    #[test]
    fn quitting_with_unsaved_edits_asks_only_for_them() {
        use crate::repo::{Action, Op};
        let dir = std::env::temp_dir().join(format!("glasir-quit-unsaved-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::git::run(&dir, &["init", "-q"]).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let ctx = egui::Context::default();
        let mut app = super::App::new(&eframe::CreationContext::_new_kittest(ctx.clone()), Some(dir.clone()));
        let mut ed = crate::repo::file_edit::FileEdit::load(&dir, "a.txt").unwrap();
        ed.text.push_str("two\n");
        app.tabs[0].edit = Some(ed);
        app.tabs[0].term.add(&ctx, &dir);
        let s = &app.tabs[0].term.tabs[0];
        s.write(&b"echo sta''rted; sleep 30\r"[..]);
        let t = std::time::Instant::now();
        while !(screen(s).contains("started") && s.busy()) {
            assert!(t.elapsed().as_secs() < 120, "timed out waiting for the shell");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let n = app.toasts.len();
        assert!(close(&ctx, &mut app).contains(&ViewportCommand::CancelClose));
        assert_eq!((app.quit_ask, app.toasts.len()), (None, n + 1), "just the toast: Quit anyway would end the shells and still not quit");
        app.tabs[0].apply(&ctx, Action::Git(Op::new("Slow", &["-c", "alias.slow=!sleep 1", "slow"])));
        assert!(app.tabs[0].busy().is_some());
        assert!(close(&ctx, &mut app).contains(&ViewportCommand::CancelClose));
        assert_eq!((app.quit_pending, app.toasts.len()), (false, n + 2), "no 'Quitting once Slow finishes' beside it");
        app.tabs[0].term.tabs[0].write(&b"\x03"[..]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closing_or_quitting_keeps_work_only_the_tab_holds() {
        use crate::repo::{Task, compose, merge_editor, rebase};
        let dir = std::env::temp_dir().join(format!("glasir-held-work-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| crate::git::run(&dir, args).unwrap();
        git(&["init", "-q"]);
        for m in ["one", "two"] {
            std::fs::write(dir.join("f"), m).unwrap();
            git(&["add", "f"]);
            git(&["-c", "user.name=T", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "-m", m]);
        }
        std::fs::write(dir.join("f"), "three").unwrap();
        let ctx = egui::Context::default();
        let mut app = super::App::new(&eframe::CreationContext::_new_kittest(ctx.clone()), Some(dir.clone()));
        let mut m = merge_editor::Editor::open(&ctx, &dir, "f");
        crate::repo::testutil::wait_for("the merge editor", || {
            if let Some(d) = Task::poll(&mut m.task) {
                (m.output, m.data) = (merge_editor::decode(&d.original), Some(d));
            }
            m.data.is_some()
        });
        let t = &mut app.tabs[0];
        (t.merge, t.rebase) = (Some(m), Some(rebase::Editor::load(&dir, "HEAD~1", "one").unwrap()));
        t.compose = Some(compose::Plan::load(&dir, None).unwrap().0);
        assert_eq!(t.unsaved(), None, "a file and a plan as loaded, Claude's commits not in yet: nothing to lose");
        t.merge.as_mut().unwrap().output.push('x');
        assert!(t.unsaved().is_some_and(|w| w.contains("resolution of f")));
        t.merge = None;
        t.rebase.as_mut().unwrap().items[0].act = rebase::Act::Drop;
        assert!(t.unsaved().is_some_and(|w| w.contains("rebase plan")));
        t.rebase = None;
        t.compose.as_mut().unwrap().commits.push(compose::Commit { message: "x".into(), units: vec![0] });
        assert!(t.unsaved().is_some_and(|w| w.contains("Commit Composer")));
        let n = app.toasts.len();
        app.close_tab(&ctx, 0);
        assert_eq!((app.tabs.len(), app.toasts.len()), (1, n + 1), "the tab stays, saying why");
        assert!(close(&ctx, &mut app).contains(&ViewportCommand::CancelClose), "quitting waits too");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod shell_tests {
    use super::{App, draft, replaceable};
    use crate::repo::{Action, RepoTab};
    use egui::{Event, Key, Modifiers, RawInput};
    use std::path::{Path, PathBuf};

    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("glasir-shell-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::git::run(&dir, &["init", "-q"]).unwrap();
        dir
    }

    fn app(ctx: &egui::Context, first: &Path) -> App {
        App::new(&eframe::CreationContext::_new_kittest(ctx.clone()), Some(first.to_path_buf()))
    }

    fn frame(ctx: &egui::Context, modifiers: Modifiers, keys: &[Key], f: impl FnMut(&mut egui::Ui)) {
        let mut events = vec![Event::ModifiersChanged(modifiers)];
        events.extend(keys.iter().map(|&key| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }));
        let raw = RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))), events, ..Default::default() };
        ctx.run_ui(raw, f).textures_delta.clear();
    }

    fn step(ctx: &egui::Context, app: &mut App, modifiers: Modifiers, keys: &[Key]) {
        frame(ctx, modifiers, keys, |ui| {
            app.shortcuts(ui.ctx());
            if app.shortcuts {
                app.shortcuts = super::super::shortcuts::show(ui.ctx());
            }
        });
    }

    #[test]
    fn a_failed_op_in_a_background_tab_is_toasted() {
        let (a, b) = (repo("bgfail-a"), repo("bgfail-b"));
        for kv in [["gpg.program", "/nonexistent-gpg"], ["commit.gpgsign", "true"], ["user.email", "t@t"], ["user.name", "T"]] {
            crate::git::run(&a, &["config", kv[0], kv[1]]).unwrap();
        }
        let ctx = egui::Context::default();
        let mut app = app(&ctx, &a);
        app.open(&ctx, &b);
        assert_eq!(app.active, 1);
        let wait = |app: &mut App, done: &dyn Fn(&RepoTab) -> bool| {
            for _ in 0..2000 {
                app.poll_tabs(&ctx);
                if done(&app.tabs[0]) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("timed out");
        };
        wait(&mut app, &|t| t.data.is_some());
        app.tabs[0].apply(&ctx, Action::Git(crate::repo::Op::new("Commit", &["commit", "--allow-empty", "-m", "x"])));
        wait(&mut app, &|t| t.choice.is_some());
        assert!(app.toasts.iter().any(|t| t.text.contains("Signing failed") && t.text.contains("open the tab")), "{:?}", app.toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
        for d in [a, b] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn a_dialog_blocks_the_tab_and_repo_shortcuts() {
        let (a, b) = (repo("modal-a"), repo("modal-b"));
        let ctx = egui::Context::default();
        let mut app = app(&ctx, &a);
        app.open(&ctx, &b);
        assert_eq!((app.tabs.len(), app.active), (2, 1));
        app.shortcuts = true;
        step(&ctx, &mut app, Modifiers::NONE, &[]);
        step(&ctx, &mut app, Modifiers::NONE, &[]);
        step(&ctx, &mut app, Modifiers::COMMAND, &[Key::Num1]);
        step(&ctx, &mut app, Modifiers::COMMAND, &[Key::W]);
        step(&ctx, &mut app, Modifiers::CTRL, &[Key::Tab]);
        step(&ctx, &mut app, Modifiers::COMMAND, &[Key::Comma]);
        assert_eq!((app.tabs.len(), app.active), (2, 1), "nothing behind the shortcut sheet reacts");
        assert!(app.settings.is_none(), "Settings don't open over it either");
        app.shortcuts = false;
        step(&ctx, &mut app, Modifiers::NONE, &[]);
        step(&ctx, &mut app, Modifiers::COMMAND, &[Key::Num1]);
        assert_eq!(app.active, 0);
        step(&ctx, &mut app, Modifiers::COMMAND, &[Key::W]);
        assert_eq!(app.tabs.len(), 1);
        for d in [a, b] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn ctrl_tab_cycles_from_the_welcome_page_too() {
        let dirs: Vec<_> = (0..3).map(|i| repo(&format!("tab-{i}"))).collect();
        let ctx = egui::Context::default();
        let mut app = app(&ctx, &dirs[0]);
        app.open(&ctx, &dirs[1]);
        app.open(&ctx, &dirs[2]);
        let mut press = |from: usize, shift: bool| {
            app.active = from;
            step(&ctx, &mut app, Modifiers::CTRL | if shift { Modifiers::SHIFT } else { Modifiers::NONE }, &[Key::Tab]);
            app.active
        };
        assert_eq!([press(3, false), press(0, false), press(1, false), press(2, false)], [0, 1, 2, 0]);
        assert_eq!([press(3, true), press(0, true), press(2, true), press(1, true)], [2, 2, 1, 0]);
        for d in dirs {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn escape_in_a_dirty_editor_asks_before_leaving() {
        let dir = repo("esc-edit");
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let mut ed = crate::repo::file_edit::FileEdit::load(&dir, "a.txt").unwrap();
        ed.text.push_str("two\n");
        let ctx = egui::Context::default();
        let mut acts = vec![];
        let mut wrap = false;
        let mut esc = |ed: &mut crate::repo::file_edit::FileEdit, keys: &[Key]| {
            frame(&ctx, Modifiers::NONE, keys, |ui| super::super::file_edit::show(ui, ed, &dir, &mut wrap, &mut acts));
            ed.ask.is_some()
        };
        assert!(!esc(&mut ed, &[]));
        assert!(esc(&mut ed, &[Key::Escape]), "Esc opens the dialog, and the same press does not close it again");
        assert!(esc(&mut ed, &[]));
        assert!(!esc(&mut ed, &[Key::Escape]), "the next Esc is Keep editing");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn escape_in_the_terminal_stays_in_the_shell() {
        let dir = repo("term-esc");
        let ctx = egui::Context::default();
        let mut app = app(&ctx, &dir);
        let mut frame = eframe::Frame::_new_kittest();
        let mut step = |app: &mut App, keys: &[Key]| {
            let events = keys.iter().map(|&key| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }).collect();
            let raw = RawInput { screen_rect: Some(egui::Rect::from_min_size(Default::default(), egui::vec2(1200.0, 800.0))), events, ..Default::default() };
            ctx.run_ui(raw, |ui| app.pass(ui, &mut frame)).textures_delta.clear();
        };
        let t = std::time::Instant::now();
        while app.tabs[0].data.is_none() || app.tabs[0].loading() {
            assert!(t.elapsed().as_secs() < 120, "timed out loading the repo");
            step(&mut app, &[]);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        app.tabs[0].apply(&ctx, Action::ToggleTerminal);
        app.tabs[0].apply(&ctx, Action::Reflog);
        for _ in 0..3 {
            step(&mut app, &[]);
        }
        let focused = || ctx.memory(|m| m.has_focus(super::super::terminal::id()));
        assert!(focused());
        step(&mut app, &[Key::Escape]);
        assert!(focused() && app.tabs[0].reflog.is_some(), "Esc typed in the terminal does not close the view behind it");
        ctx.memory_mut(|m| m.surrender_focus(super::super::terminal::id()));
        step(&mut app, &[Key::Escape]);
        assert!(app.tabs[0].reflog.is_none(), "with the keyboard elsewhere it does");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn escape_cancels_a_message_edit_when_nothing_has_the_keyboard() {
        let dir = repo("esc-msg");
        crate::git::run(&dir, &["-c", "user.name=T", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "--allow-empty", "-q", "-m", "one"]).unwrap();
        let ctx = egui::Context::default();
        let mut app = app(&ctx, &dir);
        let mut frame = eframe::Frame::_new_kittest();
        let mut step = |app: &mut App, keys: &[Key]| {
            let events = keys.iter().map(|&key| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }).collect();
            let raw = RawInput { screen_rect: Some(egui::Rect::from_min_size(Default::default(), egui::vec2(1200.0, 800.0))), events, ..Default::default() };
            ctx.run_ui(raw, |ui| app.pass(ui, &mut frame)).textures_delta.clear();
        };
        crate::ui::testkit::load(&mut app.tabs[0], &ctx);
        let head = crate::git::run(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_owned();
        app.tabs[0].apply(&ctx, Action::Select(crate::repo::Sel::Commit(head)));
        let t = std::time::Instant::now();
        let id = loop {
            if let Some(Ok(info)) = &app.tabs[0].info {
                break info.id.clone();
            }
            assert!(t.elapsed().as_secs() < 120, "timed out loading the commit");
            step(&mut app, &[]);
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        app.tabs[0].edit_msg = Some(crate::repo::MsgEdit::new(&id, "one", (None, vec![])));
        for _ in 0..3 {
            step(&mut app, &[]);
        }
        assert!(ctx.memory(|m| m.focused().is_some()), "the editor takes the keyboard");
        ctx.memory_mut(|m| m.stop_text_input());
        step(&mut app, &[]);
        step(&mut app, &[]);
        app.shortcuts = true;
        for _ in 0..3 {
            step(&mut app, &[]);
        }
        step(&mut app, &[Key::Escape]);
        assert!(!app.shortcuts && app.tabs[0].edit_msg.is_some(), "the dialog takes the Esc, the edit stays");
        step(&mut app, &[]);
        step(&mut app, &[]);
        step(&mut app, &[Key::Escape]);
        assert!(app.tabs[0].edit_msg.is_none(), "with the keyboard free, Esc cancels the edit");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn only_the_top_dialog_takes_escape<S>(sim: &mut crate::ui::testkit::Sim, state: &mut S, mut draw: impl FnMut(&mut S, &egui::Context), is_open: impl Fn(&S) -> bool) {
        let mut frame = |sim: &mut crate::ui::testkit::Sim, state: &mut S, upper: bool, esc: bool| {
            let events = if esc { vec![Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }] } else { vec![] };
            let mut took = false;
            sim.frame(events, |ui| {
                draw(state, ui.ctx());
                if upper {
                    egui::Modal::new(egui::Id::new("upper")).show(ui.ctx(), |ui| took |= super::super::format::dismissed(ui, "Cancel"));
                }
                super::super::format::note_focus(ui.ctx());
            });
            took
        };
        frame(sim, state, false, false);
        frame(sim, state, true, false);
        frame(sim, state, true, false);
        assert!(frame(sim, state, true, true), "the dialog on top takes Esc");
        assert!(is_open(state), "the one below it stays");
        frame(sim, state, false, false);
        frame(sim, state, false, false);
        frame(sim, state, false, true);
        assert!(!is_open(state), "alone, it takes Esc itself");
    }

    #[test]
    fn escape_closes_only_the_dialog_on_top() {
        use super::super::palette::{Cmd, Item, Palette};
        use crate::ui::testkit::{Sim, tab};
        let dir = repo("esc-top");
        let mut sim = Sim::new(1200.0, 800.0);
        let mut a = app(&sim.ctx, &dir);
        a.quit_ask = Some(1);
        only_the_top_dialog_takes_escape(&mut sim, &mut a, |a, ctx| a.quit_dialog(ctx), |a| a.quit_ask.is_some());
        let mut sim = Sim::new(1200.0, 800.0);
        a.shortcuts = true;
        only_the_top_dialog_takes_escape(&mut sim, &mut a, |a, ctx| a.shortcuts = a.shortcuts && super::super::shortcuts::show(ctx), |a| a.shortcuts);
        let mut sim = Sim::new(1200.0, 800.0);
        a.palette = Some(Palette { items: vec![Item { icon: "", label: "Go".into(), detail: String::new(), cmd: Cmd::PickOpen }], ..Default::default() });
        only_the_top_dialog_takes_escape(&mut sim, &mut a, |a, ctx| a.command_palette(ctx), |a| a.palette.is_some());
        let mut sim = Sim::new(1200.0, 800.0);
        let mut t = tab(&sim.ctx, "esc-top-form", &["one"]);
        t.remote_form = Some(crate::repo::remotes::Form::add(t.data.as_ref().unwrap()));
        only_the_top_dialog_takes_escape(&mut sim, &mut t, |t, ctx| super::super::sidebar::remotes::form_dialog(t, ctx, &mut vec![]), |t| t.remote_form.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn one_escape_closes_a_prompt_whose_field_is_focused_and_two_leave_a_composer_message() {
        use crate::ui::testkit::{Sim, tab};
        let esc = || vec![Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }];
        let dir = repo("esc-field-dialogs");
        commit(&dir, "one");
        let mut a = app(&egui::Context::default(), &dir);
        a.rename = Some((dir.clone(), "renamed".into()));
        let mut sim = Sim::new(1200.0, 800.0);
        for _ in 0..4 {
            sim.frame(vec![], |ui| {
                a.rename_dialog(ui.ctx());
                super::super::format::note_focus(ui.ctx());
            });
        }
        sim.frame(esc(), |ui| a.rename_dialog(ui.ctx()));
        assert!(a.rename.is_none(), "Esc closes Rename tab");
        let mut t = tab(&sim.ctx, "esc-field-prompt", &["one"]);
        t.prompt = Some(crate::repo::Prompt { title: "Name".into(), value: "x".into(), op: crate::repo::Op::new("Create branch", &["branch", "{}"]), second: None, sign: None });
        for _ in 0..4 {
            sim.frame(vec![], |ui| {
                super::super::dialogs::prompt_dialog(&mut t, ui.ctx(), &mut vec![]);
                super::super::format::note_focus(ui.ctx());
            });
        }
        sim.frame(esc(), |ui| super::super::dialogs::prompt_dialog(&mut t, ui.ctx(), &mut vec![]));
        assert!(t.prompt.is_none(), "Esc closes a simple prompt");
        t.remote_form = Some(crate::repo::remotes::Form::add(t.data.as_ref().unwrap()));
        for _ in 0..4 {
            sim.frame(vec![], |ui| {
                super::super::sidebar::remotes::form_dialog(&mut t, ui.ctx(), &mut vec![]);
                super::super::format::note_focus(ui.ctx());
            });
        }
        sim.frame(esc(), |ui| super::super::sidebar::remotes::form_dialog(&mut t, ui.ctx(), &mut vec![]));
        assert!(t.remote_form.is_none(), "Esc closes Add remote");
        std::fs::write(dir.join("f"), "changed\n").unwrap();
        let mut t = tab(&sim.ctx, "esc-field-compose", &["one"]);
        t.compose = Some(crate::repo::compose::Plan::load(&dir, None).unwrap().0);
        t.compose.as_mut().unwrap().commits.push(crate::repo::compose::Commit { message: "composer message".into(), units: vec![0] });
        let mut draw = |sim: &mut Sim, events: Vec<Event>| {
            sim.frame(events, |ui| {
                super::super::compose::compose_dialog(&mut t, ui.ctx(), &mut vec![]);
                super::super::format::note_focus(ui.ctx());
            })
        };
        draw(&mut sim, vec![]);
        draw(&mut sim, vec![]);
        let at = sim.text(|s| s.contains("composer message")).expect("message drawn");
        sim.click(at, egui::PointerButton::Primary, |ui| {
            super::super::compose::compose_dialog(&mut t, ui.ctx(), &mut vec![]);
            super::super::format::note_focus(ui.ctx());
        });
        assert!(sim.ctx.memory(|m| m.focused().is_some()), "the message is focused");
        sim.frame(esc(), |ui| {
            super::super::compose::compose_dialog(&mut t, ui.ctx(), &mut vec![]);
            super::super::format::note_focus(ui.ctx());
        });
        assert!(t.compose.is_some(), "the first Esc leaves the field");
        sim.frame(vec![], |ui| {
            super::super::compose::compose_dialog(&mut t, ui.ctx(), &mut vec![]);
            super::super::format::note_focus(ui.ctx());
        });
        sim.frame(esc(), |ui| super::super::compose::compose_dialog(&mut t, ui.ctx(), &mut vec![]));
        assert!(t.compose.is_none(), "the second closes");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn f7_waits_for_a_dialog() {
        use crate::repo::DiffSrc;
        use crate::ui::testkit::{Sim, load};
        let dir = repo("f7-keys");
        let lines = |changed: &[usize]| (1..=120).map(|n| if changed.contains(&n) { format!("changed {n}\n") } else { format!("line {n}\n") }).collect::<String>();
        std::fs::write(dir.join("big.txt"), lines(&[])).unwrap();
        crate::git::run(&dir, &["add", "big.txt"]).unwrap();
        crate::git::run(&dir, &["-c", "user.name=T", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "base"]).unwrap();
        std::fs::write(dir.join("big.txt"), lines(&[10, 40, 70, 100])).unwrap();
        let mut sim = Sim::new(900.0, 420.0);
        let mut t = RepoTab::new(&sim.ctx, dir.clone(), 100, Default::default());
        load(&mut t, &sim.ctx);
        t.apply(&sim.ctx, Action::Diff(DiffSrc::Worktree { change: crate::git::Change { path: "big.txt".into(), old_path: None, code: 'M' }, staged: false }));
        let (start, mut msgs) = (std::time::Instant::now(), vec![]);
        while t.diff.as_ref().is_none_or(|d| d.file.is_none() || d.loading()) {
            assert!(start.elapsed().as_secs() < 120, "timed out loading the diff");
            t.poll(&sim.ctx, &mut msgs);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let (mut split, mut wrap) = (false, false);
        let mut frame = |sim: &mut Sim, f7: bool, dialog: bool| {
            let events = if f7 { vec![Event::Key { key: Key::F7, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }] } else { vec![] };
            sim.frame(events, |ui| {
                super::super::diff_view::show(ui, t.diff.as_mut().unwrap(), t.data.as_ref(), &mut split, &mut wrap, &mut vec![]);
                if dialog {
                    egui::Modal::new(egui::Id::new("dialog")).show(ui.ctx(), |ui| ui.label("Confirm"));
                }
            });
            sim.text(|s| s.contains("changed 100")).is_some()
        };
        for _ in 0..3 {
            assert!(!frame(&mut sim, false, true), "the last change starts below the view");
        }
        for _ in 0..8 {
            frame(&mut sim, true, true);
            assert!(!frame(&mut sim, false, true), "F7 belongs to the dialog");
        }
        frame(&mut sim, false, false);
        frame(&mut sim, false, false);
        let seen = (0..8).any(|_| {
            frame(&mut sim, true, false);
            frame(&mut sim, false, false)
        });
        assert!(seen, "F7 walks to the last change once the dialog is gone");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn history_back_and_forward_wait_for_a_dialog() {
        use crate::repo::history::{FileView, Mode};
        use crate::ui::testkit::{Sim, tab};
        let mut sim = Sim::new(900.0, 500.0);
        let t = tab(&sim.ctx, "hist-keys", &["one"]);
        let mut fv = FileView::open(&sim.ctx, &t.dir, "f", Mode::History);
        fv.go(&sim.ctx, "f".into(), Mode::Blame(None));
        let mut nav = |sim: &mut Sim, key: Option<Key>, dialog: bool| {
            let mods = if key.is_some() { Modifiers::COMMAND } else { Modifiers::NONE };
            let events = [Event::ModifiersChanged(mods)].into_iter().chain(key.map(|key| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: mods })).collect();
            let (mut acts, mut wrap) = (vec![], false);
            sim.frame(events, |ui| {
                super::super::history::show(ui, &mut fv, &mut wrap, &mut acts);
                if dialog {
                    egui::Modal::new(egui::Id::new("dialog")).show(ui.ctx(), |ui| ui.label("Confirm"));
                }
            });
            acts.iter().any(|a| matches!(a, Action::CloseDiff))
        };
        nav(&mut sim, None, true);
        nav(&mut sim, None, true);
        assert!(!nav(&mut sim, Some(Key::OpenBracket), true) && !nav(&mut sim, Some(Key::CloseBracket), true), "⌘[ and ⌘] belong to the dialog");
        nav(&mut sim, None, false);
        nav(&mut sim, None, false);
        assert!(nav(&mut sim, Some(Key::OpenBracket), false) && nav(&mut sim, Some(Key::CloseBracket), false));
    }

    #[test]
    fn the_palette_runs_its_command_on_the_tab_it_was_built_for() {
        use super::super::palette::{Cmd, Item, Palette};
        let (a, b) = (repo("pal-a"), repo("pal-b"));
        let ctx = egui::Context::default();
        let mut app = app(&ctx, &a);
        app.open(&ctx, &b);
        let go = |dir: PathBuf| Palette {
            items: vec![Item { icon: "", label: "Go".into(), detail: String::new(), cmd: Cmd::Tab(Action::OpenTab("x".into())) }],
            dir: Some(dir),
            ..Default::default()
        };
        app.palette = Some(go(app.tabs[0].dir.clone()));
        assert_eq!(app.active, 1, "the active tab moved on while the palette was open");
        frame(&ctx, Modifiers::NONE, &[], |ui| app.command_palette(ui.ctx()));
        frame(&ctx, Modifiers::NONE, &[Key::Enter], |ui| app.command_palette(ui.ctx()));
        assert!(app.palette.is_none());
        assert!(app.tabs[0].open_tab.is_some() && app.tabs[1].open_tab.is_none());
        app.tabs[0].open_tab = None;
        app.palette = Some(go(PathBuf::from("/closed/meanwhile")));
        frame(&ctx, Modifiers::NONE, &[Key::Enter], |ui| app.command_palette(ui.ctx()));
        assert!(app.palette.is_none() && app.tabs.iter().all(|t| t.open_tab.is_none()), "a closed tab's command is dropped");
        for d in [a, b] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn switching_worktrees_keeps_what_would_be_lost() {
        use crate::repo::message::Template;
        let (a, b, c) = (repo("sw-a"), repo("sw-b"), repo("sw-c"));
        let ctx = egui::Context::default();
        let mut app = app(&ctx, &a);
        app.switch(&ctx, 0, &b);
        assert_eq!(app.tabs.len(), 1, "a free tab is replaced in place");
        assert!(app.tabs[0].dir.ends_with(b.file_name().unwrap()));
        app.tabs[0].amend = true;
        app.switch(&ctx, 0, &a);
        assert_eq!((app.tabs.len(), app.active), (2, 1), "an amend belongs to the old HEAD: a new tab");
        let mut t = RepoTab::new(&ctx, c.clone(), 1000, Default::default());
        assert!(replaceable(&t) && draft(&t).is_none());
        t.template = Some(Template { prefix: "#".into(), summary: "feat:".into(), body: "# hint".into() });
        (t.summary, t.body) = ("feat:".into(), "# hint".into());
        assert!(replaceable(&t) && draft(&t).is_none(), "a template nobody edited is no draft");
        t.summary.push_str(" add");
        assert!(replaceable(&t) && draft(&t).is_some(), "a typed message travels with the switch");
        t.amend = true;
        assert!(!replaceable(&t), "an amend's text belongs to the old HEAD");
        t.amend = false;
        (t.summary, t.body) = Default::default();
        t.ai = Some(crate::ai::start(&ctx, Default::default(), c.clone(), crate::ai::Purpose::CommitMessage { amend: false }, String::new()));
        assert!(!replaceable(&t), "a commit message Claude is still writing replaces the one typed");
        t.ai = None;
        for m in ["one", "two"] {
            crate::git::run(&c, &["-c", "user.name=T", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "--allow-empty", "-q", "-m", m]).unwrap();
        }
        t.rebase = Some(crate::repo::rebase::Editor::load(&c, "HEAD~1", "one").unwrap());
        assert!(!replaceable(&t), "an edited rebase plan");
        for d in [a, b, c] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn a_message_draft_comes_back_with_its_worktree() {
        use crate::ui::testkit::load;
        let (a, b) = (repo("draft-a"), repo("draft-b"));
        std::fs::write(a.join(".git/tpl"), "feat: \n\n# explain\n").unwrap();
        crate::git::run(&a, &["config", "commit.template", ".git/tpl"]).unwrap();
        let ctx = egui::Context::default();
        let mut app = app(&ctx, &a);
        load(&mut app.tabs[0], &ctx);
        assert_eq!((app.tabs[0].summary.as_str(), app.tabs[0].body.as_str()), ("feat:", "# explain"));
        app.switch(&ctx, 0, &b);
        app.switch(&ctx, 0, &a);
        assert!(app.tabs[0].summary.is_empty(), "an untouched template is no draft to keep");
        load(&mut app.tabs[0], &ctx);
        app.tabs[0].summary = "feat: add".into();
        app.tabs[0].coauthors = vec![("Ann".into(), "ann@x.io".into())];
        app.switch(&ctx, 0, &b);
        assert_eq!((app.tabs.len(), app.active), (1, 0), "the tab is replaced in place");
        assert!(app.tabs[0].dir.ends_with(b.file_name().unwrap()) && app.tabs[0].summary.is_empty());
        app.switch(&ctx, 0, &a);
        assert_eq!((app.tabs.len(), app.active), (1, 0));
        assert_eq!(app.tabs[0].summary, "feat: add");
        load(&mut app.tabs[0], &ctx);
        let t = &app.tabs[0];
        assert_eq!((t.summary.as_str(), t.body.as_str(), t.coauthors.len()), ("feat: add", "# explain", 1), "loading leaves the draft alone");
        assert_eq!(String::from_utf8(t.commit_op().input.unwrap()).unwrap(), "feat: add\n\nCo-authored-by: Ann <ann@x.io>", "the template's comment lines are still stripped");
        for d in [a, b] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn restoring_tabs_leaves_the_recent_list_alone() {
        let (a, b) = (repo("rec-a"), repo("rec-b"));
        let ctx = egui::Context::default();
        let mut app = app(&ctx, &a);
        let recent = vec![PathBuf::from("/x"), PathBuf::from("/y")];
        app.s.recent = recent.clone();
        app.add_tab(&ctx, std::fs::canonicalize(&b).unwrap());
        assert_eq!(app.s.recent, recent);
        app.open(&ctx, &a);
        assert!(app.s.recent[0].ends_with(a.file_name().unwrap()) && app.s.recent.len() == 3);
        for d in [a, b] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    fn commit(dir: &Path, m: &str) {
        crate::git::run(dir, &["-c", "user.name=T", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "--allow-empty", "-q", "-m", m]).unwrap();
    }

    fn esc() -> Vec<Event> {
        vec![Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }]
    }

    #[test]
    fn escape_closes_a_menu_and_not_the_view_under_it() {
        use crate::ui::testkit::{Sim, load};
        let dir = repo("esc-menu");
        commit(&dir, "menu target");
        let mut sim = Sim::new(1200.0, 800.0);
        let mut app = app(&sim.ctx, &dir);
        let mut frame = eframe::Frame::_new_kittest();
        load(&mut app.tabs[0], &sim.ctx);
        app.tabs[0].apply(&sim.ctx, Action::Reflog);
        crate::repo::testutil::wait_for("the reflog", || app.tabs[0].reflog.as_mut().is_some_and(|r| {
            r.poll();
            r.entries.is_some()
        }));
        app.tabs[0].apply(&sim.ctx, Action::Select(crate::repo::Sel::Wip));
        let quiet = std::time::Instant::now();
        while quiet.elapsed() < std::time::Duration::from_millis(600) || app.tabs[0].loading() {
            sim.frame(vec![], |ui| app.pass(ui, &mut frame));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        sim.open_menu("menu target", |ui| app.pass(ui, &mut frame));
        assert!(egui::Popup::is_any_open(&sim.ctx));
        sim.frame(esc(), |ui| app.pass(ui, &mut frame));
        assert!(!egui::Popup::is_any_open(&sim.ctx) && app.tabs[0].reflog.is_some(), "Esc closes the menu, not the reflog under it");
        app.tabs[0].commit_drop = Some(("Drop".into(), vec![], egui::pos2(20.0, 20.0)));
        sim.frame(vec![], |ui| app.pass(ui, &mut frame));
        sim.frame(esc(), |ui| app.pass(ui, &mut frame));
        assert!(app.tabs[0].commit_drop.is_none() && app.tabs[0].reflog.is_some(), "a drop menu takes Esc the same way");
        sim.frame(esc(), |ui| app.pass(ui, &mut frame));
        assert!(app.tabs[0].reflog.is_none(), "the next Esc is the reflog's");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn escape_that_leaves_a_field_only_leaves_the_field() {
        use crate::ui::testkit::Sim;
        let dir = repo("esc-field");
        commit(&dir, "one");
        commit(&dir, "two");
        let mut ed = crate::repo::rebase::Editor::load(&dir, "HEAD~1", "one").unwrap();
        let (mut sim, other) = (Sim::new(900.0, 600.0), egui::Id::new("other_field"));
        let git_dir = dir.join(".git");
        let frame = |sim: &mut Sim, ed: &mut crate::repo::rebase::Editor, keys: Vec<Event>| {
            let (mut view, mut rebase) = (false, false);
            sim.frame(keys, |ui| {
                ui.add(egui::TextEdit::singleline(&mut String::new()).id(other));
                view = super::super::format::esc_pressed(ui);
                rebase = super::super::rebase_editor::show(ui, ed, &git_dir, &mut vec![]);
                super::super::format::note_focus(ui.ctx());
            });
            (view, rebase)
        };
        frame(&mut sim, &mut ed, vec![]);
        sim.ctx.memory_mut(|m| m.request_focus(other));
        frame(&mut sim, &mut ed, vec![]);
        assert!(sim.ctx.memory(|m| m.has_focus(other)));
        assert_eq!(frame(&mut sim, &mut ed, esc()), (false, false), "Esc leaves the field; the view and the rebase plan stay");
        assert!(sim.ctx.memory(|m| m.focused().is_none()));
        assert_eq!(frame(&mut sim, &mut ed, esc()), (true, true), "the next Esc is theirs");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn only_the_top_dialog_takes_enter<S>(state: &mut S, mut draw: impl FnMut(&mut S, &egui::Context), done: impl Fn(&S) -> bool) {
        let (mut sim, field) = (crate::ui::testkit::Sim::new(1200.0, 800.0), egui::Id::new("upper_field"));
        let mut frame = |sim: &mut crate::ui::testkit::Sim, state: &mut S, upper: bool, enter: bool| {
            let events = if enter { vec![Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }] } else { vec![] };
            sim.frame(events, |ui| {
                draw(state, ui.ctx());
                if upper {
                    egui::Modal::new(egui::Id::new("upper")).show(ui.ctx(), |ui| ui.add(egui::TextEdit::singleline(&mut String::new()).id(field)));
                }
            });
        };
        frame(&mut sim, state, false, false);
        frame(&mut sim, state, false, false);
        frame(&mut sim, state, true, false);
        frame(&mut sim, state, true, false);
        sim.ctx.memory_mut(|m| m.request_focus(field));
        frame(&mut sim, state, true, false);
        assert!(sim.ctx.memory(|m| m.has_focus(field)), "the dialog below leaves the keyboard to the one on top");
        frame(&mut sim, state, true, true);
        assert!(!done(state), "Enter is the top dialog's");
        frame(&mut sim, state, false, false);
        frame(&mut sim, state, false, false);
        frame(&mut sim, state, false, true);
        assert!(done(state), "alone, it takes Enter");
    }

    #[test]
    fn only_the_top_dialog_takes_enter_and_the_keyboard() {
        use super::super::palette::{Cmd, Item, Palette};
        let dir = repo("enter-top");
        let mut a = app(&egui::Context::default(), &dir);
        a.palette = Some(Palette { items: vec![Item { icon: "", label: "Log".into(), detail: String::new(), cmd: Cmd::CommandLog }], ..Default::default() });
        only_the_top_dialog_takes_enter(&mut a, |a, ctx| a.command_palette(ctx), |a| a.show_log);
        a.rename = Some((dir.clone(), "renamed".into()));
        only_the_top_dialog_takes_enter(&mut a, |a, ctx| a.rename_dialog(ctx), |a| a.rename.is_none());
        let mut t = crate::ui::testkit::tab(&egui::Context::default(), "enter-top-prompt", &["one"]);
        t.prompt = Some(crate::repo::Prompt { title: "Name".into(), value: "x".into(), op: crate::repo::Op::new("Create branch", &["branch", "{}"]), second: None, sign: None });
        only_the_top_dialog_takes_enter(&mut t, |t, ctx| super::super::dialogs::prompt_dialog(t, ctx, &mut vec![]), |t| t.prompt.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn undo_steps_come_back_with_their_worktree() {
        use crate::repo::{Op, Undo};
        use crate::ui::testkit::load;
        let (a, b) = (repo("undo-a"), repo("undo-b"));
        commit(&a, "one");
        let ctx = egui::Context::default();
        let mut app = app(&ctx, &a);
        load(&mut app.tabs[0], &ctx);
        app.tabs[0].apply(&ctx, Action::Git(Op::new("Create branch", &["branch", "x"]).undo(Undo::Keep)));
        load(&mut app.tabs[0], &ctx);
        let step = app.tabs[0].undo_top();
        assert!(step.is_some());
        app.switch(&ctx, 0, &b);
        assert_eq!((app.tabs.len(), app.tabs[0].undo_top()), (1, None));
        app.switch(&ctx, 0, &a);
        assert_eq!(app.tabs[0].undo_top(), step, "⌘Z still undoes what was done in that worktree");
        for d in [a, b] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn find_opens_only_over_the_graph() {
        let dir = repo("find-pane");
        let ctx = egui::Context::default();
        let mut app = app(&ctx, &dir);
        app.tabs[0].apply(&ctx, Action::Reflog);
        step(&ctx, &mut app, Modifiers::COMMAND, &[Key::F]);
        assert!(app.tabs[0].search.is_none() && app.tabs[0].reflog.is_some(), "the search bar only draws over the graph");
        app.tabs[0].reflog = None;
        step(&ctx, &mut app, Modifiers::COMMAND, &[Key::F]);
        assert!(app.tabs[0].search.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
