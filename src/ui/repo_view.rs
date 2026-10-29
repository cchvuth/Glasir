//! One repository tab: toolbar, sidebar left, inspector right, and the center pane (graph, or
//! the diff / merge editor / history / rebase editor that replaces it) over the terminal.
//! Dialogs follow. Collected `Action`s are applied to the `RepoTab` at the end of the frame.

use super::settings::Settings;
use super::{diff_view, dialogs, file_edit, graph_view, history, merge_editor, rebase_editor, reflog, sidebar, toolbar::toolbar};
use crate::repo::history::Mode;
use crate::repo::{Action, RepoData, RepoTab, maintenance};
use crate::theme::{self, icon};
use egui::{Frame, Margin, Ui, UiBuilder};

pub(super) fn show(t: &mut RepoTab, ui: &mut Ui, s: &mut Settings) {
    let ctx = ui.ctx().clone();
    graph_view::magic::note_input(ui);
    let mut acts = vec![];
    let busy = t.busy().is_some();
    t.ai_cfg.clone_from(&s.ai);
    t.email_rules.clone_from(&s.email_rules);
    t.keep_branches.clone_from(&s.keep_branches);
    crate::ai::sync(&s.ai);
    egui::Panel::top("toolbar")
        .show_separator_line(false)
        .frame(Frame::NONE.inner_margin(Margin { left: 14, right: 14, top: 4, bottom: 8 }))
        .show(ui, |ui| toolbar(ui, t, s, &mut acts));
    if t.data.is_none() {
        egui::CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            ui.centered_and_justified(|ui| match &t.error {
                Some(e) => ui.colored_label(theme::del(), e),
                None => ui.spinner(),
            });
        });
    } else {
        t.refresh_diff_theme(ui.ctx());
        let budget =(ui.available_width() - 440.0).max(480.0);
        egui::Panel::left("sidebar")
            .show_separator_line(false)
            .default_size(250.0)
            .size_range(180.0..=(budget * 0.4).clamp(180.0, 520.0))
            .frame(Frame::NONE.inner_margin(Margin { left: 12, right: 5, top: 2, bottom: 12 }))
            .show(ui, |ui| theme::card(ui, 150, |ui| sidebar::sidebar(ui, t.data.as_ref().unwrap(), &t.dir, &t.sel, &mut t.filter, &mut acts)));
        egui::Panel::right("details")
            .show_separator_line(false)
            .default_size(390.0)
            .size_range(300.0..=(budget * 0.6).clamp(300.0, 720.0))
            .frame(Frame::NONE.inner_margin(Margin { left: 5, right: 12, top: 2, bottom: 12 }))
            .show(ui, |ui| theme::card(ui, 150, |ui| t.details(ui, &mut s.file_tree, &mut acts)));
        if t.term.open {
            egui::Panel::bottom("terminal")
                .show_separator_line(false)
                .resizable(true)
                .default_size(260.0)
                .size_range(110.0..=900.0)
                .frame(Frame::NONE.inner_margin(Margin { left: 3, right: 3, top: 0, bottom: 12 }))
                .show(ui, |ui| theme::card(ui, 205, |ui| super::terminal::show(ui, &mut t.term, &t.dir, s.terminal)));
        }
        egui::CentralPanel::default()
            .frame(Frame::NONE.inner_margin(Margin { left: 3, right: 3, top: 2, bottom: 12 }))
            .show(ui, |ui| {
                theme::card(ui, 205, |ui| {
                    let tool_open = t.merge.as_ref().is_some_and(|e| t.merge_tool_open(&e.path));
                    if let Some(ed) = &mut t.merge {
                        if merge_editor::show(ui, ed, t.ai.as_ref(), tool_open, &mut s.wrap, &mut acts) {
                            t.merge = None;
                        }
                        return;
                    }
                    if let Some(fv) = &mut t.file_view {
                        let mut close = false;
                        if matches!(fv.mode, Mode::History(_) | Mode::Lines { .. }) && t.diff.is_some() {
                            // History list beside the selected revision's diff.
                            let r = ui.max_rect();
                            let split = (r.width() * 0.38).clamp(280.0, 460.0);
                            let left = egui::Rect::from_min_max(r.min, egui::pos2(r.left() + split, r.bottom()));
                            let right = egui::Rect::from_min_max(egui::pos2(r.left() + split + 12.0, r.top()), r.max);
                            ui.scope_builder(UiBuilder::new().max_rect(left), |ui| close = history::show(ui, fv, &t.avatars, &mut s.wrap, &mut acts));
                            ui.scope_builder(UiBuilder::new().max_rect(right), |ui| {
                                diff_view::show(ui, t.diff.as_mut().unwrap(), t.data.as_ref(), &mut s.split_diff, &mut s.wrap, &mut acts)
                            });
                        } else {
                            close = history::show(ui, fv, &t.avatars, &mut s.wrap, &mut acts);
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
                    if let Some(ed) = &mut t.edit {
                        file_edit::show(ui, ed, &t.dir, &mut s.wrap, &mut acts);
                        return;
                    }
                    // A diff opened from the inspector covers the reflog; closing it goes back there.
                    match (&mut t.diff, &mut t.reflog) {
                        (Some(ds), _) => diff_view::show(ui, ds, t.data.as_ref(), &mut s.split_diff, &mut s.wrap, &mut acts),
                        (None, Some(rl)) => {
                            if reflog::show(ui, rl, t.data.as_ref().unwrap(), &mut acts) {
                                t.reflog = None;
                            }
                        }
                        (None, None) => {
                            let d = t.data.as_ref().unwrap();
                            optimize_hint(ui, d, &t.dir, busy, s, &mut acts);
                            if let Some(se) = &mut t.search {
                                se.history.clone_from(&s.search_history);
                                se.poll(&ctx, d);
                            }
                            graph_view::show(ui, d, &t.sel, &t.multi, &mut t.scroll_to, &mut t.search, &t.avatars, s.magic, &mut acts);
                            if let Some(se) = &t.search
                                && se.history != s.search_history
                            {
                                s.search_history.clone_from(&se.history);
                            }
                        }
                    }
                })
            });
    }

    dialogs::prompt_dialog(t, &ctx, &mut acts);
    dialogs::confirm_dialog(t, &ctx, &mut acts);
    dialogs::ai_dialog(t, &ctx, &mut acts);
    dialogs::drop_menu(t, &ctx, &mut acts);
    dialogs::commit_drop_menu(t, &ctx, &mut acts);
    dialogs::cleanup_dialog(t, &ctx, &mut acts, &mut s.stale_days);
    dialogs::absorb_dialog(t, &ctx, &mut acts);
    super::compose::compose_dialog(t, &ctx, &mut acts);
    dialogs::choice_dialog(t, &ctx, &mut acts);
    sidebar::remotes::form_dialog(t, &ctx, &mut acts);
    super::doctor::doctor_dialog(t, &ctx, &mut acts);
    for a in acts {
        let view = matches!(a, Action::View(_));
        t.apply(&ctx, a);
        if view && t.view == Default::default() {
            s.views.remove(&t.dir);
        } else if view {
            s.views.insert(t.dir.clone(), t.view.clone());
        }
    }
}

/// One-time strip above the graph when the repo would benefit from `git maintenance`.
fn optimize_hint(ui: &mut Ui, d: &RepoData, dir: &std::path::Path, busy: bool, s: &mut Settings, acts: &mut Vec<Action>) {
    if busy || !d.health.wants_optimizing() || d.state != crate::git::State::Clean || s.optimize_seen.iter().any(|p| p == dir) {
        return;
    }
    let why = match d.health.loose >= maintenance::LOOSE_MAX {
        true => format!("{} loose objects slow git down", d.health.loose),
        false => "No commit-graph: history queries are slower than they need to be".into(),
    };
    Frame::new().fill(theme::accent().gamma_multiply(0.18)).inner_margin(Margin::symmetric(10, 6)).corner_radius(12.0).show(ui, |ui| {
        let mut done = false;
        theme::bar(ui, |ui| {
            theme::group(ui, |ui| {
                done |= theme::pill(ui, icon::X).on_hover_text("Don't suggest this again").clicked();
                if theme::pill(ui, format!("{}  Optimize", icon::WRENCH)).on_hover_text("Pack loose objects and refresh the commit-graph (git maintenance)").clicked() {
                    acts.push(Action::Git(maintenance::op()));
                    done = true;
                }
            });
            theme::rest(ui, |ui| ui.label(format!("{}  Optimize repository: {why}", icon::LIGHTBULB)));
        });
        if done {
            s.optimize_seen.push(dir.to_path_buf());
        }
    });
    ui.add_space(4.0);
}

