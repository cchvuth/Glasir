//! Persisted `Settings` and the Settings dialog (theme flavour swatches, glass,
//! auto-fetch, pull mode, commit email rules, Claude token / model).

use super::app::App;
use crate::ai;
use crate::git;
use crate::repo::{PullMode, Task, tools};
use crate::theme::{self, icon};
use egui::{Align2, Button, Id, Key, RichText, Sense, TextEdit, Ui, vec2};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub recent: Vec<PathBuf>,
    pub open: Vec<PathBuf>,
    pub active: usize,
    pub flavor: theme::Flavor,
    /// Last-used flavour of each kind, for the title-bar sun/moon toggle.
    pub last_dark: theme::Flavor,
    pub last_light: theme::Flavor,
    /// Pre-flavour setting (`light: true`), read once to migrate to Daylight.
    #[serde(skip_serializing)]
    pub(super) light: bool,
    pub max_commits: usize,
    pub pull: PullMode,
    pub split_diff: bool,
    pub clone_dir: Option<PathBuf>,
    pub ai: ai::Config,
    /// macOS 26+: real Liquid Glass (NSGlassEffectView) behind the window.
    pub native_glass: bool,
    /// Liquid Glass transparency, 0.0 (solid) ..= 1.0 (clearest).
    pub transparency: f32,
    /// Background fetch interval in minutes; 0 = off.
    pub auto_fetch: u32,
    /// Graph filter (hidden / soloed refs, first-parent) per repo path.
    pub views: std::collections::HashMap<PathBuf, crate::repo::View>,
    /// Commit email rules, one `host -> @domain` per line (`repo::identity`).
    pub email_rules: String,
    pub tools: crate::repo::tools::Tools,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            recent: vec![],
            open: vec![],
            active: 0,
            flavor: theme::Flavor::Midnight,
            last_dark: theme::Flavor::Midnight,
            last_light: theme::Flavor::Daylight,
            light: false,
            max_commits: crate::repo::PAGE,
            pull: PullMode::Merge,
            split_diff: false,
            clone_dir: None,
            ai: ai::Config::default(),
            native_glass: false,
            transparency: 0.25,
            auto_fetch: 10,
            views: Default::default(),
            email_rules: String::new(),
            tools: Default::default(),
        }
    }
}

/// Label column of every settings grid, so the sections line up.
const LABEL_W: f32 = 130.0;

#[derive(Default)]
pub(super) struct SettingsDlg {
    token: String,
    source: Option<&'static str>,
    note: Option<(bool, String)>,
    test: Option<Task<git::Res<String>>>,
    /// Detected editor / terminal, shown as the fields' hints.
    editor: String,
    terminal: &'static str,
}

impl App {
    pub(super) fn open_settings(&mut self) {
        let (editor, terminal) = (tools::default_editor(), tools::default_terminal());
        self.settings = Some(SettingsDlg { source: ai::token_source(), editor, terminal, ..Default::default() });
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
            let max_h = ui.ctx().content_rect().height() - 160.0;
            egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
                egui::Grid::new("settings_grid").num_columns(2).min_col_width(LABEL_W).spacing([16.0, 8.0]).show(ui, |ui| {
                    ui.label("Theme");
                    ui.vertical(|ui| {
                        for row in [theme::Flavor::DARK, theme::Flavor::LIGHT] {
                            ui.horizontal(|ui| {
                                for f in row {
                                    if flavor_swatch(ui, f, s.flavor == f).clicked() {
                                        set_flavor(s, ui.ctx(), f);
                                    }
                                }
                            });
                        }
                    });
                    ui.end_row();
                    ui.label("Commits to load");
                    if ui.add(egui::DragValue::new(&mut s.max_commits).range(1000..=2_000_000).speed(1000)).changed() {
                        reload = true;
                    }
                    ui.end_row();
                    ui.label("Glass transparency");
                    ui.horizontal(|ui| {
                        let r = theme::slider(ui, &mut s.transparency);
                        ui.label(RichText::new(format!("{:.0}%", s.transparency * 100.0)).small().color(theme::dim(ui)));
                        if r.changed() {
                            theme::set_transparency(s.transparency);
                        }
                    })
                    .response
                    .on_hover_text("How much of the backdrop shows through glass panels. Higher is clearer.");
                    ui.end_row();
                    if cfg!(target_os = "macos") {
                        ui.label("Window glass");
                        ui.checkbox(&mut s.native_glass, "Native Liquid Glass backdrop (experimental, macOS 26+, restart)");
                        ui.end_row();
                    }
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
                    ui.label("Commit email rules");
                    ui.vertical(|ui| {
                        ui.add(TextEdit::multiline(&mut s.email_rules).hint_text("ghe.corp.example -> @corp.example").desired_rows(2).desired_width(330.0));
                        ui.label(RichText::new("Warns in the commit box when a repo on that host commits with another email").small().color(theme::dim(ui)));
                    });
                    ui.end_row();
                });

                ui.add_space(10.0);
                ui.separator();
                tools_section(ui, &mut s.tools, &st.editor, st.terminal);
                ui.add_space(10.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} AI (Claude)", icon::SPARKLE)).strong());
                    ui.checkbox(&mut s.ai.enabled, "Enable AI features").on_hover_text("Off: every AI button and menu item is hidden");
                });
                ui.label(
                    RichText::new(
                        "Bring your own token: an Anthropic API key (sk-ant-api…) or an OAuth token (sk-ant-oat…, e.g. from `claude setup-token`). It is stored in your system keychain and only sent to the API URL below.",
                    )
                    .small()
                    .color(theme::dim(ui)),
                );
                ui.add_space(4.0);
                egui::Grid::new("ai_grid").num_columns(2).min_col_width(LABEL_W).spacing([16.0, 8.0]).show(ui, |ui| {
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
            self.settings = None;
            if reload {
                for t in &mut self.tabs {
                    t.reload(ctx, true);
                }
            }
        }
    }

}

fn tools_section(ui: &mut Ui, t: &mut tools::Tools, editor: &str, terminal: &str) {
    ui.label(RichText::new(format!("{} Tools", icon::TERMINAL)).strong());
    ui.add_space(4.0);
    egui::Grid::new("tools_grid").num_columns(2).min_col_width(LABEL_W).spacing([16.0, 8.0]).show(ui, |ui| {
        ui.label("Editor");
        ui.add(TextEdit::singleline(&mut t.editor).hint_text(editor).desired_width(330.0))
            .on_hover_text("Opens files and the repository (toolbar, ⌘P, file menus). {path} is the file; without it the path is appended. Empty = detected.");
        ui.end_row();
        ui.label("Terminal");
        ui.add(TextEdit::singleline(&mut t.terminal).hint_text(terminal).desired_width(330.0))
            .on_hover_text("Opened in the repository folder ({path}). Empty = your platform's terminal.");
        ui.end_row();
    });
    ui.add_space(8.0);
    ui.label(RichText::new("Custom commands").strong());
    ui.label(
        RichText::new("git arguments (run without a shell) or a shell line, run in the repository. Placeholders: {repo} {branch} {sha} {path}. With {path} a command shows in file menus, with {sha} in commit menus, the rest in ⌘P; output goes to a toast and the command log.")
            .small()
            .color(theme::dim(ui)),
    );
    ui.add_space(4.0);
    let mut remove = None;
    for (i, c) in t.commands.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(&mut c.name).hint_text("Name").desired_width(130.0));
            ui.add(TextEdit::singleline(&mut c.command).hint_text("git push origin HEAD:refs/for/{branch}").code_editor().desired_width(320.0));
            if theme::pill(ui, icon::TRASH).on_hover_text("Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        t.commands.remove(i);
    }
    if ui.button(format!("{} Add command", icon::PLUS)).clicked() {
        t.commands.push(Default::default());
    }
}

fn set_flavor(s: &mut Settings, ctx: &egui::Context, f: theme::Flavor) {
    s.flavor = f;
    if f.dark() {
        s.last_dark = f;
    } else {
        s.last_light = f;
    }
    theme::apply(ctx, f);
}

pub(super) fn toggle_flavor(s: &mut Settings, ctx: &egui::Context) {
    let f = if s.flavor.dark() { s.last_light } else { s.last_dark };
    set_flavor(s, ctx, f);
}

/// A clickable mini-preview of a flavour: its backdrop, a glass card and the accent.
fn flavor_swatch(ui: &mut Ui, f: theme::Flavor, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(86.0, 62.0), Sense::click());
    let p = ui.painter().with_clip_rect(rect);
    let r = rect.shrink(2.0);
    p.rect_filled(r, 10.0, f.palette().base);
    theme::canvas_of(&p.with_clip_rect(r.shrink(1.0)), r, false, f);
    let [gr, gg, gb] = f.palette().glass;
    let card = egui::Rect::from_min_size(r.min + vec2(8.0, 8.0), vec2(r.width() - 16.0, 22.0));
    p.rect_filled(card, 7.0, egui::Color32::from_rgba_unmultiplied(gr, gg, gb, 200));
    p.circle_filled(card.left_center() + vec2(11.0, 0.0), 5.0, f.palette().accent);
    let text = if f.dark() { egui::Color32::from_white_alpha(210) } else { egui::Color32::from_black_alpha(190) };
    p.text(r.center_bottom() - vec2(0.0, 13.0), Align2::CENTER_CENTER, f.name(), egui::FontId::proportional(11.5), text);
    let ring = if selected { egui::Stroke::new(2.5, f.palette().accent) } else { egui::Stroke::new(1.0, theme::faint(ui)) };
    p.rect_stroke(r, 10.0, ring, egui::StrokeKind::Outside);
    resp.on_hover_text(if f.dark() { "Dark flavour" } else { "Light flavour" })
}
