//! The main window: `App` (open tabs, settings, global dialogs), the eframe
//! frame loop, global shortcuts and window setup. Each screen area lives in
//! its own `ui` module; this file only wires them together.

use super::settings::{Settings, SettingsDlg, toggle_flavor};
use super::toasts::Toast;
use super::welcome::CloneDlg;
use super::{command_log, graph_view, palette, repo_view};
use crate::git;
use crate::repo::{Action, Msg, RepoTab, Search, Sel, Task, fetch_op, unstage_all_op};
use crate::theme::{self, icon};
use egui::{Frame, Key, Margin, Modifiers, RichText, Ui};
use std::path::{Path, PathBuf};

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
        let s: Settings = cc.storage.and_then(|st| eframe::get_value(st, "settings")).unwrap_or_default();
        theme::install_fonts(&cc.egui_ctx);
        theme::set_transparency(s.transparency);
        let mut s = s;
        if s.max_commits == 50_000 {
            s.max_commits = crate::repo::PAGE;
        }
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
        let mut app =
            App {
            shot,
            stacks_stale: false,
            commits_stale: false,
            perf: std::env::var_os("GLASIR_PERF").map(|_| (std::time::Instant::now(), vec![])),
            bench: None,
            tabs: vec![],
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
                app.open(&cc.egui_ctx, &p);
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
            Ok(top) => self.add_tab(ctx, PathBuf::from(top.trim())),
            Err(e) => self.toast(ctx, format!("Not a git repository: {}\n{e}", path.display()), true),
        }
    }

    /// Selects the tab of the repository at `top` (its top-level), opening it if needed.
    fn add_tab(&mut self, ctx: &egui::Context, top: PathBuf) {
        self.active = match self.tabs.iter().position(|t| t.dir == top) {
            Some(i) => i,
            None => {
                self.tabs.push(RepoTab::new(ctx, top.clone(), self.s.max_commits, self.s.views.get(&top).cloned().unwrap_or_default()));
                self.tabs.len() - 1
            }
        };
        self.s.recent.retain(|p| *p != top);
        self.s.recent.insert(0, top);
        self.s.recent.truncate(15);
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.memory(|m| m.focused().is_some());
        let (cmd, shift) = ctx.input(|i| (i.modifiers.command, i.modifiers.shift));
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        // Consumed: off macOS a text field would also take Ctrl+K / W / U as readline edits.
        let eat = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        if ctx.input(|i| i.modifiers.ctrl && !i.modifiers.mac_cmd && i.key_pressed(Key::Backtick))
            && let Some(t) = self.tabs.get_mut(self.active)
        {
            t.apply(ctx, if self.s.terminal || !t.term.tabs.is_empty() { Action::ToggleTerminal } else { Action::Terminal });
        }
        // Off macOS ⌘ is Ctrl, which the shell needs (^R, ^L, ^W, ^Z...).
        if !cfg!(target_os = "macos") && ctx.memory(|m| m.has_focus(super::terminal::id())) {
            return;
        }
        if cmd && pressed(Key::O) {
            self.pick_and_open(ctx);
        }
        if cmd && pressed(Key::Comma) {
            self.open_settings();
        }
        if (cmd && pressed(Key::Slash)) || (!typing && pressed(Key::Questionmark)) {
            self.shortcuts = !self.shortcuts;
        }
        if cmd && shift && pressed(Key::L) {
            self.show_log = !self.show_log;
        }
        let fetch = cmd && !shift && pressed(Key::L);
        if eat(Modifiers::COMMAND, Key::P) | eat(Modifiers::COMMAND, Key::K) {
            self.palette = Some(Default::default());
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
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(Key::Tab)) && !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        let mut act = None;
        if pressed(Key::F5) || (cmd && pressed(Key::R)) {
            act = Some(Action::Reload);
        }
        if fetch {
            act = Some(Action::Git(fetch_op()));
        }
        if !typing && cmd && pressed(Key::Z) {
            act = Some(if shift { Action::Redo } else { Action::Undo });
        }
        if !typing && cmd && pressed(Key::Y) {
            act = Some(Action::Redo);
        }
        if cmd && pressed(Key::F) && t.edit.is_none() {
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
        let plain = ctx.input(|i| !i.modifiers.any()) && ctx.memory(|m| m.top_modal_layer().is_none());
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
        if let Some(p) = self.tabs.get(i).and_then(RepoTab::unsaved) {
            return self.toast(ctx, format!("Save or discard your edits to {p} first."), true);
        }
        if let Some(b) = self.tabs.get(i).and_then(RepoTab::busy).map(str::to_owned) {
            return self.toast(ctx, format!("Close this tab once '{b}' finishes (or cancel it)."), true);
        }
        if self.tabs.get(i).is_some_and(|t| t.term.busy()) {
            return self.toast(ctx, "A command is still running in this tab's terminal: let it finish or stop it there, then close the tab.".into(), true);
        }
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

    fn quit_dialog(&mut self, ctx: &egui::Context) {
        let Some(n) = self.quit_ask else { return };
        let mut done = None;
        egui::Modal::new(egui::Id::new("quit_terminal")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("Quit Glasir?");
            ui.label(if n == 1 { "A command is still running in the terminal. Quitting stops it.".into() } else { format!("{n} commands are still running in the terminal. Quitting stops them.") });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("Quit anyway").strong()).fill(theme::DEL.gamma_multiply(0.6))).clicked() {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        if let Some(quit) = done {
            self.quit_ask = None;
            if quit {
                for t in &mut self.tabs {
                    t.term = Default::default();
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn command_palette(&mut self, ctx: &egui::Context) {
        let Some(p) = &mut self.palette else { return };
        if p.items.is_empty() {
            p.items = palette::items(self.tabs.get(self.active), &self.s.recent, self.s.pull, self.s.terminal || self.tabs.get(self.active).is_some_and(|t| !t.term.tabs.is_empty()));
        }
        let Some(choice) = palette::show(ctx, p) else { return };
        self.palette = None;
        match choice {
            None => {}
            Some(palette::Cmd::Tab(a)) => {
                if let Some(t) = self.tabs.get_mut(self.active) {
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
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some((since, times)) = &mut self.perf {
            times.extend(frame.info().cpu_usage.map(|s| s * 1000.0));
            if since.elapsed().as_secs_f32() >= 2.0 && !times.is_empty() {
                times.sort_by(f32::total_cmp);
                let avg = times.iter().sum::<f32>() / times.len() as f32;
                let p95 = times[(times.len() * 95 / 100).min(times.len() - 1)];
                eprintln!("perf: {} frames  avg {avg:.2} ms  p95 {p95:.2} ms  max {:.2} ms", times.len(), times[times.len() - 1]);
                (*since, *times) = (std::time::Instant::now(), vec![]);
            }
        }
        crate::repo::tools::set(&self.s.tools);
        crate::repo::avatars::set_enabled(self.s.avatars);
        crate::repo::merge_opts::set_pick_x(self.s.cherry_pick_x);
        self.stacks_stale |= crate::repo::stack::set_follow(self.s.stacks, &self.s.keep_branches);
        if self.stacks_stale && self.settings.is_none() {
            self.stacks_stale = false;
            self.tabs.iter_mut().for_each(|t| t.reload(&ctx, true));
        }
        self.s.zoom = ctx.zoom_factor(); // ⌘+ / ⌘- / ⌘0 are egui's own; persisted here
        // An op that added an undo step gets an Undo button on its toast.
        let mut msgs: Vec<(Msg, Option<(PathBuf, u64)>)> = vec![];
        for t in &mut self.tabs {
            let (top, mut m) = (t.undo_top(), vec![]);
            t.poll(&ctx, &mut m);
            m.append(&mut t.notes);
            let undo = t.undo_top().filter(|&id| Some(id) > top).map(|id| (t.dir.clone(), id));
            let n = m.len();
            msgs.extend(m.into_iter().enumerate().map(|(i, m)| (m, undo.clone().filter(|_| i + 1 == n))));
        }
        let opens: Vec<PathBuf> = self.tabs.iter_mut().filter_map(|t| t.open_tab.take()).collect();
        for p in opens {
            self.open(&ctx, &p);
        }
        for (m, undo) in msgs {
            self.toast_undo(&ctx, m.text, m.error, undo);
        }
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
        let busy = self.tabs.iter().find_map(|t| t.busy().map(str::to_owned));
        if hold_close(&ctx, busy.as_deref(), &mut self.quit_pending) {
            self.toast(&ctx, format!("Quitting once '{}' finishes (or cancel it).", busy.as_deref().unwrap_or_default()), false);
        }
        if ctx.input(|i| i.viewport().close_requested())
            && let Some((i, p)) = self.tabs.iter().enumerate().find_map(|(i, t)| t.unsaved().map(|p| (i, p.to_owned())))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            (self.active, self.quit_pending) = (i, false);
            self.toast(&ctx, format!("Save or discard your edits to {p} before quitting."), true);
        }
        // A shell's job (vim, ssh, a dev server) may never end: ask rather than hold the quit.
        if busy.is_none() && ctx.input(|i| i.viewport().close_requested()) {
            let n: usize = self.tabs.iter().map(|t| t.term.running()).sum();
            if n > 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.quit_ask = Some(n);
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
                    ui.label(RichText::new(icon::WARNING).color(theme::MOD).size(16.0));
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
        #[cfg(target_os = "macos")]
        mac_quit::BUSY.store(self.tabs.iter().any(|t| t.busy().is_some() || t.term.busy() || t.unsaved().is_some()), std::sync::atomic::Ordering::SeqCst);
        drop_hint(&ctx, self.drop_hint_shot);
        self.show_toasts(&ctx);
        self.screenshot(&ctx);
        graph_view::dnd::preview(&ctx);
    }

    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.s.open = self.tabs.iter().map(|t| t.dir.clone()).collect();
        self.s.active = self.active;
        eframe::set_value(storage, "settings", &self.s);
    }
}

/// Closing while an op runs would leave it half-applied: the rest of its commands and
/// `poll_op`'s recovery (e.g. a failed Compose's unstage) never run. Holds the window
/// open until `busy` is None, then closes it. True when the close was first held.
fn hold_close(ctx: &egui::Context, busy: Option<&str>, pending: &mut bool) -> bool {
    if busy.is_some() && ctx.input(|i| i.viewport().close_requested()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        return !std::mem::replace(pending, true);
    }
    if busy.is_none() && std::mem::take(pending) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    false
}

/// ⌘Q and the Dock's Quit send `terminate:`, which winit turns into an exit with no close request,
/// so `hold_close` never sees it: while an op or terminal command runs, request a close instead.
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
mod close_tests {
    use egui::{RawInput, ViewportCommand, ViewportEvent, ViewportId};

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
        use eframe::App as _;
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
            let mut out = ctx.run_ui(raw, |ui| app.ui(ui, &mut frame));
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
        use eframe::App as _;
        let dir = std::env::temp_dir().join(format!("glasir-terminal-focus-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::git::run(&dir, &["init", "-q"]).unwrap();
        let ctx = egui::Context::default();
        let mut app = super::App::new(&eframe::CreationContext::_new_kittest(ctx.clone()), Some(dir.clone()));
        let mut frame = eframe::Frame::_new_kittest();
        let mut step = |app: &mut super::App| {
            let raw = RawInput { screen_rect: Some(egui::Rect::from_min_size(Default::default(), egui::vec2(1200.0, 800.0))), ..Default::default() };
            ctx.run_ui(raw, |ui| app.ui(ui, &mut frame)).textures_delta.clear();
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
}
