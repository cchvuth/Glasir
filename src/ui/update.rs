//! The title bar's update pill: a newer release found at startup, installing it, restarting.

use super::app::App;
use crate::git::Res;
use crate::repo::Task;
use crate::theme::{self, icon};
use crate::update::{self, Release};
use egui::Ui;
use std::path::PathBuf;

#[derive(Default)]
pub(super) struct Updater {
    check: Option<Task<Res<Option<Release>>>>,
    pub(super) found: Option<Release>,
    install: Option<Task<Res<PathBuf>>>,
    /// Installed: relaunch this.
    ready: Option<PathBuf>,
    /// The relaunch helper is waiting for this process to exit.
    relaunching: bool,
    checked: Option<std::time::Instant>,
}

impl App {
    /// At start and when the window comes back into focus; GitHub allows 60 unauthenticated
    /// requests an hour, so at most one check per 10 minutes.
    pub(super) fn check_updates(&mut self, ctx: &egui::Context) {
        let u = &mut self.update;
        let recent = u.checked.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(600));
        if !self.s.check_updates || self.shot.is_some() || cfg!(test) || recent || u.check.is_some() || u.install.is_some() || u.ready.is_some() {
            return;
        }
        u.checked = Some(std::time::Instant::now());
        u.check = Some(Task::spawn(ctx, update::check));
    }

    /// Closes the window (asking first as any quit does) and starts the installed version once it's gone.
    fn restart(&mut self, ctx: &egui::Context) {
        let Some(p) = &self.update.ready else { return };
        if !self.update.relaunching {
            match update::relaunch(p) {
                Ok(()) => self.update.relaunching = true,
                Err(e) => return self.toast(ctx, e, true),
            }
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    pub(super) fn update_pill(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        // Offline or rate-limited: nothing to say until the next start.
        if let Some(Ok(r)) = Task::poll(&mut self.update.check) {
            self.update.found = r;
        }
        match Task::poll(&mut self.update.install) {
            Some(Ok(p)) => {
                self.update.ready = Some(p);
                self.restart(&ctx);
            }
            Some(Err(e)) => {
                let page = self.update.found.as_ref().map_or(String::new(), |r| format!("\nDownload it from {}", r.page));
                self.toast(&ctx, format!("Update failed: {e}{page}"), true);
            }
            None => {}
        }
        if let Some(p) = self.update.ready.clone() {
            if theme::pill(ui, format!("{}  Restart to update", icon::ARROWS_CLOCKWISE)).on_hover_text(format!("The new version is installed at {}: restart Glasir to use it", p.display())).clicked() {
                self.restart(&ctx);
            }
        } else if self.update.install.is_some() {
            ui.spinner();
        } else if let Some(r) = &self.update.found {
            let auto = r.asset.is_some() && update::target().is_some();
            let tip = match auto {
                true => format!("Glasir {} is out: download and install it here (right-click: release notes)", r.version),
                false => format!("Glasir {} is out: open its release page", r.version),
            };
            // Accent-filled with a pulsing glow behind it (painted once its size is known).
            let slot = ui.painter().add(egui::Shape::Noop);
            let pill = theme::pill(ui, egui::RichText::new(format!("{}  Update to {}", icon::DOWNLOAD_SIMPLE, r.version)).color(egui::Color32::WHITE).strong()).on_hover_text(tip);
            let pulse = 0.5 + 0.5 * (ui.input(|i| i.time) as f32 * std::f32::consts::TAU / 1.6).sin();
            let (rect, accent) = (pill.rect, theme::accent());
            let mut glow: Vec<egui::Shape> = [(5.0, 0.12), (3.0, 0.22), (1.5, 0.35)]
                .into_iter()
                .map(|(grow, k): (f32, f32)| egui::Shape::rect_filled(rect.expand(grow * (0.5 + 0.5 * pulse)), rect.height() / 2.0 + grow, accent.gamma_multiply(k * (0.3 + 0.7 * pulse))))
                .collect();
            glow.push(egui::Shape::rect_filled(rect, rect.height() / 2.0, accent));
            ui.painter().set(slot, egui::Shape::Vec(glow));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
            let page = r.page.clone();
            pill.context_menu(|ui| {
                if ui.button("Release notes").clicked() {
                    let _ = open::that_detached(&page);
                    ui.close();
                }
            });
            if pill.clicked() {
                match auto {
                    true => {
                        let r = r.clone();
                        self.update.install = Some(Task::spawn(&ctx, move || update::install(&r)));
                    }
                    false => _ = open::that_detached(&page),
                }
            }
        }
    }
}
