//! The graph of a repository without commits: a "Make your first commit" guide
//! (and how to publish it) under the WIP row.

use crate::ui::format::keys;
use crate::repo::{Action, RepoData};
use crate::theme::{self, icon};
use egui::{Button, RichText, Ui};

pub(super) fn first_commit_guide(ui: &mut Ui, d: &RepoData, acts: &mut Vec<Action>) {
    ui.vertical_centered(|ui| {
        ui.set_max_width((ui.available_width() - 24.0).min(440.0));
        theme::empty_state(ui, icon::ROCKET_LAUNCH, "Make your first commit", "This repository has no commits yet. Three steps:");
        let step = |ui: &mut Ui, n: &str, text: &str| {
            ui.add(egui::Label::new(RichText::new(format!("{n}  {text}")).color(theme::text(ui))).wrap());
        };
        step(ui, "1", "Add or create files in the repository folder");
        let mut buttons = |ui: &mut Ui| {
            if ui.add(Button::new(format!("{} Show in file manager", icon::FOLDER_OPEN))).clicked() {
                acts.push(Action::Reveal(String::new()));
            }
            if ui.add(Button::new(format!("{} Open in editor", icon::CODE))).clicked() {
                acts.push(Action::Editor(String::new()));
            }
        };
        // Side by side when they fit, else stacked (a wider row would push the guide off-centre).
        if ui.available_width() >= 330.0 {
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 330.0) / 2.0);
                buttons(ui);
            });
        } else {
            buttons(ui);
        }
        ui.add_space(4.0);
        step(ui, "2", &keys("Stage them on the right: double-click a file, or Stage all (⇧⌘S)"));
        step(ui, "3", &keys("Write a summary and press Commit (⌘↩)"));
        if d.remotes.is_empty() {
            ui.add_space(14.0);
            ui.label(RichText::new(format!("{} Publish it", icon::CLOUD_ARROW_UP)).strong());
            ui.add(
                egui::Label::new(
                    RichText::new("Create an empty repository on GitHub or GitHub Enterprise (no README), copy its URL, add it as a remote, then Push.")
                        .small()
                        .color(theme::dim(ui)),
                )
                .wrap(),
            );
            ui.add_space(4.0);
            if ui.button(format!("{} Add remote…", icon::PLUS)).clicked() {
                acts.push(Action::RemoteForm(crate::repo::remotes::Form::add(d)));
            }
        }
    });
}
