//! Thin layer over the `git` CLI: everything shells out, so user config, hooks,
//! credential helpers (GHE SSO), LFS, signing and partial clones behave as in the terminal.
//! Call sites use the flat `git::` names re-exported below. No egui here.

mod diff;
mod env;
mod log;
mod model;
mod network;
mod read;
mod reflog;
mod runner;
mod snapshot;
mod url;

pub type Res<T> = Result<T, String>;

pub use diff::{FileDiff, Hunk, commit_diff, partial_patch, worktree_diff};
#[cfg(test)]
pub use diff::parse_diff;
#[cfg(unix)]
pub use env::import_login_env;
pub use env::check_git;
pub use log::{LOG, Logged};
pub use model::{Change, Commit, CommitInfo, Ref, RefKind, Stash, State, Status, Worktree};
pub use network::{fetch_failure, fetch_quiet, with_autostash};
pub use read::{DATE_FORMAT, commit_info, compare_info, git_dir, log, predict_conflicts, refs, remotes, stash_info, stashes, state, status, worktrees};
pub use reflog::{ReflogEntry, reflog};
pub use runner::{CANCELLED, Cancel, cmd, progress_label, run, run_input, run_raw};
pub use snapshot::{Snapshot, restore, snapshot};
pub use url::{url_encode, url_path, web_url};
