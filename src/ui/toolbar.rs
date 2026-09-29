//! Per-repo toolbar: repo / branch status, undo-redo, fetch-pull-push,
//! branch-stash-pop, conflict and fetch-failure chips, progress and cancel, and
//! the embedded terminal toggle and the open-in editor / terminal / file manager menu.

use crate::ui::format::keys;
use crate::ai;
use crate::git;
use crate::repo::maintenance;
use crate::repo::{Action, Op, Prompt, PullMode, RepoTab, Undo, fetch_op, op, pop_stash, pull_op, stash_prompt};
use crate::theme::{self, icon};
use crate::ui::settings::Settings;
use egui::{Button, RichText, Ui};

pub(super) fn toolbar(ui: &mut Ui, t: &RepoTab, s: &mut Settings, acts: &mut Vec<Action>) {
    ui.spacing_mut().item_spacing.x = 10.0;
    theme::bar_h(ui, 36.0, |ui| {
        theme::group(ui, |ui| {
            // Right to left: the overflow menu sits at the far right.
            if let Some(d) = &t.data {
                ui.menu_button(icon::DOTS_THREE, |ui| history_tools(ui, d, t.busy().is_some(), acts)).response.on_hover_text("History tools");
            }
            if theme::pill(ui, icon::ARROWS_CLOCKWISE).on_hover_text("Refresh (F5)").clicked() {
                acts.push(Action::Reload);
            }
            if (s.terminal || !t.term.tabs.is_empty()) && theme::pill(ui, icon::TERMINAL).on_hover_text(keys("Terminal (⌃`)")).clicked() {
                acts.push(Action::ToggleTerminal);
            }
            ui.menu_button(icon::ARROW_SQUARE_OUT, |ui| open_items(ui, acts)).response.on_hover_text("Open in editor, terminal or file manager");
            if let Some(web) = t.data.as_ref().and_then(|d| d.web())
                && theme::pill(ui, icon::GLOBE).on_hover_text(format!("Open {web}")).clicked()
            {
                acts.push(Action::Url(web));
            }
        });
        theme::rest(ui, |ui| {
            // Leave the controls after the name their width (as measured last frame), so a
            // long branch name is cut instead of pushing them over the right-hand group.
            let id = ui.id().with("controls");
            let room = ui.available_width() - ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(0.0);
            // Too narrow for the labelled controls (as measured the last time they had
            // labels): drop the labels and keep the icons, which have hover texts.
            let compact = ui.available_width() - ui.data(|d| d.get_temp::<f32>(id.with("full"))).unwrap_or(0.0) < 130.0;
            ui.scope(|ui| {
                ui.set_max_width(room.max(120.0));
                theme::group(ui, |ui| {
                    ui.add_space(6.0);
                    ui.label(RichText::new(&t.name).strong().size(14.5));
                    if let Some(d) = &t.data {
                        let st = &d.status;
                        let branch = st.branch.clone().unwrap_or_else(|| {
                            format!("detached @ {}", st.head.as_deref().map_or("—", |h| &h[..h.len().min(7)]))
                        });
                        let mut text = format!("{}  {branch}", icon::GIT_BRANCH);
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
            });
            let x0 = ui.cursor().min.x;
            let busy = t.busy().is_some();
            ui.add_enabled_ui(!busy && t.data.is_some(), |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend); // measured at full size, never cut
                theme::group(ui, |ui| {
                    let undo = ui.add_enabled(t.undo_label().is_some(), Button::new(label(compact, icon::ARROW_COUNTER_CLOCKWISE, "Undo")).frame_when_inactive(false).corner_radius(255));
                    if undo.on_hover_text(t.undo_label().map_or("Nothing to undo".into(), |l| keys(&format!("Undo {l} (⌘Z)")))).clicked() {
                        acts.push(Action::Undo);
                    }
                    let redo = ui.add_enabled(t.redo_label().is_some(), Button::new(label(compact, icon::ARROW_CLOCKWISE, "Redo")).frame_when_inactive(false).corner_radius(255));
                    if redo.on_hover_text(t.redo_label().map_or("Nothing to redo".into(), |l| keys(&format!("Redo {l} (⇧⌘Z)")))).clicked() {
                        acts.push(Action::Redo);
                    }
                });
                theme::group(ui, |ui| toolbar_remote(ui, t, s, compact, acts));
                theme::group(ui, |ui| toolbar_local(ui, t.data.as_ref().and_then(|d| d.stashes.first()), compact, acts));
            });
            if let Some((target, files)) = t.conflicts.as_ref().filter(|c| !c.1.is_empty()) {
                let chip = RichText::new(format!("{}  {} conflict{} with {target}", icon::WARNING, files.len(), if files.len() == 1 { "" } else { "s" }))
                    .color(theme::MOD);
                theme::group(ui, |ui| ui.label(chip))
                    .on_hover_text(format!("Merging {target} now would conflict in:\n{}", files.join("\n")));
            }
            if let Some(e) = &t.fetch_failed {
                let chip = RichText::new(format!("{}  {}", icon::CLOUD_WARNING, git::fetch_failure(e))).color(theme::MOD);
                let r = theme::group(ui, |ui| theme::pill(ui, chip)).on_hover_text(format!("Background fetch failed:\n{}\n\nClick to fetch now (asks for credentials if needed).", e.trim()));
                if r.clicked() && !busy {
                    acts.push(Action::Git(fetch_op()));
                }
            }
            if let Some(label) = t.busy() {
                ui.spinner();
                theme::progress(ui, &format!("{label}…"), t.progress());
                if t.cancellable() && theme::group(ui, |ui| theme::pill(ui, format!("{}  Cancel", icon::STOP_CIRCLE))).clicked() {
                    acts.push(Action::Cancel);
                }
            } else if t.loading() {
                ui.spinner();
            }
            let w = ui.cursor().min.x - x0;
            ui.data_mut(|d| {
                d.insert_temp(id, w);
                if !compact {
                    d.insert_temp(id.with("full"), w);
                }
            });
        });
    });
}

/// Hand the repository to the user's own tools (also in ⌘P).
fn open_items(ui: &mut Ui, acts: &mut Vec<Action>) {
    for (label, a) in [
        (format!("{}  Open in editor", icon::CODE), Action::Editor(String::new())),
        (format!("{}  Open terminal here", icon::TERMINAL), Action::Terminal),
        (format!("{}  Show in file manager", icon::FOLDER_OPEN), Action::Reveal(String::new())),
    ] {
        if ui.button(label).clicked() {
            acts.push(a);
        }
    }
    ui.label(RichText::new("Set the editor and terminal in Settings → Tools").small().color(theme::dim(ui)));
}

fn history_tools(ui: &mut Ui, d: &crate::repo::RepoData, busy: bool, acts: &mut Vec<Action>) {
    if ui.button(format!("{}  Reflog — recover lost commits", icon::LIFEBUOY)).clicked() {
        acts.push(Action::Reflog);
    }
    if let Some((label, a)) = crate::repo::spinoff::move_unpushed(d)
        && ui.button(format!("{}  {label}", icon::GIT_FORK)).clicked()
    {
        acts.push(a);
    }
    if ui.button(format!("{}  Apply patch…", icon::BANDAIDS)).clicked() {
        acts.push(Action::ApplyPatch);
    }
    if let Some(remote) = d.default_remote()
        && ui.button(format!("{}  Push all tags to {remote}", icon::TAG)).clicked()
    {
        acts.push(Action::Git(crate::repo::tags::push_all(remote)));
    }
    // Never while something else is running or a merge / rebase is half done.
    let idle = !busy && d.state == git::State::Clean;
    let tip = "Wait for the running operation, or finish the merge / rebase in progress";
    let maintain = ui.add_enabled(idle, Button::new(format!("{}  Perform repo maintenance", icon::WRENCH)));
    if maintain.on_hover_text("Pack loose objects and refresh the commit-graph (git maintenance)").on_disabled_hover_text(tip).clicked() {
        acts.push(Action::Git(maintenance::op()));
        ui.close();
    }
    let on = d.health.scheduled;
    let sched = ui.add_enabled(idle, Button::new(format!("{}  {} background maintenance", icon::CLOCK, if on { "Disable" } else { "Enable" })));
    if sched.on_hover_text("Let git tidy this repository on a schedule (git maintenance start)").on_disabled_hover_text(tip).clicked() {
        acts.push(Action::Git(maintenance::schedule_op(!on)));
        ui.close();
    }
    if d.lfs.used {
        ui.separator();
        if !d.lfs.installed {
            ui.label(RichText::new(format!("{}  {}", icon::WARNING, crate::repo::LFS_MISSING)).small().color(theme::MOD));
            return;
        }
        ui.menu_button(format!("{}  Git LFS", icon::DATABASE), |ui| {
            for (label, tip, args) in [
                ("Pull LFS objects", "Download the LFS files of the current checkout", &["lfs", "pull"][..]),
                ("Fetch LFS objects", "Download LFS files without updating the working tree", &["lfs", "fetch"]),
                ("Prune old LFS objects", "Delete local LFS files that are pushed and no longer checked out", &["lfs", "prune"]),
                ("Install LFS hooks", "Set up git-lfs hooks for this repository", &["lfs", "install", "--local"]),
            ] {
                if ui.button(label).on_hover_text(tip).clicked() {
                    acts.push(op(label, args));
                    ui.close();
                }
            }
        });
    }
}

fn toolbar_remote(ui: &mut Ui, t: &RepoTab, s: &mut Settings, compact: bool, acts: &mut Vec<Action>) {
    {
        {
            if theme::pill(ui, label(compact, icon::CLOUD_ARROW_DOWN, "Fetch")).on_hover_text("Fetch all remotes and prune").clicked() {
                acts.push(Action::Git(fetch_op()));
            }
            if theme::pill(ui, label(compact, icon::ARROW_LINE_DOWN, "Pull")).on_hover_text(format!("{:?} (with autostash)", s.pull)).clicked() {
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
            // No remote yet: Push starts publishing (add a remote) instead of doing nothing.
            let no_remote = t.data.as_ref().filter(|d| d.remotes.is_empty());
            let push = theme::pill(ui, label(compact, icon::ARROW_LINE_UP, "Push"));
            let push = match no_remote {
                Some(_) => push.on_hover_text("No remote yet: add one (e.g. a new empty repository on GitHub) to publish this branch"),
                None => push.on_hover_text("Push the current branch (sets its upstream on first push)"),
            };
            if push.clicked() {
                match no_remote {
                    Some(d) => acts.push(Action::RemoteForm(crate::repo::remotes::Form::add(d))),
                    None => acts.extend(t.push_op(false).map(Action::Git)),
                }
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

/// A toolbar button's text: icon and word, or the icon alone in a narrow window.
fn label(compact: bool, icon: &str, text: &str) -> String {
    if compact { icon.into() } else { format!("{icon}  {text}") }
}

fn toolbar_local(ui: &mut Ui, newest: Option<&git::Stash>, compact: bool, acts: &mut Vec<Action>) {
    {
        {
            if theme::pill(ui, label(compact, icon::GIT_BRANCH, "Branch")).on_hover_text("Create a branch from HEAD and check it out").clicked() {
                acts.push(Action::Prompt(Prompt {
                    title: "New branch from HEAD".into(),
                    value: String::new(),
                    op: Op::new("Create branch", &["checkout", "-b", "{}"]).undo(Undo::Keep),
                second: None,
                sign: None,
                }));
            }
            if theme::pill(ui, label(compact, icon::TRAY_ARROW_DOWN, "Stash")).on_hover_text(keys("Stash all changes, untracked files included (⌘Z pops it back)")).clicked() {
                acts.push(stash_prompt());
            }
            ui.menu_button(icon::CARET_DOWN, |ui| {
                if ai::enabled() && ui.button(format!("{}  Stash with message", icon::SPARKLE)).clicked() {
                    acts.push(Action::Ai(ai::Purpose::StashMessage));
                    ui.close();
                }
                if ui.button("Stash staged changes only").clicked() {
                    acts.push(op("Stash staged", &["stash", "push", "--staged"]));
                    ui.close();
                }
            }).response.on_hover_text("More stash options");
            let pop = ui.add_enabled_ui(newest.is_some(), |ui| theme::pill(ui, label(compact, icon::TRAY_ARROW_UP, "Pop"))).inner;
            if let Some(s) = newest.filter(|_| pop.on_hover_text(keys("Apply the newest stash and drop it (⌘Z puts it back)")).on_disabled_hover_text("No stashes").clicked()) {
                acts.push(Action::Git(pop_stash(s)));
            }
        }
    }
}
