//! Test helpers: a headless egui context driven frame by frame (pointer events, the texts drawn) and a loaded scratch repo.

use crate::git::{Ref, RefKind};
use crate::repo::RepoTab;
use egui::{Event, PointerButton, Pos2, RawInput, Rect, Ui, vec2};

pub struct Sim {
    pub ctx: egui::Context,
    time: f64,
    size: egui::Vec2,
    shapes: Vec<egui::epaint::ClippedShape>,
}

impl Sim {
    pub fn new(w: f32, h: f32) -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx, crate::theme::Flavor::Midnight);
        let mut sim = Sim { ctx, time: 0.0, size: vec2(w, h), shapes: vec![] };
        sim.frame(vec![], |_| {});
        sim
    }

    pub fn frame(&mut self, events: Vec<Event>, ui: impl FnMut(&mut Ui)) {
        self.time += 0.05;
        let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)), time: Some(self.time), events, ..Default::default() };
        let mut out = self.ctx.run_ui(raw, ui);
        out.textures_delta.clear();
        self.shapes = out.shapes;
    }

    pub fn text(&self, is: impl Fn(&str) -> bool) -> Option<Pos2> {
        self.shapes.iter().find_map(|c| match &c.shape {
            egui::Shape::Text(t) if is(t.galley.text()) => Some(t.galley.rect.translate(t.pos.to_vec2()).center()),
            _ => None,
        })
    }

    pub fn click(&mut self, at: Pos2, button: PointerButton, mut ui: impl FnMut(&mut Ui)) {
        let press = |pressed| Event::PointerButton { pos: at, button, pressed, modifiers: Default::default() };
        for events in [vec![Event::PointerMoved(at)], vec![], vec![press(true)], vec![press(false)], vec![], vec![], vec![]] {
            self.frame(events, &mut ui);
        }
    }

    pub fn open_menu(&mut self, row: &str, mut draw: impl FnMut(&mut Ui)) {
        self.frame(vec![], &mut draw);
        let at = self.text(|t| t.ends_with(row)).unwrap_or_else(|| panic!("{row} is not drawn"));
        self.click(at, PointerButton::Secondary, draw);
    }

    pub fn pick(&mut self, item: &str, mut draw: impl FnMut(&mut Ui)) {
        self.frame(vec![], &mut draw);
        self.frame(vec![], &mut draw);
        assert!(egui::Popup::is_any_open(&self.ctx), "the menu closed");
        let at = self.text(|t| t.starts_with(item)).unwrap_or_else(|| panic!("{item} is not in the menu"));
        self.click(at, PointerButton::Primary, draw);
    }

    pub fn menu_survives(&mut self, row: &str, item: &str, mut draw: impl FnMut(&mut Ui, bool)) {
        self.open_menu(row, |ui| draw(ui, false));
        self.pick(item, |ui| draw(ui, true));
    }
}

pub fn local_ref(name: &str) -> Ref {
    Ref { kind: RefKind::Local, name: name.into(), full: format!("refs/heads/{name}"), target: String::new(), upstream: String::new(), ahead: 0, behind: 0, gone: false, head: false, worktree: None, date: 0, message: String::new(), annotated: false }
}

pub fn load(t: &mut RepoTab, ctx: &egui::Context) {
    let (mut msgs, start) = (vec![], std::time::Instant::now());
    while t.data.is_none() || t.loading() || t.busy().is_some() {
        assert!(start.elapsed().as_secs() < 120, "timed out loading the repo");
        t.poll(ctx, &mut msgs);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

pub struct Tab {
    tab: RepoTab,
    _dir: Scratch,
}

struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl std::ops::Deref for Tab {
    type Target = RepoTab;
    fn deref(&self) -> &RepoTab {
        &self.tab
    }
}

impl std::ops::DerefMut for Tab {
    fn deref_mut(&mut self) -> &mut RepoTab {
        &mut self.tab
    }
}

pub fn tab(ctx: &egui::Context, name: &str, commits: &[&str]) -> Tab {
    let dir = std::env::temp_dir().join(format!("glasir-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| crate::git::run(&dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "T"]);
    git(&["config", "commit.gpgsign", "false"]);
    for msg in commits {
        std::fs::write(dir.join("f"), msg).unwrap();
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", msg]);
    }
    let mut t = RepoTab::new(ctx, dir.clone(), 100, Default::default());
    load(&mut t, ctx);
    Tab { tab: t, _dir: Scratch(dir) }
}
