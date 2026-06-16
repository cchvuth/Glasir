//! The main window: `App` (open tabs, settings, global dialogs), the eframe
//! frame loop, global shortcuts and window setup. Each screen area lives in
//! its own `ui` module; this file only wires them together.

use super::settings::{Settings, SettingsDlg, toggle_flavor};
use super::toasts::Toast;
use super::welcome::CloneDlg;
use super::{command_log, graph_view, palette, repo_view};
use crate::git;
use crate::repo::{Action, Msg, Op, RepoTab, Search, Task, fetch_op};
use crate::theme::{self, icon};
use egui::{Frame, Key, Margin, RichText, Ui};
use std::path::{Path, PathBuf};

pub struct App {
    /// `GLASIR_SCREENSHOT=out.bmp`: save a screenshot once loaded, then exit (UI checks/CI).
    pub(super) shot: Option<(String, u32)>,
    /// `GLASIR_PERF=1`: per-frame CPU times, summarised on stderr every 2 s.
    perf: Option<(std::time::Instant, Vec<f32>)>,
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
    /// The OS draws glass behind the window, so the canvas stays translucent.
    pub(super) glass_backdrop: bool,
    pub(super) shortcuts: bool,
    /// Screenshot harness: paint the drop hint without a drag.
    pub(super) drop_hint_shot: bool,
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
        let mut app =
            App {
            shot,
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
            glass_backdrop,
            shortcuts: false,
            drop_hint_shot: false,
            s,
        };
        let active = app.s.active;
        for p in app.s.open.clone() {
            if p.exists() {
                app.open(&cc.egui_ctx, &p);
            }
        }
        app.active = active.min(app.tabs.len());
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
            Err(e) => self.toast(ctx, format!("Not a git repository: {}\n{e}", path.display()), true),
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.memory(|m| m.focused().is_some());
        let (cmd, shift) = ctx.input(|i| (i.modifiers.command, i.modifiers.shift));
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
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
        if cmd && (pressed(Key::P) || pressed(Key::K)) {
            self.palette = Some(Default::default());
        }
        if cmd && pressed(Key::T) {
            self.active = self.tabs.len();
        }
        if cmd && pressed(Key::W) && self.active < self.tabs.len() {
            self.tabs.remove(self.active);
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
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
        if cmd && pressed(Key::F) {
            t.diff = None;
            t.search = Some(Search::open());
        }
        if cmd && shift && pressed(Key::S) {
            act = t.data.as_ref().and_then(|d| crate::repo::stage_all_op(&d.status)).map(Action::Git).or(act);
        }
        if cmd && shift && pressed(Key::U) {
            act = Some(Action::Git(Op::new("Unstage all", &["reset", "-q"]).light()));
        }
        if let Some(a) = act {
            t.apply(ctx, a);
        }
    }

    pub(super) fn pick_and_open(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new().set_title("Open repository").pick_folder() {
            self.open(ctx, &p);
        }
    }

    fn command_palette(&mut self, ctx: &egui::Context) {
        let Some(p) = &mut self.palette else { return };
        let items = palette::items(self.tabs.get(self.active), &self.s.recent, self.s.pull);
        let Some(choice) = palette::show(ctx, p, items) else { return };
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
        self.s.zoom = ctx.zoom_factor(); // ⌘+ / ⌘- / ⌘0 are egui's own; persisted here
        // An op that added an undo step gets an Undo button on its toast.
        let mut msgs: Vec<(Msg, Option<(PathBuf, usize)>)> = vec![];
        for t in &mut self.tabs {
            let (depth, mut m) = (t.undo_depth(), vec![]);
            t.poll(&ctx, &mut m);
            m.append(&mut t.notes);
            let undo = (t.undo_depth() > depth).then(|| (t.dir.clone(), t.undo_depth()));
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
            t.reload(&ctx, true);
        }
        let every = if self.shot.is_some() { 0 } else { self.s.auto_fetch };
        for (i, t) in self.tabs.iter_mut().enumerate() {
            t.auto_fetch(&ctx, every, i == self.active && focused && !self.focused);
        }
        self.focused = focused;
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
        self.clone_dialog(&ctx);
        self.settings_dialog(&ctx);
        self.command_palette(&ctx);
        if self.shortcuts {
            self.shortcuts = super::shortcuts::show(&ctx);
        }
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
