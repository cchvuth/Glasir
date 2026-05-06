//! Right-hand inspector: what the graph selection shows. `commit` (commit or
//! compare details and files), `wip` (staging lists + commit box), `multi`
//! (several selected commits). File rows and their shared menu entries live here.

mod commit;
mod multi;
mod wip;

use crate::git::Change;
use crate::repo::history::Mode;
use crate::repo::{Action, RepoTab, Sel};
use crate::theme::{self, icon};
use egui::{RichText, Sense, Ui};

impl RepoTab {
    pub fn details(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        if self.multi.len() > 1 {
            return self.multi_panel(ui, acts);
        }
        match self.sel.clone() {
            Some(Sel::Wip) => self.wip_panel(ui, acts),
            Some(Sel::Commit(_) | Sel::Compare { .. }) => self.commit_panel(ui, acts),
            None => {
                ui.centered_and_justified(|ui| ui.label(RichText::new("Select a commit").color(theme::dim(ui))));
            }
        }
    }
}

/// History/Blame entries shared by every file context menu.
pub fn file_items(ui: &mut Ui, path: &str, acts: &mut Vec<Action>) {
    if ui.button(format!("{} File history", icon::CLOCK_COUNTER_CLOCKWISE)).clicked() {
        acts.push(Action::FileView(path.into(), Mode::History));
    }
    if ui.button(format!("{} Blame", icon::USER_LIST)).clicked() {
        acts.push(Action::FileView(path.into(), Mode::Blame));
    }
}

/// A file row: colored status letter, file name, dimmed directory.
pub fn file_row(ui: &mut Ui, c: &Change, selected: bool) -> egui::Response {
    let (dir, file) = c.path.rsplit_once('/').map_or(("", c.path.as_str()), |(d, f)| (d, f));
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 24.0), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 8.0, theme::selection(ui));
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 8.0, theme::hover(ui));
    }
    let p = ui.painter().with_clip_rect(rect);
    let y = rect.center().y;
    let code = if c.code == '?' { 'A' } else { c.code };
    p.text(egui::pos2(rect.left() + 6.0, y), egui::Align2::LEFT_CENTER, code, egui::FontId::monospace(12.0), theme::change_color(c.code));
    let r = p.text(egui::pos2(rect.left() + 22.0, y), egui::Align2::LEFT_CENTER, file, egui::FontId::proportional(13.0), theme::text(ui));
    if !dir.is_empty() {
        p.text(egui::pos2(r.right() + 6.0, y), egui::Align2::LEFT_CENTER, dir, egui::FontId::proportional(11.5), theme::dim(ui));
    }
    resp.on_hover_text(match &c.old_path {
        Some(o) => format!("{o} → {}", c.path),
        None => c.path.clone(),
    })
}
