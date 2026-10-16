//! The ⌘F search bar above the graph. The query, the git search and its hits live in
//! `crate::repo::Search`.

use crate::ui::format::{esc_pressed_in, keys, view_key};
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
    let (f3, shift) = (view_key(ui, Key::F3), ui.input(|i| i.modifiers.shift));
    let mut step = if f3 { if shift { -1 } else { 1 } } else { 0 };
    // Buttons and count first: a narrow graph pane shrinks the field, never the other way round.
    theme::bar(ui, |ui| {
        // A narrow graph pane: tighter buttons, so the field keeps room for the query.
        if ui.available_width() < 520.0 {
            ui.spacing_mut().button_padding.x = 6.0;
            ui.spacing_mut().item_spacing.x = 4.0;
        }
        if theme::pill(ui, icon::X).on_hover_text("Close (Esc)").clicked() {
            close = true;
        }
        if theme::pill(ui, icon::CARET_DOWN).on_hover_text(keys("Next (↩ / F3)")).clicked() {
            step = 1;
        }
        if theme::pill(ui, icon::CARET_UP).on_hover_text(keys("Previous (⇧↩ / ⇧F3)")).clicked() {
            step = -1;
        }
        let n = s.found.len();
        let more = if s.running() { "…" } else if n >= crate::repo::MAX_HITS { "+" } else { "" };
        let label = match &s.error {
            _ if !s.active => String::new(),
            Some(_) => "Invalid search".into(),
            None if n == 0 && s.running() => "Searching…".into(),
            None if n == 0 => "No matches".into(),
            None => format!("{} of {n}{more}", s.cur + 1),
        };
        let l = ui.label(egui::RichText::new(label).small().color(if s.error.is_some() { theme::del() } else { theme::dim(ui) }));
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
                    .id(egui::Id::new(("graph_search", &d.git_dir)))
                    .margin(crate::theme::INPUT_MARGIN)
                    .hint_text("Search commits, or author: path: code: …")
                    .desired_width(ui.available_width().min(320.0)),
            );
            let te = te.on_hover_text(HELP);
            if std::mem::take(&mut s.focus) {
                te.request_focus();
            }
            let (enter, esc) = (ui.input(|i| i.key_pressed(Key::Enter)), esc_pressed_in(ui, te.id));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::testkit::{Sim, tab};

    #[test]
    fn the_field_keeps_its_focus_when_the_history_button_appears() {
        let mut sim = Sim::new(700.0, 100.0);
        let mut t = tab(&sim.ctx, "search-focus", &["one"]);
        let d = t.data.take().unwrap();
        let mut s = Search::open(t.dir.clone());
        let draw = |sim: &mut Sim, s: &mut Search| {
            for _ in 0..2 {
                sim.frame(vec![], |ui| _ = search_bar(ui, s, &d, &mut None, &mut vec![]));
            }
        };
        draw(&mut sim, &mut s);
        let field = sim.ctx.memory(|m| m.focused());
        assert!(field.is_some(), "the field did not take focus");
        s.history = vec!["fix".into()];
        draw(&mut sim, &mut s);
        assert_eq!(sim.ctx.memory(|m| m.focused()), field);
    }
}
