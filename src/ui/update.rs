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
}

impl App {
    pub(super) fn check_updates(&mut self, ctx: &egui::Context) {
        self.update.check = Some(Task::spawn(ctx, update::check));
    }

    pub(super) fn update_pill(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        // Offline or rate-limited: nothing to say until the next start.
        if let Some(Ok(r)) = Task::poll(&mut self.update.check) {
            self.update.found = r;
        }
        match Task::poll(&mut self.update.install) {
            Some(Ok(p)) => self.update.ready = Some(p),
            Some(Err(e)) => {
                let page = self.update.found.as_ref().map_or(String::new(), |r| format!("\nDownload it from {}", r.page));
                self.toast(&ctx, format!("Update failed: {e}{page}"), true);
            }
            None => {}
        }
        if let Some(p) = self.update.ready.clone() {
            if theme::pill(ui, format!("{}  Restart to update", icon::ARROWS_CLOCKWISE)).on_hover_text("The new version is installed: restart Glasir to use it").clicked() {
                match update::relaunch(&p) {
                    Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    Err(e) => self.toast(&ctx, e, true),
                }
            }
        } else if self.update.install.is_some() {
            ui.spinner();
        } else if let Some(r) = &self.update.found {
            let auto = r.asset.is_some() && update::target().is_some();
            let tip = match auto {
                true => format!("Glasir {} is out: download and install it here (right-click: release notes)", r.version),
                false => format!("Glasir {} is out: open its release page", r.version),
            };
            let pill = theme::pill(ui, egui::RichText::new(format!("{}  Update to {}", icon::DOWNLOAD_SIMPLE, r.version)).color(theme::accent())).on_hover_text(tip);
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
