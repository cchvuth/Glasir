//! Everything drawn on screen (egui). Screens read `RepoTab` / `RepoData` and push `Action`s;
//! `RepoTab::apply` runs them after the frame, so no screen mutates repository state directly.
//! Window shell, repo tab and dialogs live in the submodules; reusable widgets in `theme::glass`.

mod app;
mod bisect;
mod command_log;
mod compose;
mod diff_view;
mod file_edit;
mod dialogs;
mod doctor;
mod format;
mod graph_view;
mod history;
mod image_diff;
mod inspector;
mod links;
mod merge_editor;
mod palette;
mod rebase_editor;
mod reflog;
mod repo_view;
mod screenshot;
mod settings;
mod shortcuts;
mod sidebar;
mod terminal;
#[cfg(test)]
mod testkit;
mod titlebar;
mod toasts;
mod toolbar;
mod welcome;

pub use app::App;
