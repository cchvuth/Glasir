//! Per-repo toolbar: repo / branch status, undo-redo, fetch-pull-push,
//! branch-stash-pop, progress and cancel.

use crate::repo::{Action, Op, Prompt, PullMode, RepoTab, Undo, fetch_op, op, pull_op, stash_op};
use crate::theme::{self, icon};
use crate::ui::settings::Settings;
use egui::{Align, Button, Layout, RichText, Ui};

pub(super) fn toolbar(ui: &mut Ui, t: &RepoTab, s: &mut Settings, acts: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.set_height(36.0);
        ui.spacing_mut().item_spacing.x = 10.0;
        theme::group(ui, |ui| {
            ui.add_space(6.0);
            ui.label(RichText::new(&t.name).strong().size(14.5));
            if let Some(d) = &t.data {
                let st = &d.status;
                let branch = st.branch.clone().unwrap_or_else(|| {
                    format!("detached @ {}", st.head.as_deref().map_or("—", |h| &h[..h.len().min(7)]))
                });
                let mut text = format!("{} {branch}", icon::GIT_BRANCH);
                if st.ahead > 0 {
                    text += &format!("  {}{}", icon::ARROW_UP, st.ahead);
                }
                if st.behind > 0 {
                    text += &format!("  {}{}", icon::ARROW_DOWN, st.behind);
                }
                ui.label(RichText::new(text).color(theme::dim(ui)));
            }
            ui.add_space(6.0);
        });
        let busy = t.busy().is_some();
        ui.add_enabled_ui(!busy && t.data.is_some(), |ui| {
            theme::group(ui, |ui| {
                let undo = ui.add_enabled(t.undo_label().is_some(), Button::new(format!("{} Undo", icon::ARROW_COUNTER_CLOCKWISE)).frame_when_inactive(false).corner_radius(255));
                if undo.on_hover_text(t.undo_label().map_or("Nothing to undo".into(), |l| format!("Undo {l} (⌘Z)"))).clicked() {
                    acts.push(Action::Undo);
                }
                let redo = ui.add_enabled(t.redo_label().is_some(), Button::new(format!("{} Redo", icon::ARROW_CLOCKWISE)).frame_when_inactive(false).corner_radius(255));
                if redo.on_hover_text(t.redo_label().map_or("Nothing to redo".into(), |l| format!("Redo {l} (⇧⌘Z)"))).clicked() {
                    acts.push(Action::Redo);
                }
            });
            theme::group(ui, |ui| toolbar_remote(ui, t, s, acts));
            theme::group(ui, |ui| toolbar_local(ui, acts));
        });
        if let Some(label) = t.busy() {
            ui.spinner();
            theme::progress(ui, &format!("{label}…"), t.progress());
            if t.cancellable() && theme::group(ui, |ui| theme::pill(ui, format!("{} Cancel", icon::STOP_CIRCLE))).clicked() {
                acts.push(Action::Cancel);
            }
        } else if t.loading() {
            ui.spinner();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            theme::group(ui, |ui| {
                if theme::pill(ui, icon::ARROWS_CLOCKWISE).on_hover_text("Refresh (F5)").clicked() {
                    acts.push(Action::Reload);
                }
                if let Some(web) = t.data.as_ref().and_then(|d| d.web())
                    && theme::pill(ui, icon::GLOBE).on_hover_text(format!("Open {web}")).clicked()
                {
                    acts.push(Action::Url(web));
                }
            });
        });
    });
}

fn toolbar_remote(ui: &mut Ui, t: &RepoTab, s: &mut Settings, acts: &mut Vec<Action>) {
    {
        {
            if theme::pill(ui, format!("{} Fetch", icon::CLOUD_ARROW_DOWN)).on_hover_text("Fetch all remotes and prune").clicked() {
                acts.push(Action::Git(fetch_op()));
            }
            if theme::pill(ui, format!("{} Pull", icon::ARROW_LINE_DOWN)).on_hover_text(format!("{:?} (with autostash)", s.pull)).clicked() {
                acts.push(Action::Git(pull_op(s.pull)));
            }
            ui.menu_button(icon::CARET_DOWN, |ui| {
                for (mode, label) in [
                    (PullMode::Merge, "Fast-forward if possible, else merge"),
                    (PullMode::FfOnly, "Fast-forward only"),
                    (PullMode::Rebase, "Rebase"),
                ] {
                    if ui.radio(s.pull == mode, label).clicked() {
                        s.pull = mode;
                        acts.push(Action::Git(pull_op(mode)));
                        ui.close();
                    }
                }
            });
            if theme::pill(ui, format!("{} Push", icon::ARROW_LINE_UP)).clicked() {
                acts.extend(t.push_op(false).map(Action::Git));
            }
            ui.menu_button(icon::CARET_DOWN, |ui| {
                if ui.button("Force push (with lease)").clicked() {
                    acts.extend(t.push_op(true).map(Action::Git));
                    ui.close();
                }
            });
        }
    }
}

fn toolbar_local(ui: &mut Ui, acts: &mut Vec<Action>) {
    {
        {
            if theme::pill(ui, format!("{} Branch", icon::GIT_BRANCH)).clicked() {
                acts.push(Action::Prompt(Prompt {
                    title: "New branch from HEAD".into(),
                    value: String::new(),
                    op: Op::new("Create branch", &["checkout", "-b", "{}"]).undo(Undo::Keep),
                second: None,
                }));
            }
            if theme::pill(ui, format!("{} Stash", icon::TRAY_ARROW_DOWN)).clicked() {
                acts.push(Action::Git(stash_op()));
            }
            ui.menu_button(icon::CARET_DOWN, |ui| {
                if ui.button("Stash staged changes only").clicked() {
                    acts.push(op("Stash staged", &["stash", "push", "--staged"]));
                    ui.close();
                }
            });
            if theme::pill(ui, format!("{} Pop", icon::TRAY_ARROW_UP)).clicked() {
                acts.push(op("Pop stash", &["stash", "pop"]));
            }
        }
    }
}
