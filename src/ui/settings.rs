//! Persisted `Settings` and the Settings dialog (theme, auto-fetch, pull mode, Claude token / model).

use super::app::App;
use crate::ai;
use crate::git;
use crate::repo::{PullMode, Task};
use crate::theme::{self, icon};
use egui::{Button, Id, Key, RichText, TextEdit};
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
    pub ai: ai::Config,
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
            ai: ai::Config::default(),
            auto_fetch: 10,
            views: Default::default(),
        }
    }
}

#[derive(Default)]
pub(super) struct SettingsDlg {
    token: String,
    source: Option<&'static str>,
    note: Option<(bool, String)>,
    test: Option<Task<git::Res<String>>>,
}

impl App {
    pub(super) fn open_settings(&mut self) {
        self.settings = Some(SettingsDlg { source: ai::token_source(), ..Default::default() });
    }

    pub(super) fn settings_dialog(&mut self, ctx: &egui::Context) {
        let Some(st) = &mut self.settings else { return };
        if let Some(r) = Task::poll(&mut st.test) {
            st.note = Some(match r {
                Ok(m) => (true, format!("Connected ✔  {m}")),
                Err(e) => (false, e),
            });
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

            ui.add_space(10.0);
            ui.separator();
            ui.label(RichText::new(format!("{} AI (Claude)", icon::SPARKLE)).strong());
            ui.label(
                RichText::new(
                    "Bring your own token: an Anthropic API key (sk-ant-api…) or an OAuth token (sk-ant-oat…, e.g. from `claude setup-token`).                      It is stored in your system keychain and only sent to the API URL below.",
                )
                .small()
                .color(theme::dim(ui)),
            );
            ui.add_space(4.0);
            egui::Grid::new("ai_grid").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                ui.label("Claude token");
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut st.token).password(true).hint_text("Paste token").desired_width(250.0));
                    if ui.add_enabled(!st.token.trim().is_empty(), Button::new("Save")).clicked() {
                        match ai::save_token(&st.token) {
                            Ok(()) => {
                                ai::remove_plain_token();
                                st.note = Some((true, "Token saved to the system keychain.".into()));
                            }
                            Err(e) => {
                                st.note = Some(match ai::save_plain_token(&st.token) {
                                    Ok(()) => (false, format!("No system keychain ({e}); saved to a private file readable only by you.")),
                                    Err(e2) => (false, format!("Could not save the token: {e}; {e2}")),
                                });
                            }
                        }
                        st.token.clear();
                        st.source = ai::token_source();
                    }
                    if st.source.is_some() && ui.button("Remove").clicked() {
                        let _ = ai::save_token("");
                        ai::remove_plain_token();
                        st.source = ai::token_source();
                        st.note = Some((true, "Stored token removed.".into()));
                    }
                });
                ui.end_row();
                ui.label("");
                ui.label(
                    RichText::new(match st.source {
                        Some(src) => format!("{} Using the token from {src}", icon::CHECK_CIRCLE),
                        None => format!("{} No token configured", icon::WARNING),
                    })
                    .small()
                    .color(if st.source.is_some() { theme::ADD } else { theme::MOD }),
                );
                ui.end_row();
                ui.label("Model");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("ai_model").selected_text(&s.ai.model).width(200.0).show_ui(ui, |ui| {
                        for m in ai::MODELS {
                            ui.selectable_value(&mut s.ai.model, m.to_owned(), m);
                        }
                    });
                    ui.add(TextEdit::singleline(&mut s.ai.model).desired_width(150.0)).on_hover_text("Any model id");
                });
                ui.end_row();
                ui.label("API base URL");
                ui.add(TextEdit::singleline(&mut s.ai.base_url).desired_width(330.0))
                    .on_hover_text("Change only for an Anthropic-compatible gateway");
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if st.test.is_some() {
                    ui.spinner();
                    ui.label("Testing…");
                } else if ui.button("Test connection").clicked() {
                    let cfg = s.ai.clone();
                    st.note = None;
                    st.test = Some(Task::spawn(ui.ctx(), move || ai::test(&cfg)));
                }
            });
            if let Some((ok, n)) = &st.note {
                ui.colored_label(if *ok { theme::ADD } else { theme::DEL }, n);
            }
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
            self.settings = None;
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
