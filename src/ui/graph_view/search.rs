//! The ⌘F search bar above the graph. The query, the git search and its hits live in
//! `crate::repo::Search`.

use crate::repo::{Action, RepoData, Search, Sel};
use crate::theme::{self, icon};
use egui::{Button, Key, Ui};

const HELP: &str = "Searches all history: message, author and SHA. Operators:\n\
author:ann   committer:ann   message:fix   path:src/a.rs\n\
code:parse(  (added or removed)   regex:fo+   (changed lines)\n\
after:2024-01-01   before:\"2 weeks ago\"   merges   no-merges";

/// Search bar; returns true when it should close. `d` is the graph being drawn.
pub(super) fn search_bar(ui: &mut Ui, s: &mut Search, d: &RepoData, scroll_to: &mut Option<usize>, acts: &mut Vec<Action>) -> bool {
    s.rows(d);
    let mut close = false;
    let (f3, shift) = ui.input(|i| (i.key_pressed(Key::F3), i.modifiers.shift));
    let mut step = if f3 { if shift { -1 } else { 1 } } else { 0 };
    // Buttons and count first: a narrow graph pane shrinks the field, never the other way round.
    theme::bar(ui, |ui| {
        if theme::pill(ui, icon::X).on_hover_text("Close (Esc)").clicked() {
            close = true;
        }
        if theme::pill(ui, icon::CARET_DOWN).on_hover_text("Next (↩ / F3)").clicked() {
            step = 1;
        }
        if theme::pill(ui, icon::CARET_UP).on_hover_text("Previous (⇧↩ / ⇧F3)").clicked() {
            step = -1;
        }
        let n = s.found.len();
        let more = if s.running() { "…" } else if n >= crate::repo::MAX_HITS { "+" } else { "" };
        let label = match &s.error {
            _ if s.query.trim().is_empty() => String::new(),
            Some(_) => "Invalid search".into(),
            None if n == 0 && s.running() => "Searching…".into(),
            None if n == 0 => "No matches".into(),
            None => format!("{} of {n}{more}", s.cur + 1),
        };
        let l = ui.label(egui::RichText::new(label).small().color(if s.error.is_some() { theme::DEL } else { theme::dim(ui) }));
        if let Some(e) = &s.error {
            l.on_hover_text(e);
        }
        let toggle = |ui: &mut Ui, on: bool, text: &str, tip: &str| ui.add(Button::selectable(on, text).corner_radius(255)).on_hover_text(tip).clicked();
        if toggle(ui, s.filter, icon::FUNNEL, "Filter graph to results") {
            s.filter = !s.filter;
            s.remember();
        }
        if toggle(ui, s.case, "Aa", "Match case") {
            s.case = !s.case;
        }
        if !s.history.is_empty() {
            ui.menu_button(icon::CLOCK_COUNTER_CLOCKWISE, |ui| {
                for h in s.history.clone() {
                    if ui.button(&h).clicked() {
                        s.query = h;
                        s.focus = true;
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text("Recent searches");
        }
        theme::rest(ui, |ui| {
            ui.label(egui::RichText::new(icon::MAGNIFYING_GLASS).color(theme::dim(ui)));
            let te = ui.add(
                egui::TextEdit::singleline(&mut s.query)
                    .margin(crate::theme::INPUT_MARGIN)
                    .hint_text("Search commits, or author: path: code: …")
                    .desired_width(ui.available_width().min(320.0)),
            );
            let te = te.on_hover_text(HELP);
            if std::mem::take(&mut s.focus) {
                te.request_focus();
            }
            let (enter, esc) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
            if te.lost_focus() && enter {
                step = if shift { -1 } else { 1 };
                te.request_focus();
            }
            if esc {
                close = true;
            }
        });
    });
    if step != 0 && !s.found.is_empty() {
        s.remember();
        let n = s.found.len() as i32;
        s.cur = (s.cur as i32 + step).rem_euclid(n) as usize;
        let id = s.found[s.cur].id.clone();
        // Outside the loaded history: Jump loads enough of it first.
        match d.row_of.get(&id) {
            Some(&row) => {
                *scroll_to = Some(row);
                acts.push(Action::Select(Sel::Commit(id)));
            }
            None => acts.push(Action::Jump(id)),
        }
    }
    close
}
