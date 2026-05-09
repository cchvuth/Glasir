//! Plain data parsed from git output: commits, refs, status, stashes,
//! commit details and worktrees.

#[derive(Clone, Debug, Default)]
pub struct Commit {
    pub id: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub date: String,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Local,
    Remote,
    Tag,
}

#[derive(Clone, Debug)]
pub struct Ref {
    pub kind: RefKind,
    /// Short name: `main`, `origin/main`, `v1.0`.
    pub name: String,
    pub full: String,
    /// Peeled commit id.
    pub target: String,
    pub upstream: String,
    pub ahead: u32,
    pub behind: u32,
    pub gone: bool,
    pub head: bool,
    /// Path of the other (linked) worktree this branch is checked out in.
    pub worktree: Option<String>,
    /// Unix time the ref's object was created (tag date / commit date).
    pub date: i64,
}

impl Ref {
    pub fn remote(&self) -> &str {
        self.name.split('/').next().unwrap_or("")
    }
    /// `origin/feature/x` -> `feature/x`
    pub fn remote_branch(&self) -> &str {
        self.name.split_once('/').map_or(&*self.name, |(_, b)| b)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub path: String,
    pub old_path: Option<String>,
    /// M A D R C T U ?
    pub code: char,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    /// None when detached.
    pub branch: Option<String>,
    /// None when the branch is unborn (no commits yet).
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub staged: Vec<Change>,
    pub unstaged: Vec<Change>,
    pub conflicted: Vec<Change>,
}

impl Status {
    pub fn is_dirty(&self) -> bool {
        !(self.staged.is_empty() && self.unstaged.is_empty() && self.conflicted.is_empty())
    }
}

#[derive(Clone, Debug)]
pub struct Stash {
    pub id: String,
    /// The commit the stash was made on (its first parent).
    pub base: String,
    pub name: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Clean,
    Merging,
    Rebasing,
    CherryPicking,
    Reverting,
    Bisecting,
}

#[derive(Clone, Debug, Default)]
pub struct CommitInfo {
    pub id: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub date: String,
    pub committer: String,
    pub committer_date: String,
    pub message: String,
    pub files: Vec<Change>,
    /// A stash's untracked files (its third parent), shown after `files`.
    pub untracked: Vec<Change>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Worktree {
    pub path: String,
    pub head: String,
    /// Short branch name, `None` when detached.
    pub branch: Option<String>,
    /// Lock reason (possibly empty) when locked.
    pub locked: Option<String>,
    pub prunable: bool,
    pub bare: bool,
    /// Uncommitted changes (0 when unknown).
    pub changes: usize,
}
