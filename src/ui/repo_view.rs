//! One repository tab: toolbar on top, sidebar left, inspector right and the
//! center pane (graph, or the diff / merge editor / history / rebase editor
//! that replaces it), then the tab's dialogs. Collected `Action`s are applied
//! to the `RepoTab` at the end of the frame.

use super::settings::Settings;
use super::{diff_view, dialogs, graph_view, history, merge_editor, rebase_editor, sidebar, toolbar::toolbar};
use crate::repo::history::Mode;
use crate::repo::{Action, RepoTab};
use crate::theme;
use egui::{Frame, Margin, Ui, UiBuilder};

pub(super) fn show(t: &mut RepoTab, ui: &mut Ui, s: &mut Settings) {
    let ctx = ui.ctx().clone();
    let mut acts = vec![];
    egui::Panel::top("toolbar")
        .frame(Frame::NONE.inner_margin(Margin { left: 14, right: 14, top: 4, bottom: 8 }))
        .show(ui, |ui| toolbar(ui, t, s, &mut acts));
    if t.data.is_none() {
        egui::CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            ui.centered_and_justified(|ui| match &t.error {
                Some(e) => ui.colored_label(theme::DEL, e),
                None => ui.spinner(),
            });
        });
    } else {
        egui::Panel::left("sidebar")
            .default_size(250.0)
            .frame(Frame::NONE.inner_margin(Margin { left: 12, right: 5, top: 2, bottom: 12 }))
            .show(ui, |ui| theme::card(ui, 150, |ui| sidebar::sidebar(ui, t.data.as_ref().unwrap(), &mut t.filter, &mut acts)));
        egui::Panel::right("details")
            .default_size(390.0)
            .frame(Frame::NONE.inner_margin(Margin { left: 5, right: 12, top: 2, bottom: 12 }))
            .show(ui, |ui| theme::card(ui, 150, |ui| t.details(ui, &mut acts)));
        egui::CentralPanel::default()
            .frame(Frame::NONE.inner_margin(Margin { left: 3, right: 3, top: 2, bottom: 12 }))
            .show(ui, |ui| {
                theme::card(ui, 205, |ui| {
                    if let Some(ed) = &mut t.merge {
                        if merge_editor::show(ui, ed, &mut acts) {
                            t.merge = None;
                        }
                        return;
                    }
                    if let Some(fv) = &mut t.file_view {
                        let mut close = false;
                        if fv.mode == Mode::History && t.diff.is_some() {
                            // History list beside the selected revision's diff.
                            let r = ui.max_rect();
                            let split = (r.width() * 0.38).clamp(280.0, 460.0);
                            let left = egui::Rect::from_min_max(r.min, egui::pos2(r.left() + split, r.bottom()));
                            let right = egui::Rect::from_min_max(egui::pos2(r.left() + split + 12.0, r.top()), r.max);
                            ui.scope_builder(UiBuilder::new().max_rect(left), |ui| close = history::show(ui, fv, &mut acts));
                            ui.scope_builder(UiBuilder::new().max_rect(right), |ui| {
                                diff_view::show(ui, t.diff.as_mut().unwrap(), &mut s.split_diff, &mut acts)
                            });
                        } else {
                            close = history::show(ui, fv, &mut acts);
                        }
                        if close {
                            t.file_view = None;
                            t.diff = None;
                        }
                        return;
                    }
                    if let Some(ed) = &mut t.rebase {
                        let git_dir = t.data.as_ref().unwrap().git_dir.clone();
                        if rebase_editor::show(ui, ed, &git_dir, &mut acts) {
                            t.rebase = None;
                        }
                        return;
                    }
                    match &mut t.diff {
                        Some(ds) => diff_view::show(ui, ds, &mut s.split_diff, &mut acts),
                        None => graph_view::show(ui, t.data.as_ref().unwrap(), &t.sel, &t.multi, &mut t.scroll_to, &mut t.search, &mut acts),
                    }
                })
            });
    }

    dialogs::prompt_dialog(t, &ctx, &mut acts);
    dialogs::confirm_dialog(t, &ctx, &mut acts);
    dialogs::drop_menu(t, &ctx, &mut acts);
    dialogs::cleanup_dialog(t, &ctx, &mut acts);
    dialogs::choice_dialog(t, &ctx, &mut acts);
    for a in acts {
        if let Action::View(v) = &a {
            if *v == Default::default() {
                s.views.remove(&t.dir);
            } else {
                s.views.insert(t.dir.clone(), v.clone());
            }
        }
        t.apply(&ctx, a);
    }
}
