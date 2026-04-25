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
}

#[derive(Clone, Debug)]
pub enum Action {
    Select(Sel),
    Diff(DiffSrc),
    CloseDiff,
    Git(Op),
    Prompt(Prompt),
    Copy(String),
    Reveal(String),
    Jump(String),
    Error(String),
    Undo,
    Redo,
    Reload,
    Cancel,
}

pub struct Msg {
    pub text: String,
    pub error: bool,
}
