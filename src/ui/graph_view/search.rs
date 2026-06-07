//! The ⌘F search bar above the graph. The query and hits live in `crate::repo::Search`.

use crate::repo::{Action, RepoData, Search, Sel};
use crate::theme::{self, icon};
use egui::Ui;

/// Search bar; returns true when it should close.
pub(super) fn search_bar(ui: &mut Ui, s: &mut Search, d: &RepoData, scroll_to: &mut Option<usize>, acts: &mut Vec<Action>) -> bool {
    s.update(d);
    let mut close = false;
    let mut step = 0i32;
    // Buttons and count first: a narrow graph pane shrinks the field, never the other way round.
    theme::bar(ui, |ui| {
        if theme::pill(ui, icon::X).on_hover_text("Close (Esc)").clicked() {
            close = true;
        }
        if theme::pill(ui, icon::CARET_DOWN).on_hover_text("Next (↩)").clicked() {
            step = 1;
        }
        if theme::pill(ui, icon::CARET_UP).on_hover_text("Previous (⇧↩)").clicked() {
            step = -1;
        }
        let n = s.hits.len();
        let label = if s.query.trim().is_empty() { String::new() } else if n == 0 { "No matches".into() } else { format!("{} of {n}", s.cur + 1) };
        ui.label(egui::RichText::new(label).small().color(theme::dim(ui)));
        theme::rest(ui, |ui| {
            ui.label(egui::RichText::new(icon::MAGNIFYING_GLASS).color(theme::dim(ui)));
            let te = ui.add(
                egui::TextEdit::singleline(&mut s.query).hint_text("Search commits: message, author, SHA").desired_width(ui.available_width().min(320.0)),
            );
            if std::mem::take(&mut s.focus) {
                te.request_focus();
            }
            let (enter, shift, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.modifiers.shift, i.key_pressed(egui::Key::Escape)));
            if te.lost_focus() && enter {
                step = if shift { -1 } else { 1 };
                te.request_focus();
            }
            if esc {
                close = true;
            }
        });
    });
    if step != 0 && !s.hits.is_empty() {
        let n = s.hits.len() as i32;
        s.cur = (s.cur as i32 + step).rem_euclid(n) as usize;
        let row = s.hits[s.cur];
        *scroll_to = Some(row);
        acts.extend(d.commit(row).map(|c| Action::Select(Sel::Commit(c.id.clone()))));
    }
    close
}
