//! Title bar: the repo tabs (drag to move the window, double-click to zoom)
//! and the global buttons on the right (theme, palette, command log, settings).

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
            if theme::pill(ui, icon::GEAR_SIX).on_hover_text("Settings (⌘,)").clicked() {
                self.open_settings();
            }
            if theme::pill(ui, icon::TERMINAL_WINDOW).on_hover_text("Command log (⇧⌘L)").clicked() {
                self.show_log = !self.show_log;
            }
            if theme::pill(ui, format!("{}  Search commands  ⌘P", icon::MAGNIFYING_GLASS)).on_hover_text("Command palette: commands, branches and repositories (? lists the shortcuts)").clicked() {
                self.palette = Some(Default::default());
            }
            let glyph = if self.s.flavor.dark() { icon::SUN } else { icon::MOON };
            if theme::pill(ui, glyph).on_hover_text("Switch light / dark").clicked() {
                toggle_flavor(&mut self.s, ui.ctx());
            }
            theme::rest(ui, |ui| {
                let mut close = None;
                for (i, t) in self.tabs.iter().enumerate() {
                    let active = i == self.active;
                    let strong = theme::text(ui);
                    let dim = theme::dim(ui);
                    let tab = |ui: &mut Ui| {
                        let name = RichText::new(format!("{} {}", icon::FOLDER_SIMPLE, t.name)).color(if active { strong } else { dim });
                        let r = theme::pill(ui, name).on_hover_text(t.dir.display().to_string());
                        let x = theme::pill(ui, RichText::new(icon::X).small().color(dim)).on_hover_text("Close tab (⌘W)");
                        (r, x.clicked())
                    };
                    let (r, closed) = if active { theme::group(ui, tab) } else { ui.horizontal(tab).inner };
                    if r.clicked() {
                        self.active = i;
                    }
                    if r.middle_clicked() || closed {
                        close = Some(i);
                    }
                    ui.add_space(4.0);
                }
                let on_welcome = self.active == self.tabs.len();
                let tip = if on_welcome { "Open a repository (⌘O)" } else { "New tab (⌘T)" };
                if theme::pill(ui, icon::PLUS).on_hover_text(tip).clicked() {
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
