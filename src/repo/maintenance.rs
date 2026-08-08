//! Repository maintenance: when a repo is worth optimizing (many loose objects, no
//! commit-graph), the one-click `git maintenance` run and the background-schedule toggle.

use super::op::Op;
use crate::git::{self, Res};
use std::path::Path;

/// `exec` pseudo-command: `MAINTAIN run|start|stop`.
pub(super) const MAINTAIN: &str = "!maintain";

/// Loose objects from which git itself would soon want to gc (its default is 6700).
pub const LOOSE_MAX: usize = 2000;
/// Without a commit-graph, history walks only start to slow down around here.
const GRAPH_MIN: usize = 5000;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Health {
    pub loose: usize,
    pub packed: usize,
    pub graph: bool,
    /// Listed in the user's `maintenance.repo` (scheduled background maintenance).
    pub scheduled: bool,
}

impl Health {
    /// Worth the "Optimize repository" hint.
    pub fn wants_optimizing(&self) -> bool {
        self.loose >= LOOSE_MAX || (!self.graph && self.loose + self.packed >= GRAPH_MIN)
    }
}

/// Three cheap git calls; any failure reads as "healthy" (no hint).
pub fn health(dir: &Path) -> Health {
    let Ok(out) = git::run(dir, &["rev-parse", "--show-toplevel", "--git-path", "objects/info/commit-graph", "--git-path", "objects/info/commit-graphs"]) else {
        return Health { graph: true, ..Default::default() };
    };
    let l: Vec<&str> = out.lines().collect();
    let exists = |p: Option<&&str>| p.is_some_and(|p| dir.join(p).exists());
    let counts = git::run(dir, &["count-objects", "-v"]).unwrap_or_default();
    let count = |k: &str| counts.lines().find_map(|l| l.strip_prefix(k)?.trim().parse().ok()).unwrap_or(0);
    let top = l.first().copied().unwrap_or_default();
    let reg = git::run(dir, &["config", "--global", "--get-all", "maintenance.repo"]).unwrap_or_default();
    Health { loose: count("count:"), packed: count("in-pack:"), graph: exists(l.get(1)) || exists(l.get(2)), scheduled: !top.is_empty() && reg.lines().any(|r| r == top) }
}

/// The toolbar / palette command (`run` refuses mid-merge, rebase, ...).
pub fn op() -> Op {
    Op::new("Repo maintenance", &[MAINTAIN, "run"])
}

/// Turn scheduled background maintenance on or off for this repository; ⌘Z flips it back.
pub fn schedule_op(on: bool) -> Op {
    let (label, other) = if on { ("Enable background maintenance", "Disable background maintenance") } else { ("Disable background maintenance", "Enable background maintenance") };
    let (cmd, back) = if on { ("start", "stop") } else { ("stop", "start") };
    let o = Op::new(label, &[MAINTAIN, cmd]).revert(Op::new(other, &[MAINTAIN, back]));
    match on {
        true => o.confirm("Let git run maintenance for this repository in the background (hourly, daily, weekly)? This adds it to the maintenance.repo list in your global git config and installs git's scheduled job (launchd, cron, systemd or Task Scheduler). Undo with ⌘Z."),
        false => o,
    }
}

/// Executes a `MAINTAIN` command (see `op::exec`).
pub(super) fn run(dir: &Path, cmd: &str) -> Res<String> {
    match cmd {
        "start" => git::run(dir, &["maintenance", "start"]),
        "stop" => {
            git::run(dir, &["maintenance", "unregister"])?;
            // `register` also writes these two repo-local settings and `unregister` leaves
            // them, which would keep git's own auto-gc off here.
            // ponytail: a user's own identical values go too.
            for (k, v) in [("maintenance.auto", "^false$"), ("maintenance.strategy", "^incremental$")] {
                let _ = git::run(dir, &["config", "--local", "--unset", k, v]);
            }
            Ok(String::new())
        }
        _ => {
            if git::state(&git::git_dir(dir)?) != git::State::Clean {
                return Err("Finish or abort the merge, rebase, cherry-pick or bisect in progress first.".into());
            }
            // Loose objects first: with no pack yet, incremental-repack has nothing to index and fails.
            match git::run(dir, &["maintenance", "run", "--task=loose-objects"]) {
                // git before 2.30 has no `maintenance`.
                Err(e) if e.contains("not a git command") => git::run(dir, &["gc", "--auto"]),
                Err(e) => Err(e),
                // The task only deletes the loose copies of what an earlier run packed.
                Ok(_) => git::run(dir, &["prune-packed", "-q"]).and_then(|_| git::run(dir, &["maintenance", "run", "--task=commit-graph", "--task=incremental-repack"])),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::op::exec;
    use super::*;
    use crate::repo::testutil::{commit, identity, scratch, sh};

    #[test]
    fn hint_heuristic() {
        let h = |loose, packed, graph| Health { loose, packed, graph, scheduled: false }.wants_optimizing();
        assert!(!h(10, 100, false), "a small repo needs no commit-graph");
        assert!(h(10, 6000, false));
        assert!(!h(10, 6000, true));
        assert!(h(2000, 0, true), "many loose objects");
        assert!(!h(1999, 0, true));
    }

    #[test]
    fn health_then_maintenance() {
        let dir = scratch("maintenance");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a", "a\n");
        commit(&dir, "b", "b\n");
        let before = health(&dir);
        assert!(before.loose >= 6 && before.packed == 0 && !before.graph && !before.scheduled, "{before:?}");
        let head = sh(&dir, &["rev-parse", "HEAD"]);
        exec(&dir, &op()).unwrap();
        let after = health(&dir);
        assert!(after.loose == 0 && after.packed >= 6 && after.graph, "{after:?}");
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), head);
        sh(&dir, &["fsck", "--strict"]);
        // Mid-merge it refuses and leaves everything alone.
        sh(&dir, &["checkout", "-q", "-b", "x"]);
        commit(&dir, "a", "x\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "a", "m\n");
        let _ = git::run(&dir, &["merge", "x"]);
        assert!(exec(&dir, &op()).unwrap_err().contains("in progress"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
