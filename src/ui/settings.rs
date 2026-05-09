//! Persisted `Settings` and the Settings dialog (theme, auto-fetch, pull mode).

use super::app::App;
use crate::repo::PullMode;
use crate::theme;
use egui::{Id, Key, RichText};
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
    pub split_diff: bool,
    pub clone_dir: Option<PathBuf>,
    /// Background fetch interval in minutes; 0 = off.
    pub auto_fetch: u32,
    /// Graph filter (hidden / soloed refs, first-parent) per repo path.
    pub views: std::collections::HashMap<PathBuf, crate::repo::View>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            recent: vec![],
            open: vec![],
            active: 0,
            light: false,
            max_commits: crate::repo::PAGE,
            pull: PullMode::Merge,
            split_diff: false,
            clone_dir: None,
            auto_fetch: 10,
            views: Default::default(),
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
                ui.label("Auto-fetch");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut s.auto_fetch).range(0..=120).suffix(" min"));
                    ui.label(RichText::new("every N minutes and on focus · 0 = off").small().color(theme::dim(ui)));
                });
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
