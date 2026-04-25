//! Loaded repository snapshot (`RepoData`) and the parallel `load_all` that
//! builds it.

use crate::git::{self, Commit, Ref, RefKind, Res, Status};
use crate::graph::{Graph, NONE};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct RepoData {
    pub log: Arc<Vec<Commit>>,
    pub refs: Arc<Vec<Ref>>,
    pub remotes: Arc<Vec<(String, String)>>,
    pub git_dir: PathBuf,
    pub status: Status,
    /// Row 0 is the uncommitted-changes pseudo commit.
    pub wip: bool,
    pub graph: Graph,
    /// Commit id -> display row.
    pub row_of: HashMap<String, usize>,
    /// Commit id -> indices into `refs`, most important first.
    pub refs_at: HashMap<String, Vec<usize>>,
}

pub fn is_trunk(name: &str) -> bool {
    matches!(name, "main" | "master" | "develop" | "trunk")
}

impl RepoData {
    pub fn rows(&self) -> usize {
        self.log.len() + self.wip as usize
    }

    /// `None` for the WIP row.
    pub fn commit(&self, row: usize) -> Option<&Commit> {
        if self.wip && row == 0 { None } else { self.log.get(row - self.wip as usize) }
    }

    pub fn local(&self, name: &str) -> Option<&Ref> {
        self.refs.iter().find(|r| r.kind == RefKind::Local && r.name == name)
    }

    /// Web URL of the main remote (github.com or GitHub Enterprise).
    pub fn web(&self) -> Option<String> {
        let r = self.remotes.iter().find(|r| r.0 == "origin").or(self.remotes.first())?;
        git::web_url(&r.1)
    }

    pub fn web_of(&self, remote: &str) -> Option<String> {
        self.remotes.iter().find(|r| r.0 == remote).and_then(|r| git::web_url(&r.1)).or_else(|| self.web())
    }

    pub fn default_branch(&self) -> String {
        let has = |n: &str| self.refs.iter().any(|r| r.kind == RefKind::Remote && r.name == n);
        ["origin/main", "origin/master"].into_iter().find(|n| has(n)).map(|n| n[7..].to_owned()).unwrap_or_else(|| "main".into())
    }

    pub(super) fn build(
        log: Arc<Vec<Commit>>,
        refs: Arc<Vec<Ref>>,
        remotes: Arc<Vec<(String, String)>>,
        git_dir: PathBuf,
        status: Status,
    ) -> Self {
        let wip = status.is_dirty();
        let off = wip as usize;
        let mut row_of: HashMap<String, usize> = log.iter().enumerate().map(|(i, c)| (c.id.clone(), i + off)).collect();
        let mut parents: Vec<Vec<u32>> =
            log.iter().map(|c| c.parents.iter().map(|p| row_of.get(p).map_or(NONE, |&r| r as u32)).collect()).collect();
        let mut prio = vec![6u32; log.len()];
        let mut refs_at: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, r) in refs.iter().enumerate() {
            let p = match r.kind {
                RefKind::Local if r.head => 0,
                RefKind::Local if is_trunk(&r.name) => 1,
                RefKind::Local => 2,
                RefKind::Remote if is_trunk(r.remote_branch()) => 3,
                RefKind::Remote => 4,
                RefKind::Tag => 5,
            };
            if let Some(&row) = row_of.get(&r.target) {
                prio[row - off] = prio[row - off].min(p);
            }
            refs_at.entry(r.target.clone()).or_default().push(i);
        }
        for v in refs_at.values_mut() {
            v.sort_by_key(|&i| (!refs[i].head, refs[i].kind as u8, refs[i].name.clone()));
        }
        let head_row = status.head.as_ref().and_then(|h| row_of.get(h)).copied();
        if let Some(r) = head_row {
            prio[r - off] = 0;
        }
        if wip {
            parents.insert(0, vec![head_row.map_or(NONE, |r| r as u32)]);
            prio.insert(0, 0);
            row_of.insert(String::new(), 0);
        }
        let graph = Graph::layout(&parents, &prio);
        Self {
            log,
            refs,
            remotes,
            git_dir,
            status,
            wip,
            graph,
            row_of,
            refs_at,
        }
    }
}

pub(super) fn load_all(dir: &Path, limit: usize) -> Res<RepoData> {
    let git_dir = git::git_dir(dir)?;
    let (status, refs, log, remotes) = std::thread::scope(|s| {
        let st = s.spawn(|| git::status(dir));
        let rf = s.spawn(|| git::refs(dir));
        let lg = s.spawn(|| {
            let r = ["--branches", "--remotes", "--tags", "HEAD"];
            // Unborn HEAD: retry without it.
            git::log(dir, limit, &r).or_else(|e| match r.split_last() {
                Some((&"HEAD", rest)) => git::log(dir, limit, rest),
                _ => Err(e),
            })
        });
        let rm = s.spawn(|| git::remotes(dir).unwrap_or_default());
        (st.join().unwrap(), rf.join().unwrap(), lg.join().unwrap(), rm.join().unwrap())
    });
    Ok(RepoData::build(Arc::new(log?), Arc::new(refs?), Arc::new(remotes), git_dir, status?))
}
