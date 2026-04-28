//! Everything drawn on screen (egui). Screens read `RepoTab` / `RepoData` and
//! push `Action`s; `RepoTab::apply` runs them after the frame, so no screen
//! mutates repository state directly.
//!
//! Window shell: `app` (App, frame loop, shortcuts), `titlebar`, `toolbar`,
//! `welcome` (+ clone), `settings`, `toasts`, `command_log`, `screenshot` (dev harness).
//! A repo tab: `repo_view` (layout) with `sidebar/`, `graph_view/`, `inspector/`
//! and the center-pane views `diff_view`, `rebase_editor`;
//! its modals are in `dialogs`; `format` holds
//! shared text formatters (dates, initials). Reusable widgets live in `theme::glass`.

mod app;
mod command_log;
mod diff_view;
mod dialogs;
mod format;
mod graph_view;
mod inspector;
mod rebase_editor;
mod repo_view;
mod screenshot;
mod settings;
mod sidebar;
mod titlebar;
mod toasts;
mod toolbar;
mod welcome;

pub use app::App;
