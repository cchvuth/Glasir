//! One repository tab: toolbar on top, sidebar left, inspector right and the
//! center pane (graph, or the diff / merge editor / rebase editor
//! that replaces it), then the tab's dialogs. Collected `Action`s are applied
//! to the `RepoTab` at the end of the frame.

use super::settings::Settings;
use super::{diff_view, dialogs, graph_view, merge_editor, rebase_editor, sidebar, toolbar::toolbar};
use crate::repo::RepoTab;
use crate::theme;
use egui::{Frame, Margin, Ui};

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
                    if let Some(ed) = &mut t.rebase {
                        let git_dir = t.data.as_ref().unwrap().git_dir.clone();
                        if rebase_editor::show(ui, ed, &git_dir, &mut acts) {
                            t.rebase = None;
                        }
                        return;
                    }
                    match &mut t.diff {
                        Some(ds) => diff_view::show(ui, ds, &mut s.split_diff, &mut acts),
                        None => graph_view::show(ui, t.data.as_ref().unwrap(), &t.sel, &t.multi, &mut t.scroll_to, &mut acts),
                    }
                })
            });
    }

    dialogs::prompt_dialog(t, &ctx, &mut acts);
    dialogs::confirm_dialog(t, &ctx, &mut acts);
    dialogs::drop_menu(t, &ctx, &mut acts);
    dialogs::choice_dialog(t, &ctx, &mut acts);
    for a in acts {
        t.apply(&ctx, a);
    }
}
