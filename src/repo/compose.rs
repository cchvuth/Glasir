//! Commit Composer: every uncommitted change as numbered units (tracked diff
//! hunks; untracked, binary and other unsplittable files whole), Claude's plan
//! for them (validated and repaired), the edits the plan dialog makes, and the
//! op that commits the plan. Recompose: the same for the changes of the
//! unpushed commits from one up to HEAD (plus, optionally, the uncommitted ones).

use super::{Msg, Op, RepoTab, Task, Undo};
use crate::ai;
use crate::git::{self, FileDiff, Res};
use std::path::Path;

pub struct Unit {
    pub path: String,
    /// "modified, hunk 2/3" or "new file, whole file".
    pub what: String,
    /// (file, hunk) in `Plan::files`; `None` = the whole file, staged with `git add` (or removed, when deleted).
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
    /// The commit the plan's commits go on top of: HEAD, or the recomposed commits' parent.
    base: String,
    /// The uncommitted changes are units (always for Compose); else the units are `base..HEAD`.
    pub wip: bool,
    /// How many commits a recompose replaces (0: Compose).
    pub replaces: usize,
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

/// What a recompose takes apart: the commits from `oldest` up to HEAD (exactly
/// `picked` when that isn't empty), plus the uncommitted changes when `wip`.
#[derive(Clone, Debug)]
pub struct Recompose {
    pub oldest: String,
    pub picked: Vec<String>,
    pub wip: bool,
}

/// The parent of the range and the range's commits, oldest first, as `- subject` lines.
/// Refused unless they're unpushed, merge-free and nothing is in progress.
fn range(dir: &Path, r: &Recompose) -> Res<(String, usize, String)> {
    if git::state(&git::git_dir(dir)?) != git::State::Clean {
        return Err("Finish or abort the merge, rebase or other operation in progress first.".into());
    }
    let oldest = git::run(dir, &["rev-parse", "-q", "--verify", &format!("{}^{{commit}}", r.oldest)])?.trim().to_owned();
    let base = git::run(dir, &["rev-parse", "-q", "--verify", &format!("{oldest}^")]).map_err(|_| "The first commit can't be recomposed.".to_string())?.trim().to_owned();
    let all = git::run(dir, &["rev-list", "--parents", "HEAD", "--not", &base, "--"])?;
    let ids: Vec<&str> = all.lines().filter_map(|l| l.split(' ').next()).collect();
    if !ids.contains(&oldest.as_str()) || (!r.picked.is_empty() && (r.picked.len() != ids.len() || !ids.iter().all(|i| r.picked.iter().any(|p| p == i)))) {
        return Err("Pick commits in a row on the current branch, ending at HEAD.".into());
    }
    if all.lines().any(|l| l.split(' ').count() > 2) {
        return Err("There's a merge commit in the range; recompose only commits after it.".into());
    }
    if git::run(dir, &["rev-list", "--count", "HEAD", "--not", &base, "--remotes", "--"])?.trim() != ids.len().to_string() {
        return Err("Some of these commits are already pushed; recomposing would rewrite published history.".into());
    }
    // A branch inside the range (a stack's lower branch) would be left on the old commits.
    let inside = git::run(dir, &["for-each-ref", "--merged=HEAD", &format!("--no-merged={base}"), "--format=%(objectname) %(refname:short)", "refs/heads"])?;
    if let Some((_, b)) = inside.lines().filter_map(|l| l.split_once(' ')).find(|(oid, _)| Some(*oid) != ids.first().copied()) {
        return Err(format!("{b} is stacked inside these commits and would be left on the old ones: recompose from {b}'s tip upward, or recompose {b} on its own."));
    }
    let subjects = git::run(dir, &["log", "--reverse", "--format=- %s", "HEAD", "--not", &base, "--"])?;
    Ok((base, ids.len(), subjects))
}

/// The changes the units are made of: `git diff <base>` and the untracked
/// files (`wip`), or `git diff <base> HEAD`. Submodules with only local edits are left out: `git add`
/// can't commit those. Each commit starts from HEAD's tree, so a staged version
/// that differs from the working tree would be lost: refused.
fn changes(dir: &Path, base: &str, wip: bool) -> Res<(String, String)> {
    let diff = ["diff", "--no-color", "--no-ext-diff", "--no-textconv", "--no-renames", "--submodule=short", "--ignore-submodules=dirty", "--src-prefix=a/", "--dst-prefix=b/", base];
    if !wip {
        return Ok((git::run(dir, &[&diff[..], &["HEAD", "--"]].concat())?, String::new()));
    }
    let names = |cached: &[&str]| git::run(dir, &[&["diff", "--name-only", "-z", "--no-renames", "--ignore-submodules=dirty"][..], cached].concat());
    let (staged, unstaged) = (names(&["--cached"])?, names(&[])?);
    if let Some(p) = staged.split('\0').find(|p| !p.is_empty() && unstaged.split('\0').any(|u| u == *p)) {
        return Err(format!("{p} has staged changes that differ from the working tree; commit, stage or unstage them first."));
    }
    Ok((git::run(dir, &[&diff[..], &["--"]].concat())?, git::run(dir, &["ls-files", "-z", "--others", "--exclude-standard"])?))
}

impl Plan {
    /// The units and their listing for Claude (`[id] path (what)` + the diff);
    /// `r`: recompose those commits instead of committing the uncommitted changes.
    pub fn load(dir: &Path, r: Option<&Recompose>) -> Res<(Plan, String)> {
        let head = git::run(dir, &["rev-parse", "-q", "--verify", "HEAD"]).map_err(|_| "Make a first commit first.".to_string())?.trim().to_owned();
        let (base, replaces, mut listing) = match r {
            Some(r) => range(dir, r).map(|(b, n, s)| (b, n, format!("(These changes are now the commits below, oldest first; plan them afresh.)\n{s}\n")))?,
            None => (head.clone(), 0, String::new()),
        };
        let wip = r.is_none_or(|r| r.wip);
        let (raw, untracked) = changes(dir, &base, wip)?;
        let files = git::parse_diff(&raw);
        let to: &[&str] = if wip { &[] } else { &["HEAD"] };
        let names = git::run(dir, &[&["diff", "--name-only", "-z", "--no-renames", "--ignore-submodules=dirty", &base][..], to, &["--"]].concat())?;
        let mut units = vec![];
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
        // Also listed by `diff HEAD` after `git rm --cached`: its deletion unit covers it.
        for path in untracked.split('\0').filter(|p| !p.is_empty() && !names.split('\0').any(|n| n == *p)) {
            let c = git::Change { path: path.into(), old_path: None, code: '?' };
            let f = git::worktree_diff(dir, &c, false, Default::default()).unwrap_or_default();
            let body = if f.binary || f.lossy { &[][..] } else { &f.hunks[..] };
            add(&mut units, path, "new file, untracked".into(), None, body);
        }
        if units.is_empty() {
            return Err(if wip { "There are no uncommitted changes." } else { "These commits change nothing together." }.into());
        }
        Ok((Plan { units, commits: vec![], note: None, files, head, base, wip, replaces, changes: raw + &untracked }, listing))
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
    /// A recompose first moves HEAD to the base (`reset --soft`), takes whole
    /// files from the original HEAD unless `wip`, ends with the index as it
    /// was, and rolls back to where it started if a step fails.
    pub fn op(&self, dir: &Path, git_dir: &Path) -> Res<Op> {
        let state = super::rebase::head_state(dir);
        if !state.ends_with(&format!(" {}", self.head)) {
            return Err("HEAD moved since the plan was made; compose again.".into());
        }
        if changes(dir, &self.base, self.wip).map(|(d, u)| d + &u)? != self.changes {
            return Err("The changes were edited since the plan was made; compose again.".into());
        }
        if self.commits.iter().any(|c| c.message.trim().is_empty()) {
            return Err("Every commit needs a message.".into());
        }
        let n = self.commits.len();
        let s = if n == 1 { "" } else { "s" };
        let mut op = Op::new(format!("Compose {n} commit{s}"), &["read-tree", "--reset", &self.base]);
        // What's staged now: put back at the end (not `wip`) and by a rollback.
        let index = git::run(dir, &["write-tree"]).map_err(|_| "Resolve the conflicts first.".to_string())?.trim().to_owned();
        if self.replaces > 0 {
            op.label = format!("Recompose {} commits into {n}", self.replaces);
            op.cmds.splice(0..0, [vec![super::rebase::HEAD_IS.to_owned(), state], ["reset", "-q", "--soft", &self.base].map(String::from).to_vec()]);
            op.on_fail = vec![["reset", "-q", "--soft", &self.head].map(String::from).to_vec(), ["read-tree", &index].map(String::from).to_vec()];
        }
        let mut done: Vec<usize> = vec![];
        for (k, c) in self.commits.iter().enumerate() {
            done.extend(&c.units);
            let mut patch = String::new();
            for (fi, f) in self.files.iter().enumerate() {
                let sel = |hi: usize| done.iter().any(|&u| self.units[u].hunk == Some((fi, hi)));
                patch += &git::partial_patch(f, |hi, _| sel(hi), false).unwrap_or_default();
            }
            // A deletion may still be on disk (`git rm --cached`): `add` would track it again.
            let whole = |del: bool| -> Vec<&str> { done.iter().map(|&u| &self.units[u]).filter(|u| u.hunk.is_none() && u.what.starts_with("deleted") == del).map(|u| u.path.as_str()).collect() };
            let (whole, gone) = (whole(false), whole(true));
            if k > 0 {
                op = op.then(&["read-tree", "--reset", &self.base]);
            }
            if !patch.is_empty() {
                let file = git_dir.join(format!("glasir-compose-{k}.patch"));
                std::fs::write(&file, patch).map_err(|e| e.to_string())?;
                op = op.then(&["apply", "--cached", &file.to_string_lossy()]);
            }
            if !self.wip {
                // Both from the original HEAD's tree: `reset <tree> -- path` also unstages a deletion.
                let all = [&whole[..], &gone[..]].concat();
                if !all.is_empty() {
                    op = op.then(&[&["--literal-pathspecs", "reset", "-q", &self.head, "--"][..], &all].concat());
                }
            } else if !whole.is_empty() {
                op = op.then(&[&["--literal-pathspecs", "add", "--"][..], &whole].concat());
            }
            if self.wip && !gone.is_empty() {
                op = op.then(&[&["update-index", "--force-remove", "--"][..], &gone].concat());
            }
            let msg = git_dir.join(format!("glasir-compose-{k}.msg"));
            std::fs::write(&msg, c.message.trim()).map_err(|e| e.to_string())?;
            op = op.then(&["commit", "-q", "-F", &msg.to_string_lossy()]);
        }
        if !self.wip {
            op = op.then(&["read-tree", &index]);
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
    pub(super) fn start_compose(&mut self, ctx: &egui::Context, r: Option<Recompose>) {
        let dir = self.dir.clone();
        self.compose = None;
        self.compose_task = Some(Task::spawn(ctx, move || Plan::load(&dir, r.as_ref())));
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
        let mut p = Plan { units: vec![], commits: vec![], note: None, files: vec![], head: String::new(), base: String::new(), wip: true, replaces: 0, changes: String::new() };
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
        let (mut plan, listing) = Plan::load(&dir, None).unwrap();
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

    /// Three unpushed commits (a's two ends apart, a binary file, a deletion) recomposed
    /// into two, leaving the uncommitted changes (staged and not) exactly as they were;
    /// ⌘Z brings the old commits back; pushed commits, merges and gaps are refused; a
    /// failed step rolls back; with `wip` the uncommitted changes are planned too.
    #[test]
    fn recompose_end_to_end() {
        let (dir, remote) = (scratch("recompose"), scratch("recompose-remote"));
        let g = |a: &[&str]| sh(&dir, a);
        let rev = |r: &str| g(&["rev-parse", r]).trim().to_owned();
        let r = |oldest: &str, picked: &[&str], wip| Recompose { oldest: oldest.into(), picked: picked.iter().map(|s| s.to_string()).collect(), wip };
        sh(&remote, &["init", "-q", "--bare", "-b", "main"]);
        g(&["init", "-q", "-b", "main"]);
        identity(&dir);
        let lines = |ch: &[usize]| (1..=20).map(|i| if ch.contains(&i) { format!("A{i}\n") } else { format!("a{i}\n") }).collect::<String>();
        commit(&dir, "a", &lines(&[]));
        commit(&dir, "gone", "old\n");
        g(&["remote", "add", "origin", &remote.to_string_lossy()]);
        g(&["push", "-q", "-u", "origin", "main"]);
        commit(&dir, "a", &lines(&[2]));
        let c1 = rev("HEAD");
        std::fs::write(dir.join("bin"), b"\0\x01").unwrap();
        g(&["add", "bin"]);
        g(&["rm", "-q", "gone"]);
        g(&["commit", "-q", "-m", "bin, no gone"]);
        commit(&dir, "a", &lines(&[2, 18]));
        let (c3, tree) = (rev("HEAD"), rev("HEAD^{tree}"));
        // Uncommitted: a staged new file and an unstaged edit of a.
        std::fs::write(dir.join("s"), "staged\n").unwrap();
        g(&["add", "s"]);
        std::fs::write(dir.join("a"), lines(&[2, 10, 18])).unwrap();
        let wip = || (g(&["diff", "--cached"]), g(&["diff"]), g(&["status", "--porcelain"]));
        let before = wip();

        let refused = |r: &Recompose, why: &str| assert!(Plan::load(&dir, Some(r)).is_err_and(|e| e.contains(why)), "{why}");
        refused(&r("HEAD~3", &[], false), "pushed");
        refused(&r(&c1, &[&c3, &c1], false), "in a row");
        refused(&r(&rev("HEAD~4"), &[], false), "first commit");
        std::fs::write(git::git_dir(&dir).unwrap().join("MERGE_HEAD"), &c3).unwrap();
        refused(&r(&c1, &[], false), "in progress");
        std::fs::remove_file(git::git_dir(&dir).unwrap().join("MERGE_HEAD")).unwrap();
        // A branch inside the range would be left on the old commits; one at HEAD is a copy.
        g(&["branch", "below", "HEAD~1"]);
        g(&["branch", "copy"]);
        refused(&r(&c1, &[], false), "below is stacked");
        g(&["branch", "-D", "below"]);

        let (mut plan, listing) = Plan::load(&dir, Some(&r(&c1, &[&c3, &rev("HEAD~1"), &c1], false))).unwrap();
        let what: Vec<String> = plan.units.iter().map(|u| format!("{} {}", u.path, u.what)).collect();
        assert_eq!(what, ["a modified, hunk 1/2", "a modified, hunk 2/2", "bin new file, binary, whole file", "gone deleted, hunk 1/1"]);
        assert!(listing.starts_with("(These changes are now the commits below") && listing.contains("- bin, no gone\n"), "{listing}");
        g(&["branch", "-D", "copy"]);
        plan.fill(r#"{"commits":[{"message":"Drop gone, a18","units":[4,2]},{"message":"Rest","units":[1,3]}]}"#).unwrap();
        let op = plan.op(&dir, &git::git_dir(&dir).unwrap()).unwrap();
        assert_eq!((op.label.as_str(), op.undo), ("Recompose 3 commits into 2", Undo::Soft));
        let snap = git::snapshot(&dir).unwrap();
        super::super::op::exec(&dir, &op).unwrap();
        assert_eq!(g(&["log", "--format=%s", "origin/main..HEAD"]), "Rest\nDrop gone, a18\n");
        assert_eq!(g(&["show", "--format=", "--name-status", "HEAD~1"]), "M\ta\nD\tgone\n");
        assert!(!g(&["show", "HEAD~1"]).contains("+A2"));
        assert_eq!(rev("HEAD^{tree}"), tree, "same end result");
        assert_eq!(wip(), before, "uncommitted changes untouched");
        // Undo (the op's `Undo::Soft`): the old commits, with nothing else changed.
        git::restore(&dir, &snap, Undo::Soft.mode()).unwrap();
        assert_eq!((rev("HEAD"), wip()), (c3.clone(), before.clone()));
        // A failed step (apply refuses trailing whitespace) rolls everything back.
        g(&["config", "apply.whitespace", "error"]);
        std::fs::write(dir.join("w"), "w \n").unwrap();
        g(&["add", "w"]);
        g(&["commit", "-q", "-m", "w", "--", "w"]); // `s` stays staged
        let (mut plan, _) = Plan::load(&dir, Some(&r(&c1, &[], false))).unwrap();
        plan.fill(r#"{"commits":[{"message":"one","units":[1]},{"message":"two","units":[2,3,4,5]}]}"#).unwrap();
        let (head, staged) = (rev("HEAD"), g(&["diff", "--cached"]));
        assert!(super::super::op::exec(&dir, &plan.op(&dir, &git::git_dir(&dir).unwrap()).unwrap()).is_err_and(|e| e.contains("Rolled back")));
        assert_eq!((rev("HEAD"), g(&["diff", "--cached"]), g(&["log", "-1", "--format=%s"])), (head, staged, "w\n".into()));
        assert!(g(&["diff", "--cached"]).contains("+staged"));
        g(&["config", "--unset", "apply.whitespace"]);
        let (mut plan, _) = Plan::load(&dir, Some(&r(&c1, &[], true))).unwrap();
        assert!(plan.units.iter().any(|u| u.path == "s"));
        plan.fill(r#"{"commits":[{"message":"All of it","units":[]}]}"#).unwrap();
        super::super::op::exec(&dir, &plan.op(&dir, &git::git_dir(&dir).unwrap()).unwrap()).unwrap();
        assert_eq!((g(&["log", "--format=%s", "origin/main..HEAD"]), g(&["status", "--porcelain"])), ("Remaining changes\n".into(), String::new()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&remote);
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
        let (plan, _) = Plan::load(&dir, None).unwrap();
        assert_eq!(plan.units.iter().map(|u| u.path.as_str()).collect::<Vec<_>>(), ["x"]);
        std::fs::write(dir.join("new"), "precious\n").unwrap();
        g(&["add", "new"]);
        std::fs::remove_file(dir.join("new")).unwrap();
        assert!(Plan::load(&dir, None).is_err_and(|e| e.starts_with("new has staged changes")));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&sub);
    }

    /// Staged content that differs from the working tree would be dropped by
    /// `read-tree --reset`: refused when planning and when committing.
    #[test]
    fn compose_refuses_staged_content_not_in_the_worktree() {
        let dir = scratch("compose-split-stage");
        let g = |a: &[&str]| sh(&dir, a);
        g(&["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "f", "a\n");
        std::fs::write(dir.join("f"), "a\nstaged-only precious\n").unwrap();
        g(&["add", "f"]);
        std::fs::write(dir.join("f"), "a\nworktree\n").unwrap();
        assert!(Plan::load(&dir, None).is_err_and(|e| e.starts_with("f has staged changes")));
        // Staged after the plan was made: the op refuses.
        g(&["add", "f"]);
        let (mut plan, _) = Plan::load(&dir, None).unwrap();
        plan.fill(r#"{"commits":[{"message":"one","units":[1]}]}"#).unwrap();
        std::fs::write(dir.join("f"), "a\nstaged-only precious\n").unwrap();
        g(&["add", "f"]);
        std::fs::write(dir.join("f"), "a\nworktree\n").unwrap();
        assert!(plan.op(&dir, &git::git_dir(&dir).unwrap()).is_err_and(|e| e.starts_with("f has staged changes")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file untracked with `git rm --cached` is one deletion unit, committed
    /// as removed and left on disk.
    #[test]
    fn compose_keeps_a_file_untracked() {
        let dir = scratch("compose-rm-cached");
        let g = |a: &[&str]| sh(&dir, a);
        g(&["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "secret.env", "k=v\n");
        g(&["rm", "-q", "--cached", "secret.env"]);
        let (mut plan, _) = Plan::load(&dir, None).unwrap();
        let what: Vec<String> = plan.units.iter().map(|u| format!("{} {}", u.path, u.what)).collect();
        assert_eq!(what, ["secret.env deleted, hunk 1/1"]);
        plan.fill(r#"{"commits":[{"message":"Untrack secret","units":[1]}]}"#).unwrap();
        super::super::op::exec(&dir, &plan.op(&dir, &git::git_dir(&dir).unwrap()).unwrap()).unwrap();
        assert_eq!(g(&["ls-tree", "--name-only", "HEAD"]), "");
        assert_eq!(std::fs::read_to_string(dir.join("secret.env")).unwrap(), "k=v\n");
        // Whole-file deletions (binary, empty) stay untracked too, next to another change.
        std::fs::remove_file(dir.join("secret.env")).unwrap();
        std::fs::write(dir.join("key.bin"), b"\0\x01").unwrap();
        std::fs::write(dir.join(".token"), "").unwrap();
        g(&["add", "key.bin", ".token"]);
        g(&["commit", "-q", "-m", "keys"]);
        g(&["rm", "-q", "--cached", "key.bin", ".token"]);
        std::fs::write(dir.join("o"), "o\n").unwrap();
        let (mut plan, _) = Plan::load(&dir, None).unwrap();
        let what: Vec<String> = plan.units.iter().map(|u| format!("{} {}", u.path, u.what)).collect();
        assert_eq!(what, [".token deleted, whole file", "key.bin deleted, binary, whole file", "o new file, untracked"]);
        plan.fill(r#"{"commits":[{"message":"o","units":[3]},{"message":"Untrack keys","units":[1,2]}]}"#).unwrap();
        super::super::op::exec(&dir, &plan.op(&dir, &git::git_dir(&dir).unwrap()).unwrap()).unwrap();
        assert_eq!(g(&["ls-tree", "--name-only", "HEAD"]), "o\n");
        assert_eq!(g(&["show", "--format=", "--name-status", "HEAD"]), "D\t.token\nD\tkey.bin\n");
        assert_eq!(std::fs::read(dir.join("key.bin")).unwrap(), b"\0\x01");
        let _ = std::fs::remove_dir_all(&dir);
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
        let (mut plan, _) = Plan::load(&dir, None).unwrap();
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
