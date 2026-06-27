//! Commit Composer dialog: Claude's commit plan, editable (messages, moving
//! changes between commits, split / merge / reorder), then Create commits.

use crate::repo::{Action, RepoTab};
use crate::theme::{self, icon};
use egui::{Button, Frame, Id, Key, RichText, TextEdit};

enum Edit {
    Move(usize, Option<usize>),
    MergeUp(usize),
    Swap(usize),
}

pub(super) fn compose_dialog(t: &mut RepoTab, ctx: &egui::Context, acts: &mut Vec<Action>) {
    let busy = t.busy().is_some();
    let Some(p) = t.compose.as_mut().filter(|p| !p.commits.is_empty()) else { return };
    let (mut close, mut run, mut edit) = (false, false, None);
    egui::Modal::new(Id::new("compose")).show(ctx, |ui| {
        ui.set_width(680.0);
        ui.heading(format!("{} Compose commits", icon::SPARKLE));
        ui.label(
            RichText::new(format!("{}'s plan for your uncommitted changes, oldest commit first. Edit the messages, move changes between commits, then create them. Undo with ⌘Z.", crate::ai::name()))
                .small()
                .color(theme::dim(ui)),
        );
        if let Some(n) = &p.note {
            ui.label(RichText::new(format!("{} {n}", icon::WARNING)).small().color(theme::MOD));
        }
        ui.add_space(6.0);
        let n = p.commits.len();
        // Short names for the move menus, before the messages are borrowed for editing.
        let names: Vec<String> = p.commits.iter().enumerate().map(|(k, c)| format!("{}. {}", k + 1, c.message.lines().next().unwrap_or(""))).collect();
        egui::ScrollArea::vertical().max_height(480.0).auto_shrink([false, true]).show(ui, |ui| {
            for (k, c) in p.commits.iter_mut().enumerate() {
                Frame::new().fill(theme::faint(ui)).corner_radius(12).inner_margin(10).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{} Commit {}", icon::GIT_COMMIT, k + 1)).strong());
                        let s = if c.units.len() == 1 { "" } else { "s" };
                        ui.label(RichText::new(format!("{} change{s}", c.units.len())).small().color(theme::dim(ui)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if k > 0 && theme::pill(ui, icon::ARROWS_MERGE).on_hover_text("Merge into the commit above").clicked() {
                                edit = Some(Edit::MergeUp(k));
                            }
                            if k + 1 < n && theme::pill(ui, icon::ARROW_DOWN).on_hover_text("Move down").clicked() {
                                edit = Some(Edit::Swap(k));
                            }
                            if k > 0 && theme::pill(ui, icon::ARROW_UP).on_hover_text("Move up").clicked() {
                                edit = Some(Edit::Swap(k - 1));
                            }
                        });
                    });
                    ui.add(TextEdit::multiline(&mut c.message).margin(crate::theme::INPUT_MARGIN).hint_text("Commit message").desired_rows(1).desired_width(f32::INFINITY));
                    for &u in &c.units {
                        let unit = &p.units[u];
                        ui.horizontal(|ui| {
                            ui.menu_button(RichText::new(icon::ARROWS_DOWN_UP).small(), |ui| {
                                for (j, name) in names.iter().enumerate().filter(|(j, _)| *j != k) {
                                    if ui.add(Button::new(format!("Move to {name}")).truncate()).clicked() {
                                        edit = Some(Edit::Move(u, Some(j)));
                                    }
                                }
                                if c.units.len() > 1 && ui.button(format!("{} Split into a new commit", icon::ARROWS_SPLIT)).clicked() {
                                    edit = Some(Edit::Move(u, None));
                                }
                            })
                            .response
                            .on_hover_text("Move this change to another commit");
                            ui.label(RichText::new(&unit.path).strong());
                            ui.label(RichText::new(&unit.what).small().color(theme::dim(ui)));
                        });
                    }
                });
                ui.add_space(6.0);
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let ready = p.commits.iter().all(|c| !c.message.trim().is_empty());
            let s = if n == 1 { "" } else { "s" };
            if theme::primary(ui, ready && !busy, &format!("Create {n} commit{s}"), 170.0).on_disabled_hover_text("Every commit needs a message").clicked() {
                run = true;
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                close = true;
            }
        });
    });
    match edit {
        Some(Edit::Move(u, to)) => p.move_unit(u, to),
        Some(Edit::MergeUp(k)) => p.merge_up(k),
        Some(Edit::Swap(k)) => p.commits.swap(k, k + 1),
        None => {}
    }
    if run && let Some(d) = &t.data {
        acts.push(p.op(&t.dir, &d.git_dir).map_or_else(Action::Error, Action::Git));
        close = true;
    }
    if close {
        t.compose = None;
    }
}
