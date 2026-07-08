//! Persisted `Settings` and the Settings dialog (theme flavour swatches, glass,
//! auto-fetch, pull mode, commit email rules, AI provider / token / model).

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
    /// UI zoom (⌘+ / ⌘- / ⌘0).
    pub zoom: f32,
    /// The welcome page's first-run tips are still shown.
    pub tips: bool,
    /// Commit authors' GitHub / GitHub Enterprise profile pictures (`repo::avatars`).
    pub avatars: bool,
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
            zoom: 1.0,
            tips: true,
            avatars: true,
        }
    }
}

/// Label column of every settings grid, so the sections line up.
const LABEL_W: f32 = 130.0;

#[derive(Default)]
pub(super) struct SettingsDlg {
    token: String,
    /// Where the token comes from (an environment variable or where it was saved).
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
        let source = token_source(&mut self.s.ai);
        self.settings = Some(SettingsDlg { source, editor, terminal, ..Default::default() });
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
                    ui.label("Avatars");
                    ui.checkbox(&mut s.avatars, "Show GitHub avatars")
                        .on_hover_text("Authors' profile pictures from the origin remote's GitHub / GitHub Enterprise host, using git's saved credentials for it. Off: no avatar requests at all.");
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
                    ui.label("Commit email rules");
                    ui.vertical(|ui| {
                        ui.add(TextEdit::multiline(&mut s.email_rules).margin(crate::theme::INPUT_MARGIN).hint_text("ghe.corp.example -> @corp.example").desired_rows(2).desired_width(330.0));
                        ui.label(RichText::new("Warns in the commit box when a repo on that host commits with another email").small().color(theme::dim(ui)));
                    });
                    ui.end_row();
                });

                ui.add_space(10.0);
                ui.add_space(6.0);
                tools_section(ui, &mut s.tools, &st.editor, st.terminal);
                ui.add_space(10.0);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} AI", icon::SPARKLE)).strong());
                    ui.checkbox(&mut s.ai.enabled, "Enable AI features").on_hover_text("Off: every AI button and menu item is hidden");
                });
                let p = s.ai.provider;
                ui.label(
                    RichText::new(match p {
                        ai::Provider::Claude => "Bring your own token: an Anthropic API key (sk-ant-api…) or an OAuth token (sk-ant-oat…, e.g. from `claude setup-token`). It is stored in your system keychain and only sent to the API URL below.",
                        ai::Provider::OpenAi => "Bring your own OpenAI API key (sk-…, from platform.openai.com). It is stored in your system keychain and only sent to the API URL below.",
                    })
                    .small()
                    .color(theme::dim(ui)),
                );
                ui.add_space(4.0);
                egui::Grid::new("ai_grid").num_columns(2).min_col_width(LABEL_W).spacing([16.0, 8.0]).show(ui, |ui| {
                    ui.label("Provider");
                    ui.horizontal(|ui| {
                        for q in ai::Provider::ALL {
                            if ui.radio_value(&mut s.ai.provider, q, q.label()).changed() {
                                st.source = token_source(&mut s.ai);
                                st.token.clear();
                                st.note = None;
                            }
                        }
                    });
                    ui.end_row();
                    let p = s.ai.provider;
                    ui.label(if p == ai::Provider::OpenAi { "OpenAI API key" } else { "Claude token" });
                    ui.horizontal(|ui| {
                        ui.add(TextEdit::singleline(&mut st.token).margin(crate::theme::INPUT_MARGIN).password(true).hint_text("Paste token").desired_width(250.0));
                        if ui.add_enabled(!st.token.trim().is_empty(), Button::new("Save")).clicked() {
                            let saved = match ai::save_token(p, &st.token) {
                                Ok(()) => {
                                    ai::remove_plain_token(p);
                                    st.note = Some((true, "Token saved to the system keychain.".into()));
                                    Some(ai::IN_KEYCHAIN)
                                }
                                Err(e) => match ai::save_plain_token(p, &st.token) {
                                    Ok(()) => {
                                        st.note = Some((false, format!("No system keychain ({e}); saved to a private file readable only by you.")));
                                        Some(ai::IN_FILE)
                                    }
                                    Err(e2) => {
                                        st.note = Some((false, format!("Could not save the token: {e}; {e2}")));
                                        None
                                    }
                                },
                            };
                            st.token.clear();
                            if let Some(at) = saved {
                                *s.ai.current().2 = Some(at.into());
                                st.source = ai::env_source(p).or(Some(at));
                            }
                        }
                        if st.source.is_some() && ui.button("Remove").clicked() {
                            let in_file = s.ai.current().2.as_deref() == Some(ai::IN_FILE);
                            ai::remove_plain_token(p);
                            match ai::save_token(p, "") {
                                // A file-saved token means there is no keychain to delete from.
                                Err(e) if !in_file => st.note = Some((false, format!("Could not remove the token from the keychain: {e}"))),
                                _ => {
                                    *s.ai.current().2 = Some(String::new());
                                    st.source = ai::env_source(p);
                                    st.note = Some((true, "Stored token removed.".into()));
                                }
                            }
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
                    let (model, base_url, _) = s.ai.current();
                    ui.label("Model");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("ai_model").selected_text(model.as_str()).width(200.0).show_ui(ui, |ui| {
                            for &m in p.models() {
                                ui.selectable_value(model, m.to_owned(), m);
                            }
                        });
                        ui.add(TextEdit::singleline(model).margin(crate::theme::INPUT_MARGIN).desired_width(150.0)).on_hover_text("Any model id");
                    });
                    ui.end_row();
                    ui.label("API base URL");
                    ui.add(TextEdit::singleline(base_url).margin(crate::theme::INPUT_MARGIN).desired_width(330.0)).on_hover_text(match p {
                        ai::Provider::Claude => "Change only for an Anthropic-compatible gateway",
                        ai::Provider::OpenAi => "Change only for an OpenAI-compatible endpoint (gateway or local server)",
                    });
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
            ui.add_space(4.0);
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

/// Where the selected provider's token comes from. Never reads the keychain
/// (macOS would ask for the password): the saved location is remembered in the
/// settings; older settings check once.
fn token_source(cfg: &mut ai::Config) -> Option<&'static str> {
    let p = cfg.provider;
    let saved_in = cfg.current().2;
    if saved_in.is_none() {
        *saved_in = Some(ai::find_saved(p).unwrap_or_default().to_owned());
    }
    let saved = match saved_in.as_deref() {
        Some(s) if s == ai::IN_FILE => Some(ai::IN_FILE),
        Some(s) if !s.is_empty() => Some(ai::IN_KEYCHAIN),
        _ => None,
    };
    ai::env_source(p).or(saved)
}

fn tools_section(ui: &mut Ui, t: &mut tools::Tools, editor: &str, terminal: &str) {
    ui.label(RichText::new(format!("{} Tools", icon::TERMINAL)).strong());
    ui.add_space(4.0);
    egui::Grid::new("tools_grid").num_columns(2).min_col_width(LABEL_W).spacing([16.0, 8.0]).show(ui, |ui| {
        ui.label("Editor");
        ui.add(TextEdit::singleline(&mut t.editor).margin(crate::theme::INPUT_MARGIN).hint_text(editor).desired_width(330.0))
            .on_hover_text("Opens files and the repository (toolbar, ⌘P, file menus). {path} is the file; without it the path is appended. Empty = detected.");
        ui.end_row();
        ui.label("Terminal");
        ui.add(TextEdit::singleline(&mut t.terminal).margin(crate::theme::INPUT_MARGIN).hint_text(terminal).desired_width(330.0))
            .on_hover_text("Started in the repository folder; {path} = the folder. Empty = your platform's terminal.");
        ui.end_row();
    });
    ui.add_space(8.0);
    ui.label(RichText::new("Custom commands").strong());
    ui.label(
        RichText::new("git arguments (run without a shell) or a shell line, run in the repository. Placeholders (quoted for you; leave them outside quotes): {repo} {branch} {sha} {path}. With {path} a command shows in file menus, with {sha} in commit menus, the rest in ⌘P; output goes to a toast and the command log.")
            .small()
            .color(theme::dim(ui)),
    );
    ui.add_space(4.0);
    let mut remove = None;
    for (i, c) in t.commands.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(&mut c.name).margin(crate::theme::INPUT_MARGIN).hint_text("Name").desired_width(130.0));
            ui.add(TextEdit::singleline(&mut c.command).margin(crate::theme::INPUT_MARGIN).hint_text("git push origin HEAD:refs/for/{branch}").code_editor().desired_width(320.0));
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
    theme::canvas_in(&p, r, 10.0, false, f);
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
