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
    /// An annotated tag's message (empty for branches and lightweight tags).
    pub message: String,
    /// An annotated tag (its message may be empty).
    pub annotated: bool,
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
    /// Conflicted path -> porcelain v2 `XY` code (`conflict_kind` explains it).
    pub conflict_xy: std::collections::HashMap<String, String>,
    /// Changed submodules: path -> porcelain v2 `S<c><m><u>` field
    /// (`C` new commit checked out, `M` modified content, `U` untracked content).
    pub submodules: std::collections::HashMap<String, String>,
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
    /// `git am` stopped on a patch.
    Applying,
}

/// Added / deleted lines per path (`None`: binary), from `git::numstat`.
pub type Stats = std::collections::HashMap<String, Option<(u32, u32)>>;

#[derive(Clone, Debug, Default)]
pub struct CommitInfo {
    pub id: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub date: String,
    pub time: i64,
    pub committer: String,
    pub committer_email: String,
    pub committer_date: String,
    pub committer_time: i64,
    pub message: String,
    /// `Co-authored-by:` trailers as (name, email).
    pub coauthors: Vec<(String, String)>,
    pub files: Vec<Change>,
    /// Line counts, merged in after the list is shown.
    pub stats: Stats,
    /// A stash's untracked files (its third parent), shown after `files`.
    pub untracked: Vec<Change>,
    /// Signature check (`%G?`): G good, U good but untrusted/unknown signer, X/Y expired
    /// signature/key, R revoked key, B bad, E can't be checked, N none; '\0' not checked.
    pub sig: char,
    /// Signer (`%GS`) and key (`%GK`).
    pub signer: String,
    pub key: String,
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
