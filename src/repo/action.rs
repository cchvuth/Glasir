//! What the UI emits: `Action`s (applied by `RepoTab::apply` after the frame),
//! selections, prompts, failure choices and toast messages.

use super::data::View;
use super::op::Op;
use crate::ai;
use crate::git::Change;

#[derive(Clone, Debug, PartialEq)]
pub enum Sel {
    Wip,
    Commit(String),
    /// Changes from `from` to `to` (merge-base based when `three_dot`).
    Compare { from: String, to: String, three_dot: bool },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MultiOp {
    CherryPick,
    /// Open them in the rebase editor in cherry-pick mode.
    CherryPickEdit,
    Revert,
    Squash,
    Drop,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiffSrc {
    Worktree { change: Change, staged: bool },
    Commit { id: String, parent: Option<String>, change: Change },
}

impl DiffSrc {
    pub fn change(&self) -> &Change {
        match self {
            DiffSrc::Worktree { change, .. } | DiffSrc::Commit { change, .. } => change,
        }
    }
}

/// A failed op's one-click ways out: (button, op) pairs the user picks from.
pub struct Choice {
    pub title: String,
    pub text: String,
    pub ops: Vec<(String, Op)>,
}

/// Ask for a value, then run `op` with every `{}` in its args replaced.
#[derive(Clone, Debug)]
pub struct Prompt {
    pub title: String,
    pub value: String,
    pub op: Op,
    /// Optional second field (e.g. a tag message), substituted for `{2}`.
    pub second: Option<(String, String)>,
    /// A "Sign tag" checkbox: adds `--sign` / `--no-sign` after the first command's verb.
    pub sign: Option<bool>,
}

#[derive(Clone, Debug)]
pub enum Action {
    LoadMore,
    Select(Sel),
    Diff(DiffSrc),
    CloseDiff,
    DiffOpts(crate::git::DiffOpts),
    Git(Op),
    Prompt(Prompt),
    /// Check a tag's signature (`git verify-tag`, full ref) and toast the result.
    VerifyTag(String),
    Copy(String),
    Url(String),
    /// Show a file in the file manager ("" = open the repo folder).
    Reveal(String),
    /// Open a file in the user's editor ("" = the repo folder).
    Editor(String),
    Terminal,
    /// Run a custom command (Settings → Tools) with these placeholder values.
    Custom(super::tools::Custom, super::tools::Vars),
    Jump(String),
    /// Ask Claude (streams into `RepoTab::ai`).
    Ai(ai::Purpose),
    /// Open the interactive rebase editor for commits after `base`.
    Rebase { base: String, label: String },
    /// Open the 3-way merge editor for a conflicted file.
    MergeEditor(String),
    FileView(String, super::history::Mode),
    /// A branch was dropped onto another (full refs): offer merge / rebase / push.
    DropBranch { src: String, dst: String },
    Cleanup,
    /// Rewrite one commit on the current branch: reword / squash / drop.
    Rewrite { commit: String, act: crate::repo::rebase::Act, message: Option<String> },
    AmendInto(String),
    /// Plan absorbing the staged hunks into the commits they belong to (preview first).
    Absorb,
    /// Uncommit this commit and guide splitting it into several.
    Split(String),
    /// Finish the guided split (`rebase --continue`), or abort it.
    SplitEnd { abort: bool },
    OpenTab(String),
    /// Change the graph filter (hide / solo / first-parent) and reload.
    View(View),
    /// Replace the multi-selection (commit ids, graph order: newest first).
    Multi(Vec<String>),
    MultiOp(MultiOp, Vec<String>),
    /// Open the reflog view (recover commits lost to a reset or rebase).
    Reflog,
    RemoteForm(super::remotes::Form),
    /// Check the connection to this remote (doctor window).
    Doctor(String),
    /// Export commits (newest first) as a patch: copy it, or `save` it to a file.
    Patch { ids: Vec<String>, save: bool },
    ApplyPatch,
    Error(String),
    Undo,
    Redo,
    Reload,
    Cancel,
}

pub fn op(label: impl Into<String>, args: &[&str]) -> Action {
    Action::Git(Op::new(label, args))
}

pub fn prompt(title: impl Into<String>, value: impl Into<String>, op: Op) -> Action {
    Action::Prompt(Prompt { title: title.into(), value: value.into(), op, second: None, sign: None })
}

pub struct Msg {
    pub text: String,
    pub error: bool,
}
