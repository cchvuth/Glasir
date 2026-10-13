//! Title bar: the repo tabs (drag to move the window, double-click to zoom)
//! and the global buttons on the right (theme, palette, command log, settings).

use crate::ui::format::{dismissed, keys};
use super::app::App;
use super::settings::toggle_flavor;
use crate::theme::{self, icon};
use egui::{Id, RichText, Sense, Ui, ViewportCommand};

impl App {
    pub(super) fn tab_bar(&mut self, ui: &mut Ui) {
        let drag = ui.interact(ui.max_rect(), Id::new("titlebar"), Sense::click_and_drag());
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if drag.double_clicked() {
            let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
            ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!max));
        }
        theme::bar_h(ui, 30.0, |ui| {
            ui.set_clip_rect(ui.max_rect().expand(40.0));
            if theme::pill(ui, icon::GEAR_SIX).on_hover_text(keys("Settings (⌘,)")).clicked() {
                self.open_settings();
            }
            if theme::pill(ui, icon::TERMINAL_WINDOW).on_hover_text(keys("Command log (⇧⌘L)")).clicked() {
                self.show_log = !self.show_log;
            }
            if theme::pill(ui, keys(&format!("{}  Search commands  ⌘P", icon::MAGNIFYING_GLASS))).on_hover_text("Command palette: commands, branches and repositories (? lists the shortcuts)").clicked() {
                self.palette = Some(Default::default());
            }
            let glyph = if self.s.flavor.dark() { icon::SUN } else { icon::MOON };
            if theme::pill(ui, glyph).on_hover_text("Switch light / dark").clicked() {
                toggle_flavor(&mut self.s, ui.ctx());
            }
            theme::rest(ui, |ui| {
                let (mut close, mut others, mut drag) = (None, None, None);
                let (mut rects, mut ends) = (vec![], vec![]);
                // Tabs share the width left of the global buttons (labels cut with "…", the
                // "+" kept clear), so long branch names never push tabs off the bar.
                let w = ((ui.available_width() - 40.0) / self.tabs.len().max(1) as f32 - 58.0).clamp(0.0, 260.0);
                for (i, t) in self.tabs.iter().enumerate() {
                    let active = i == self.active;
                    let dim = theme::dim(ui);
                    let alias = self.s.aliases.get(&t.dir);
                    let tab = |ui: &mut Ui| {
                        let label = tab_label(ui, t, alias, active);
                        let r = ui.scope(|ui| {
                            ui.set_max_width(w);
                            ui.add(egui::Button::new(label.clone()).frame_when_inactive(false).corner_radius(255).sense(Sense::click_and_drag()))
                        });
                        let r = r.inner.on_hover_text(format!("{}\n{}", label.text, t.dir.display()));
                        let x = theme::pill(ui, RichText::new(icon::X).small().color(dim)).on_hover_text(keys("Close tab (⌘W)"));
                        (r, x.clicked())
                    };
                    let (r, closed) = if active { theme::group(ui, tab) } else { ui.horizontal(tab).inner };
                    rects.push(r.rect);
                    if r.clicked() {
                        self.active = i;
                    }
                    if r.middle_clicked() || closed {
                        close = Some(i);
                    }
                    if r.dragged() || r.drag_stopped() {
                        drag = Some((i, r.drag_stopped()));
                    }
                    r.context_menu(|ui| {
                        if ui.button("Rename…").clicked() {
                            self.rename = Some((t.dir.clone(), alias.cloned().unwrap_or_else(|| t.name.clone())));
                            ui.close();
                        }
                        if ui.add_enabled(self.tabs.len() > 1, egui::Button::new("Close other tabs")).clicked() {
                            others = Some(i);
                            ui.close();
                        }
                        if ui.button("Reveal in file manager").clicked() {
                            crate::repo::files::reveal(&t.dir);
                            ui.close();
                        }
                    });
                    ends.push(ui.min_rect().right());
                    ui.add_space(4.0);
                }
                let line = egui::Stroke::new(1.0, theme::dim(ui).gamma_multiply(0.35));
                for i in (1..rects.len()).filter(|&i| i != self.active && i - 1 != self.active) {
                    let y = rects[i].center().y;
                    ui.painter().vline((ends[i - 1] + rects[i].left()) / 2.0, y - 7.0..=y + 7.0, line);
                }
                if let (Some((from, dropped)), Some(x)) = (drag, ui.ctx().pointer_latest_pos().map(|p| p.x)) {
                    let (to, at) = move_target(&rects.iter().map(|r| r.center().x).collect::<Vec<_>>(), from, x);
                    if dropped {
                        let active = self.tabs.get(self.active).map(|t| t.dir.clone());
                        let t = self.tabs.remove(from);
                        self.tabs.insert(to, t);
                        if let Some(d) = active {
                            self.active = self.tabs.iter().position(|t| t.dir == d).unwrap_or(self.active);
                        }
                    } else {
                        let r = rects[at.min(rects.len() - 1)];
                        let line = if at == rects.len() { r.right_top() } else { r.left_top() } - egui::vec2(3.0, 0.0);
                        ui.painter().vline(line.x, r.y_range(), egui::Stroke::new(2.0, theme::accent()));
                    }
                }
                if let Some(keep) = others {
                    let dir = self.tabs[keep].dir.clone();
                    for i in (0..self.tabs.len()).rev().filter(|&i| i != keep) {
                        self.close_tab(ui.ctx(), i);
                    }
                    self.active = self.tabs.iter().position(|t| t.dir == dir).unwrap_or(0);
                }
                let on_welcome = self.active == self.tabs.len();
                let tip = if on_welcome { "Open a repository (⌘O)" } else { "New tab (⌘T)" };
                if theme::pill(ui, icon::PLUS).on_hover_text(keys(tip)).clicked() {
                    if on_welcome {
                        self.pick_and_open(ui.ctx()); // already on the new-tab page: go straight to the picker
                    } else {
                        self.active = self.tabs.len();
                    }
                }
                if let Some(i) = close {
                    self.close_tab(ui.ctx(), i);
                }
            });
        });
    }
}

/// "folder name  branch  ● ↑1 ↓2": the alias or folder name, then the loaded branch and state.
fn tab_label(ui: &Ui, t: &crate::repo::RepoTab, alias: Option<&String>, active: bool) -> egui::text::LayoutJob {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let small = egui::FontId::new(font.size * 0.85, font.family.clone());
    let (strong, dim) = (theme::text(ui), theme::dim(ui));
    let mut job = egui::text::LayoutJob::default();
    let mut add = |text: String, color, font: &egui::FontId| job.append(&text, 0.0, egui::TextFormat::simple(font.clone(), color));
    add(format!("{}  {}", icon::FOLDER_SIMPLE, alias.unwrap_or(&t.name)), if active { strong } else { dim }, &font);
    if let Some(s) = t.data.as_ref().map(|d| &d.status) {
        add(format!("  {} {}", icon::GIT_BRANCH, s.branch.as_deref().unwrap_or("detached")), dim, &small);
        if s.is_dirty() {
            add(" ●".into(), theme::changed(), &small);
        }
        if s.ahead > 0 {
            add(format!(" ↑{}", s.ahead), theme::add(), &small);
        }
        if s.behind > 0 {
            add(format!(" ↓{}", s.behind), theme::del(), &small);
        }
    }
    job
}

/// Where a tab dragged from `from` and dropped at `x` lands: (its new index,
/// the slot 0..=len the pointer is over, for the insertion marker).
fn move_target(centers: &[f32], from: usize, x: f32) -> (usize, usize) {
    let slot = centers.iter().filter(|&&c| c < x).count();
    (if slot > from { slot - 1 } else { slot }, slot)
}

#[cfg(test)]
mod tests {
    use super::move_target;

    #[test]
    fn drop_slot_follows_the_pointer() {
        let c = [10.0, 30.0, 50.0];
        assert_eq!(move_target(&c, 0, 40.0), (1, 2)); // past the 2nd tab's centre
        assert_eq!(move_target(&c, 2, 0.0), (0, 0));
        assert_eq!(move_target(&c, 1, 99.0), (2, 3));
        assert_eq!(move_target(&c, 1, 31.0), (1, 2)); // stays put
    }
}

impl App {
    /// Rename-tab dialog; an empty name goes back to the folder name.
    pub(super) fn rename_dialog(&mut self, ctx: &egui::Context) {
        let Some((dir, name)) = &mut self.rename else { return };
        let mut done = None;
        egui::Modal::new(Id::new("rename_tab")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading("Rename tab");
            ui.add(egui::TextEdit::singleline(name).margin(theme::INPUT_MARGIN).desired_width(f32::INFINITY)).request_focus();
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = Some(true);
                }
                if dismissed(ui, "Cancel") {
                    done = Some(false);
                }
            });
        });
        if let Some(ok) = done {
            let (dir, name) = (dir.clone(), name.trim().to_owned());
            self.rename = None;
            if ok && !name.is_empty() {
                self.s.aliases.insert(dir, name);
            } else if ok {
                self.s.aliases.remove(&dir);
            }
        }
    }
}
