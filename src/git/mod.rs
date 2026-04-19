//! Thin layer over the `git` CLI: everything shells out, so user config, hooks,
//! credential helpers (GHE SSO), LFS, signing and partial clones behave as in the terminal.
//! Call sites use the flat `git::` names re-exported below. No egui here.

mod env;
mod log;
mod model;
mod read;
mod runner;

pub type Res<T> = Result<T, String>;

#[cfg(unix)]
pub use env::import_login_env;
pub use env::check_git;
pub use log::{LOG, Logged};
pub use model::{Change, Commit, CommitInfo, Ref, RefKind, Status};
pub use read::{commit_info, git_dir, log, refs, remotes, status};
pub use runner::{CANCELLED, Cancel, progress_label, run, run_input};
