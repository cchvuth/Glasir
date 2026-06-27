//! Commit Composer: every uncommitted change as numbered units (tracked diff
//! hunks; untracked, binary and other unsplittable files whole), Claude's plan
//! for them (validated and repaired), the edits the plan dialog makes, and the
//! op that commits the plan.

use super::{Msg, Op, RepoTab, Task, Undo};
use crate::ai;
use crate::git::{self, FileDiff, Res};
use std::path::Path;

pub struct Unit {
    pub path: String,
    /// "modified, hunk 2/3" or "new file, whole file".
    pub what: String,
    /// (file, hunk) in `Plan::files`; `None` = the whole file, staged with `git add`.
    hunk: Option<(usize, usize)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub message: String,
    /// Indexes into `Plan::units`, ascending.
    pub units: Vec<usize>,
}

pub struct Plan {
    pub units: Vec<Unit>,
    /// Empty until Claude's reply arrives.
    pub commits: Vec<Commit>,
    /// What was repaired in Claude's plan, shown in the dialog.
    pub note: Option<String>,
    files: Vec<FileDiff>,
    head: String,
    /// [`changes`] when the plan was made: the op refuses if they moved since.
    changes: String,
}

/// Lines of a unit shown to Claude, per unit; whole new files are cut here.
const MAX_LINES: usize = 300;

fn render(out: &mut String, h: &git::Hunk) {
    out.push_str(&h.header);
    out.push('\n');
    for l in h.lines.iter().take(MAX_LINES) {
        out.push(l.kind as char);
        out.push_str(&l.text);
        out.push('\n');
    }
    if h.lines.len() > MAX_LINES {
        out.push_str(&format!("… ({} more lines)\n", h.lines.len() - MAX_LINES));
    }
}

/// The uncommitted changes the units are made of: `git diff HEAD` and the
/// untracked files. Submodules with only local edits are left out: `git add`
/// can't commit those.
fn changes(dir: &Path) -> Res<(String, String)> {
    let diff = ["diff", "HEAD", "--no-color", "--no-ext-diff", "--no-textconv", "--no-renames", "--submodule=short", "--ignore-submodules=dirty", "--src-prefix=a/", "--dst-prefix=b/"];
    Ok((git::run(dir, &diff)?, git::run(dir, &["ls-files", "-z", "--others", "--exclude-standard"])?))
}

impl Plan {
    /// The units and their listing for Claude (`[id] path (what)` + the diff).
    pub fn load(dir: &Path) -> Res<(Plan, String)> {
        let head = git::run(dir, &["rev-parse", "-q", "--verify", "HEAD"]).map_err(|_| "Make a first commit first.".to_string())?.trim().to_owned();
        let (raw, untracked) = changes(dir)?;
        let files = git::parse_diff(&raw);
        let names = git::run(dir, &["diff", "HEAD", "--name-only", "-z", "--no-renames", "--ignore-submodules=dirty"])?;
        // Each commit starts from HEAD's tree, so content only in the index would be lost.
        let staged = git::run(dir, &["diff", "--cached", "--name-only", "-z", "--no-renames", "--ignore-submodules=dirty"])?;
        if let Some(p) = staged.split('\0').find(|p| !p.is_empty() && !names.split('\0').any(|n| n == *p)) {
            return Err(format!("{p} has staged changes that aren't in the working tree; commit or unstage them first."));
        }
        let (mut units, mut listing) = (vec![], String::new());
        let mut add = |units: &mut Vec<Unit>, path: &str, what: String, hunk, body: &[git::Hunk]| {
            listing += &format!("[{}] {path} ({what})\n", units.len() + 1);
            for h in body {
                render(&mut listing, h);
            }
            units.push(Unit { path: path.into(), what, hunk });
        };
        for (fi, (f, named)) in files.iter().zip(git::file_paths(&files, &names)).enumerate() {
            let Some((path, typechange)) = named else { continue };
            let has = |p: &str| f.header.iter().any(|h| h.starts_with(p));
            let status = if typechange { "type changed" } else if has("new file") { "new file" } else if has("deleted file") { "deleted" } else { "modified" };
            // Binary, submodule, type, mode-only or non-UTF-8 changes can't be split into hunks.
            let lossy = f.hunks.iter().flat_map(|h| &h.lines).any(|l| l.text.contains('\u{FFFD}'));
            if typechange || f.hunks.is_empty() || f.binary || lossy || f.header.iter().any(|h| h.contains(" 160000")) {
                let kind = if f.binary { "binary, whole file" } else { "whole file" };
                add(&mut units, path, format!("{status}, {kind}"), None, if lossy { &[] } else { &f.hunks });
                continue;
            }
            let n = f.hunks.len();
            for (hi, h) in f.hunks.iter().enumerate() {
                add(&mut units, path, format!("{status}, hunk {}/{n}", hi + 1), Some((fi, hi)), std::slice::from_ref(h));
            }
        }
        for path in untracked.split('\0').filter(|p| !p.is_empty()) {
            let c = git::Change { path: path.into(), old_path: None, code: '?' };
            let f = git::worktree_diff(dir, &c, false, Default::default()).unwrap_or_default();
            let body = if f.binary || f.lossy { &[][..] } else { &f.hunks[..] };
            add(&mut units, path, "new file, untracked".into(), None, body);
        }
        if units.is_empty() {
            return Err("There are no uncommitted changes.".into());
        }
        Ok((Plan { units, commits: vec![], note: None, files, head, changes: raw + &untracked }, listing))
    }

    /// Takes Claude's JSON reply (see `ai::Purpose::Compose`), repairing it.
    pub fn fill(&mut self, reply: &str) -> Res<()> {
        #[derive(serde::Deserialize)]
        struct Reply {
            commits: Vec<C>,
        }
        #[derive(serde::Deserialize)]
        struct C {
            message: String,
            units: Vec<i64>,
        }
        let r: Reply = serde_json::from_str(reply.trim()).map_err(|e| format!("{}'s plan isn't valid ({e}).", ai::name()))?;
        (self.commits, self.note) = repair(r.commits.into_iter().map(|c| (c.message, c.units)).collect(), self.units.len());
        Ok(())
    }

    /// Moves unit `u` into commit `to`, or into a new commit right after its own (`None`: split).
    pub fn move_unit(&mut self, u: usize, to: Option<usize>) {
        let Some(from) = self.commits.iter().position(|c| c.units.contains(&u)) else { return };
        let to = to.unwrap_or_else(|| {
            self.commits.insert(from + 1, Commit { message: String::new(), units: vec![] });
            from + 1
        });
        self.commits[from].units.retain(|&x| x != u);
        self.commits[to].units.push(u);
        self.commits[to].units.sort_unstable();
        if self.commits[from].units.is_empty() {
            self.commits.remove(from);
        }
    }

    /// Folds commit `k` into the one before it (messages joined).
    pub fn merge_up(&mut self, k: usize) {
        if k == 0 || k >= self.commits.len() {
            return;
        }
        let c = self.commits.remove(k);
        let prev = &mut self.commits[k - 1];
        prev.units.extend(c.units);
        prev.units.sort_unstable();
        prev.message = format!("{}\n\n{}", prev.message.trim(), c.message.trim()).trim().to_owned();
    }

    /// Unstage everything; then per planned commit (oldest first): the original
    /// HEAD's tree plus every hunk planned so far as the index, whole files
    /// added, commit. Stops at the first failure, leaving the rest uncommitted
    /// (and unstaged: `poll_op` resets the index);
    /// undo (`--soft`) puts HEAD back with everything staged.
    pub fn op(&self, dir: &Path, git_dir: &Path) -> Res<Op> {
        if git::run(dir, &["rev-parse", "HEAD"])?.trim() != self.head {
            return Err("HEAD moved since the plan was made; compose again.".into());
        }
        if changes(dir).map(|(d, u)| d + &u)? != self.changes {
            return Err("The changes were edited since the plan was made; compose again.".into());
        }
        if self.commits.iter().any(|c| c.message.trim().is_empty()) {
            return Err("Every commit needs a message.".into());
        }
        let n = self.commits.len();
        let mut op = Op::new(format!("Compose {n} commit{}", if n == 1 { "" } else { "s" }), &["read-tree", "--reset", &self.head]);
        let mut done: Vec<usize> = vec![];
        for (k, c) in self.commits.iter().enumerate() {
            done.extend(&c.units);
            let mut patch = String::new();
            for (fi, f) in self.files.iter().enumerate() {
                let sel = |hi: usize| done.iter().any(|&u| self.units[u].hunk == Some((fi, hi)));
                patch += &git::partial_patch(f, |hi, _| sel(hi), false).unwrap_or_default();
            }
            let whole: Vec<&str> = done.iter().filter(|&&u| self.units[u].hunk.is_none()).map(|&u| self.units[u].path.as_str()).collect();
            if k > 0 {
                op = op.then(&["read-tree", "--reset", &self.head]);
            }
            if !patch.is_empty() {
                let file = git_dir.join(format!("glasir-compose-{k}.patch"));
                std::fs::write(&file, patch).map_err(|e| e.to_string())?;
                op = op.then(&["apply", "--cached", &file.to_string_lossy()]);
            }
            if !whole.is_empty() {
                op = op.then(&[&["--literal-pathspecs", "add", "--"][..], &whole].concat());
            }
            let msg = git_dir.join(format!("glasir-compose-{k}.msg"));
            std::fs::write(&msg, c.message.trim()).map_err(|e| e.to_string())?;
            op = op.then(&["commit", "-q", "-F", &msg.to_string_lossy()]);
        }
        Ok(op.undo(Undo::Soft))
    }
}

/// Claude's commits as unit indexes: unknown ids and repeats dropped (a unit
/// stays in its first commit), empty commits dropped, left-out units collected
/// in a last commit. Returns what was repaired.
fn repair(raw: Vec<(String, Vec<i64>)>, n: usize) -> (Vec<Commit>, Option<String>) {
    let mut seen = vec![false; n];
    let (mut unknown, mut twice) = (0, 0);
    let mut commits = vec![];
    for (message, ids) in raw {
        let mut units = vec![];
        for id in ids {
            match usize::try_from(id).ok().and_then(|i| i.checked_sub(1)).filter(|&i| i < n) {
                Some(i) if !seen[i] => {
                    seen[i] = true;
                    units.push(i);
                }
                Some(_) => twice += 1,
                None => unknown += 1,
            }
        }
        units.sort_unstable();
        if !units.is_empty() {
            commits.push(Commit { message: message.trim().to_owned(), units });
        }
    }
    let missing: Vec<usize> = (0..n).filter(|&i| !seen[i]).collect();
    let mut notes = vec![];
    if unknown > 0 {
        notes.push(format!("ignored {unknown} unknown change id(s)"));
    }
    if twice > 0 {
        notes.push(format!("{twice} change(s) planned twice stay in their first commit"));
    }
    if !missing.is_empty() {
        notes.push(format!("{} change(s) {} left out are in the last commit", missing.len(), ai::name()));
        commits.push(Commit { message: "Remaining changes".into(), units: missing });
    }
    let note = (!notes.is_empty()).then(|| {
        let s = notes.join("; ");
        format!("Fixed up {}'s plan: {s}.", ai::name())
    });
    (commits, note)
}

impl RepoTab {
    /// Collect the units in the background, then ask Claude (`poll_compose`).
    pub(super) fn start_compose(&mut self, ctx: &egui::Context) {
        let dir = self.dir.clone();
        self.compose = None;
        self.compose_task = Some(Task::spawn(ctx, move || Plan::load(&dir)));
    }

    pub(super) fn poll_compose(&mut self, ctx: &egui::Context) {
        match Task::poll(&mut self.compose_task) {
            Some(Ok((plan, listing))) => {
                self.compose = Some(plan);
                self.ai = Some(ai::start(ctx, self.ai_cfg.clone(), self.dir.clone(), ai::Purpose::Compose, listing));
            }
            Some(Err(e)) => self.notes.push(Msg { text: format!("Compose commits: {e}"), error: true }),
            None => {}
        }
    }

    /// Collecting the changes or waiting for Claude's plan.
    pub fn composing(&self) -> bool {
        self.compose_task.is_some() || self.ai_busy() == Some(&ai::Purpose::Compose)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::testutil::{commit, identity, scratch, settle, sh};

    fn ids(c: &[Commit]) -> Vec<Vec<usize>> {
        c.iter().map(|c| c.units.clone()).collect()
    }

    #[test]
    fn repair_plans() {
        let ok = vec![("a".into(), vec![2, 1]), ("b".into(), vec![3])];
        assert_eq!(repair(ok, 3), (vec![Commit { message: "a".into(), units: vec![0, 1] }, Commit { message: "b".into(), units: vec![2] }], None));
        // Unknown (0, 9, -1) and repeated ids dropped, an emptied commit removed, unit 4 left out.
        let bad = vec![("a".into(), vec![1, 0, 9, -1]), ("b".into(), vec![1]), ("c".into(), vec![2, 3, 2])];
        let (c, note) = repair(bad, 4);
        assert_eq!(ids(&c), [vec![0], vec![1, 2], vec![3]]);
        assert_eq!((c[1].message.as_str(), c[2].message.as_str()), ("c", "Remaining changes"));
        let note = note.unwrap();
        assert!(note.contains("3 unknown") && note.contains("2 change(s) planned twice") && note.contains("1 change(s) Claude left out"), "{note}");
        let (c, _) = repair(vec![], 2);
        assert_eq!(ids(&c), [vec![0, 1]]);
    }

    #[test]
    fn edit_plan() {
        let mut p = Plan { units: vec![], commits: vec![], note: None, files: vec![], head: String::new(), changes: String::new() };
        p.commits = vec![Commit { message: "a".into(), units: vec![0, 1] }, Commit { message: "b".into(), units: vec![2] }];
        p.move_unit(1, None);
        assert_eq!(ids(&p.commits), [vec![0], vec![1], vec![2]]);
        p.move_unit(1, Some(2));
        assert_eq!(ids(&p.commits), [vec![0], vec![1, 2]]);
        p.merge_up(1);
        assert_eq!((ids(&p.commits), p.commits[0].message.as_str()), (vec![vec![0, 1, 2]], "a\n\nb"));
    }

    /// 2 tracked files with 3 hunks plus an untracked file, planned into 2
    /// commits that split file `a`'s hunks; everything ends up committed.
    #[test]
    fn compose_end_to_end() {
        let dir = scratch("compose");
        let g = |a: &[&str]| sh(&dir, a);
        g(&["init", "-q", "-b", "main"]);
        identity(&dir);
        let lines = |f: &dyn Fn(usize) -> String| (1..=20).map(|i| f(i) + "\n").collect::<String>();
        commit(&dir, "a", &lines(&|i| format!("a{i}")));
        commit(&dir, "b", "b1\nb2\n");
        std::fs::write(dir.join("a"), lines(&|i| if i == 2 || i == 18 { format!("A{i}") } else { format!("a{i}") })).unwrap();
        std::fs::write(dir.join("b"), "b1\nB2\n").unwrap();
        std::fs::write(dir.join("c"), "new\n").unwrap();
        g(&["add", "b"]); // staged state doesn't matter
        let (mut plan, listing) = Plan::load(&dir).unwrap();
        let what: Vec<String> = plan.units.iter().map(|u| format!("{} {}", u.path, u.what)).collect();
        assert_eq!(what, ["a modified, hunk 1/2", "a modified, hunk 2/2", "b modified, hunk 1/1", "c new file, untracked"]);
        assert!(listing.contains("[2] a (modified, hunk 2/2)\n@@ -15,6 +15,6 @@") && listing.contains("[4] c (new file, untracked)\n@@ -0,0 +1,1 @@\n+new"), "{listing}");
        plan.fill(r#"{"commits":[{"message":"Second half of a, add c","units":[2,4]},{"message":"First half\n\nwith b","units":[3,1]}]}"#).unwrap();
        assert_eq!(plan.note, None);
        // An edit made after planning is refused rather than left out of the commits.
        let a = std::fs::read_to_string(dir.join("a")).unwrap();
        std::fs::write(dir.join("a"), "edited later\n").unwrap();
        assert!(plan.op(&dir, &git::git_dir(&dir).unwrap()).is_err());
        std::fs::write(dir.join("a"), a).unwrap();
        let op = plan.op(&dir, &git::git_dir(&dir).unwrap()).unwrap();
        assert_eq!((op.label.as_str(), op.undo), ("Compose 2 commits", Undo::Soft));
        super::super::op::exec(&dir, &op).unwrap();
        assert_eq!(g(&["log", "--format=%s", "-n", "2"]), "First half\nSecond half of a, add c\n");
        assert_eq!(g(&["show", "--format=", "--name-only", "HEAD~1"]), "a\nc\n");
        assert!(g(&["show", "HEAD~1", "--", "a"]).contains("+A18") && !g(&["show", "HEAD~1", "--", "a"]).contains("+A2"));
        assert_eq!(g(&["log", "-1", "--format=%b"]).trim(), "with b");
        assert_eq!(g(&["status", "--porcelain"]), "", "everything committed");
        // A stale plan is refused.
        assert!(plan.op(&dir, &git::git_dir(&dir).unwrap()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A submodule with only local edits is no unit; content only in the index is refused.
    #[test]
    fn compose_dirty_submodule_and_index_only_content() {
        let (dir, sub) = (scratch("compose-sm"), scratch("compose-sm-sub"));
        let g = |a: &[&str]| sh(&dir, a);
        for d in [&dir, &sub] {
            sh(d, &["init", "-q", "-b", "main"]);
            identity(d);
            commit(d, "x", "x1\n");
        }
        g(&["-c", "protocol.file.allow=always", "submodule", "add", "-q", &sub.to_string_lossy(), "sub"]);
        g(&["commit", "-q", "-m", "sub"]);
        std::fs::write(dir.join("sub/x"), "dirty\n").unwrap();
        std::fs::write(dir.join("x"), "x2\n").unwrap();
        let (plan, _) = Plan::load(&dir).unwrap();
        assert_eq!(plan.units.iter().map(|u| u.path.as_str()).collect::<Vec<_>>(), ["x"]);
        std::fs::write(dir.join("new"), "precious\n").unwrap();
        g(&["add", "new"]);
        std::fs::remove_file(dir.join("new")).unwrap();
        assert!(Plan::load(&dir).is_err_and(|e| e.starts_with("new has staged changes")));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&sub);
    }

    /// A typechange is one whole-file unit; a failed step leaves the rest unstaged, not reverted.
    #[cfg(unix)]
    #[test]
    fn compose_typechange_and_failed_step() {
        let dir = scratch("compose-tc");
        let g = |a: &[&str]| sh(&dir, a);
        g(&["init", "-q", "-b", "main"]);
        identity(&dir);
        for f in ["l", "m", "f", "g"] {
            commit(&dir, f, &format!("{f}1\n"));
        }
        std::fs::remove_file(dir.join("l")).unwrap();
        std::os::unix::fs::symlink("m", dir.join("l")).unwrap();
        std::fs::write(dir.join("f"), "f2\n").unwrap();
        std::fs::write(dir.join("g"), "g2 \n").unwrap(); // trailing whitespace: apply refuses it
        g(&["config", "apply.whitespace", "error"]);
        let (mut plan, _) = Plan::load(&dir).unwrap();
        let what: Vec<String> = plan.units.iter().map(|u| format!("{} {}", u.path, u.what)).collect();
        assert_eq!(what, ["f modified, hunk 1/1", "g modified, hunk 1/1", "l type changed, whole file"]);
        plan.fill(r#"{"commits":[{"message":"l and f","units":[3,1]},{"message":"g","units":[2]}]}"#).unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, super::super::View::default());
        settle(&ctx, &mut t);
        t.run(&ctx, plan.op(&dir, &git::git_dir(&dir).unwrap()).unwrap());
        settle(&ctx, &mut t);
        assert_eq!(g(&["log", "-1", "--format=%s"]), "l and f\n");
        assert!(g(&["ls-tree", "HEAD", "l"]).starts_with("120000 "));
        assert_eq!(g(&["status", "--porcelain"]), " M g\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
