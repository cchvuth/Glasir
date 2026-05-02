//! What the UI emits: `Action`s (applied by `RepoTab::apply` after the frame),
//! selections, prompts, failure choices and toast messages.

use super::op::Op;
use crate::git::Change;

#[derive(Clone, Debug, PartialEq)]
pub enum Sel {
    Wip,
    Commit(String),
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
}

#[derive(Clone, Debug)]
pub enum Action {
    Select(Sel),
    Diff(DiffSrc),
    CloseDiff,
    Git(Op),
    Prompt(Prompt),
    Copy(String),
    Url(String),
    Reveal(String),
    Jump(String),
    /// Open the interactive rebase editor for commits after `base`.
    Rebase { base: String, label: String },
    /// Open the 3-way merge editor for a conflicted file.
    MergeEditor(String),
    /// A branch was dropped onto another: offer merge / rebase / push.
    DropBranch { src: String, dst: String },
    /// Rewrite one commit on the current branch: reword / squash / drop.
    Rewrite { commit: String, act: crate::repo::rebase::Act, message: Option<String> },
    AmendInto(String),
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
    Action::Prompt(Prompt { title: title.into(), value: value.into(), op, second: None })
}

pub struct Msg {
    pub text: String,
    pub error: bool,
}
