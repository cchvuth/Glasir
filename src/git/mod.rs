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
pub use diff::{DiffOpts, FileDiff, Hunk, commit_diff, file_paths, partial_patch, plain_header, selects_all, worktree_diff};
pub use diff::parse_diff;
#[cfg(unix)]
pub use env::import_login_env;
pub use env::check_git;
pub use lfs::{Lfs, LfsPointer, lfs};
pub use log::{LOG, Logged};
pub use model::{Change, Commit, CommitInfo, Ref, RefKind, Stash, State, Stats, Status, Worktree};
/// A ref for display: `refs/heads/x` -> `x` ([`RepoData::rev`](crate::repo::RepoData::rev) hands out full names when the short one is ambiguous).
pub fn short_ref(r: &str) -> &str {
    ["refs/heads/", "refs/remotes/", "refs/tags/"].iter().find_map(|p| r.strip_prefix(p)).unwrap_or(r)
}

pub use network::{credential_token, diagnose, fetch_failure, fetch_quiet, fetch_remote_quiet, ls_remote_quiet, with_autostash};
pub use read::{DATE_FORMAT, LOG_FORMAT, WORKTREE, author, coauthor_line, commit_info, compare_info, config, conflict_kind, git_dir, log, numstat, parse_log, predict_conflicts, refs, remotes, stash_info, stashes, state, status, verify_tag, worktrees};
pub use reflog::{ReflogEntry, reflog};
pub use runner::{CANCELLED, Cancel, cmd, exec, progress_label, run, run_input, run_raw, run_shell, run_tool, stream};
pub use snapshot::{Entry, FOREIGN, Snap, Snapshot, View, ancestors, clean_snap, drop_backups, empty_tree, flagged, head, keep, move_files, related, move_refs, prune, snap, snapshot, current, view, write};
pub(crate) use snapshot::{scratch, used_elsewhere};
#[cfg(test)]
pub use snapshot::leaves;
pub use submodule::{Submodule, SubmoduleDiff, submodules};
pub use url::{Page, autolinks, host, is_web_link, redact, url_encode, url_path, web_url};
