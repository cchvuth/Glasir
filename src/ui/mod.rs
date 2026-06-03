//! Everything drawn on screen (egui). Screens read `RepoTab` / `RepoData` and
//! push `Action`s; `RepoTab::apply` runs them after the frame, so no screen
//! mutates repository state directly.
//!
//! Window shell: `app` (App, frame loop, shortcuts), `titlebar`, `toolbar`,
//! `welcome` (+ clone), `settings`, `toasts`, `command_log`, `screenshot` (dev harness).
//! A repo tab: `repo_view` (layout) with `sidebar/`, `graph_view/`, `inspector/`
//! and the center-pane views `diff_view`, `merge_editor`, `history`, `rebase_editor`, `reflog`;
//! its modals are in `dialogs` (the Commit Composer's in `compose`, the connection doctor in `doctor`). `palette` is the ⌘P command palette; `format` holds
//! shared text formatters (dates, initials). Reusable widgets live in `theme::glass`.

mod app;
mod bisect;
mod command_log;
mod compose;
mod diff_view;
mod dialogs;
mod doctor;
mod format;
mod graph_view;
mod history;
mod image_diff;
mod inspector;
mod merge_editor;
mod palette;
mod rebase_editor;
mod reflog;
mod repo_view;
mod screenshot;
mod settings;
mod sidebar;
mod titlebar;
mod toasts;
mod toolbar;
mod welcome;

pub use app::App;
