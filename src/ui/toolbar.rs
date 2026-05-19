//! Per-repo toolbar: repo / branch status, undo-redo, fetch-pull-push,
//! branch-stash-pop, conflict and fetch-failure chips, progress and cancel.

use crate::ai;
use crate::git;
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
        if let Some((target, files)) = t.conflicts.as_ref().filter(|c| !c.1.is_empty()) {
            let chip = RichText::new(format!("{} {} conflict{} with {target}", icon::WARNING, files.len(), if files.len() == 1 { "" } else { "s" }))
                .color(theme::MOD);
            theme::group(ui, |ui| ui.label(chip))
                .on_hover_text(format!("Merging {target} now would conflict in:\n{}", files.join("\n")));
        }
        if let Some(e) = &t.fetch_failed {
            let chip = RichText::new(format!("{} {}", icon::CLOUD_WARNING, git::fetch_failure(e))).color(theme::MOD);
            let r = theme::group(ui, |ui| theme::pill(ui, chip)).on_hover_text(format!("Background fetch failed:\n{}\n\nClick to fetch now (asks for credentials if needed).", e.trim()));
            if r.clicked() && !busy {
                acts.push(Action::Git(fetch_op()));
            }
        }
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
                // Right to left: the overflow menu sits at the far right.
                if let Some(d) = &t.data {
                    ui.menu_button(icon::DOTS_THREE, |ui| history_tools(ui, d, acts)).response.on_hover_text("History tools");
                }
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

/// The toolbar's overflow menu: reflog, spin-off, patches, tags.
fn history_tools(ui: &mut Ui, d: &crate::repo::RepoData, acts: &mut Vec<Action>) {
    if ui.button(format!("{} Reflog — recover lost commits", icon::LIFEBUOY)).clicked() {
        acts.push(Action::Reflog);
    }
    if let Some((label, a)) = crate::repo::spinoff::move_unpushed(d)
        && ui.button(format!("{} {label}", icon::GIT_FORK)).clicked()
    {
        acts.push(a);
    }
    if ui.button(format!("{} Apply patch…", icon::BANDAIDS)).clicked() {
        acts.push(Action::ApplyPatch);
    }
    if let Some((remote, _)) = d.remotes.first()
        && ui.button(format!("{} Push all tags to {remote}", icon::TAG)).clicked()
    {
        acts.push(Action::Git(crate::repo::tags::push_all(remote)));
    }
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
                ui.label(RichText::new("Pull with").small().color(theme::dim(ui)));
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
            }).response.on_hover_text("More pull options");
            if theme::pill(ui, format!("{} Push", icon::ARROW_LINE_UP)).on_hover_text("Push the current branch (sets its upstream on first push)").clicked() {
                acts.extend(t.push_op(false).map(Action::Git));
            }
            ui.menu_button(icon::CARET_DOWN, |ui| {
                if ui.button("Force push (with lease)").on_hover_text("Overwrite the remote branch, but only if it is still where you last saw it").clicked() {
                    acts.extend(t.push_op(true).map(Action::Git));
                    ui.close();
                }
            }).response.on_hover_text("More push options");
        }
    }
}

fn toolbar_local(ui: &mut Ui, acts: &mut Vec<Action>) {
    {
        {
            if theme::pill(ui, format!("{} Branch", icon::GIT_BRANCH)).on_hover_text("Create a branch from HEAD and check it out").clicked() {
                acts.push(Action::Prompt(Prompt {
                    title: "New branch from HEAD".into(),
                    value: String::new(),
                    op: Op::new("Create branch", &["checkout", "-b", "{}"]).undo(Undo::Keep),
                second: None,
                }));
            }
            if theme::pill(ui, format!("{} Stash", icon::TRAY_ARROW_DOWN)).on_hover_text("Stash all changes, untracked files included").clicked() {
                acts.push(Action::Git(stash_op()));
            }
            ui.menu_button(icon::CARET_DOWN, |ui| {
                if ui.button(format!("{} Stash with AI message", icon::SPARKLE)).clicked() {
                    acts.push(Action::Ai(ai::Purpose::StashMessage));
                    ui.close();
                }
                if ui.button("Stash staged changes only").clicked() {
                    acts.push(op("Stash staged", &["stash", "push", "--staged"]));
                    ui.close();
                }
            }).response.on_hover_text("More stash options");
            if theme::pill(ui, format!("{} Pop", icon::TRAY_ARROW_UP)).on_hover_text("Apply the newest stash and drop it").clicked() {
                acts.push(op("Pop stash", &["stash", "pop"]));
            }
        }
    }
}
