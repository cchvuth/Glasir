//! Persisted `Settings` and the Settings dialog (theme, pull mode).

use super::app::App;
use crate::repo::PullMode;
use crate::theme;
use egui::{Id, Key};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub recent: Vec<PathBuf>,
    pub open: Vec<PathBuf>,
    pub active: usize,
    pub light: bool,
    pub max_commits: usize,
    pub pull: PullMode,
    pub clone_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            recent: vec![],
            open: vec![],
            active: 0,
            light: false,
            max_commits: 50_000,
            pull: PullMode::Merge,
            clone_dir: None,
        }
    }
}

impl App {
    pub(super) fn open_settings(&mut self) {
        self.settings = true;
    }

    pub(super) fn settings_dialog(&mut self, ctx: &egui::Context) {
        if !self.settings {
            return;
        }
        let s = &mut self.s;
        let mut close = false;
        let mut reload = false;
        egui::Modal::new(Id::new("settings")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("Settings");
            ui.add_space(6.0);
            egui::Grid::new("settings_grid").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                ui.label("Theme");
                ui.horizontal(|ui| {
                    if ui.radio_value(&mut s.light, false, "Dark").changed() | ui.radio_value(&mut s.light, true, "Light").changed() {
                        theme::apply(ui.ctx(), s.light);
                    }
                });
                ui.end_row();
                ui.label("Commits to load");
                if ui.add(egui::DragValue::new(&mut s.max_commits).range(1000..=2_000_000).speed(1000)).changed() {
                    reload = true;
                }
                ui.end_row();
                ui.label("Default pull");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut s.pull, PullMode::Merge, "Merge");
                    ui.radio_value(&mut s.pull, PullMode::FfOnly, "Fast-forward only");
                    ui.radio_value(&mut s.pull, PullMode::Rebase, "Rebase");
                });
                ui.end_row();
            });

            ui.add_space(8.0);
            ui.separator();
            if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                close = true;
            }
        });
        if reload {
            for t in &mut self.tabs {
                t.max_commits = self.s.max_commits;
            }
        }
        if close {
            self.settings = false;
            if reload {
                for t in &mut self.tabs {
                    t.reload(ctx, true);
                }
            }
        }
    }

}

pub(super) fn toggle_theme(s: &mut Settings, ctx: &egui::Context) {
    s.light = !s.light;
    theme::apply(ctx, s.light);
}
