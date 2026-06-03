//! Thin layer over the `git` CLI: everything shells out, so user config, hooks,
//! credential helpers (GHE SSO), LFS, signing and partial clones behave as in the terminal.
//! Call sites use the flat `git::` names re-exported below. No egui here.

mod bisect;
mod diff;
mod env;
mod lfs;
mod log;
mod model;
mod network;
mod read;
mod reflog;
mod runner;
mod snapshot;
mod submodule;
mod url;

pub type Res<T> = Result<T, String>;

pub use bisect::{Bisect, bisect};
pub use diff::{DiffOpts, FileDiff, Hunk, commit_diff, file_paths, partial_patch, worktree_diff};
pub use diff::parse_diff;
#[cfg(unix)]
pub use env::import_login_env;
pub use env::check_git;
pub use lfs::{Lfs, LfsPointer, lfs};
pub use log::{LOG, Logged};
pub use model::{Change, Commit, CommitInfo, Ref, RefKind, Stash, State, Status, Worktree};
pub use network::{diagnose, fetch_failure, fetch_quiet, ls_remote_quiet, with_autostash};
pub use read::{DATE_FORMAT, commit_info, compare_info, config, git_dir, log, predict_conflicts, refs, remotes, stash_info, stashes, state, status, worktrees};
pub use reflog::{ReflogEntry, reflog};
pub use runner::{CANCELLED, Cancel, cmd, progress_label, run, run_input, run_raw};
pub use snapshot::{Snapshot, restore, snapshot};
pub use submodule::{Submodule, SubmoduleDiff, submodules};
pub use url::{autolinks, host, redact, url_encode, url_path, web_url};
