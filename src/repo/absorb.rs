//! Absorb: commit each staged hunk as a fixup of the unpushed commit that last
//! touched its lines, then autosquash. [`Plan`] is the dry run shown first;
//! hunks without one clear target stay staged.

use super::{Msg, Op, RepoTab, Task, Undo};
use crate::git::{self, FileDiff, Res};
use std::collections::HashMap;
use std::path::Path;

pub struct Row {
    pub path: String,
    /// "line 12: +3 −1"
    pub what: String,
    /// Index into `Plan::cands`, or why the hunk stays staged.
    pub target: Result<usize, String>,
    /// (file, hunk) in `Plan::files`; `None` for a file without text hunks.
    at: Option<(usize, usize)>,
}

pub struct Plan {
    pub rows: Vec<Row>,
    /// Commits hunks may go to: (id, summary), newest first.
    pub cands: Vec<(String, String)>,
    files: Vec<FileDiff>,
    head: String,
    /// The staged tree: put back as the index at the end, so leftovers stay staged.
    index: String,
}

fn short(id: &str) -> &str {
    &id[..id.len().min(7)]
}

/// Final line number -> commit, from `git blame --porcelain`.
fn parse_blame(out: &str) -> HashMap<u32, String> {
    let mut m = HashMap::new();
    for l in out.lines().filter(|l| !l.starts_with('\t')) {
        let f: Vec<&str> = l.split(' ').collect();
        if (3..=4).contains(&f.len()) && f[0].len() >= 40 && f[0].bytes().all(|b| b.is_ascii_hexdigit())
            && let Ok(n) = f[2].parse()
        {
            m.insert(n, f[0].to_owned());
        }
    }
    m
}

/// Which candidate (index, newest first) a hunk belongs to. `removed`: commits
/// owning its removed lines; `around`: commits owning the lines just above and below.
fn attribute(removed: &[&str], around: &[&str], cands: &[&str]) -> Result<usize, String> {
    let pos = |c: &str| cands.iter().position(|x| *x == c);
    let near = around.iter().filter_map(|c| pos(c)).min();
    let Some(&first) = removed.first() else {
        // Pure addition: the newer of its neighbours.
        return near.ok_or_else(|| "the lines around it aren't from an unpushed commit".into());
    };
    if removed.iter().any(|c| *c != first) {
        return Err("its lines come from several commits".into());
    }
    let t = pos(first).ok_or_else(|| format!("its lines are from {}, which is pushed or too old", short(first)))?;
    // Moving the fix below a newer change right next to it would conflict.
    match near.filter(|&n| n < t) {
        Some(n) => Err(format!("it touches lines changed later in {}", short(cands[n]))),
        None => Ok(t),
    }
}

impl Plan {
    pub fn load(dir: &Path) -> Res<Plan> {
        if git::state(&git::git_dir(dir)?) != git::State::Clean {
            return Err("Finish or abort the merge, rebase or other operation in progress first.".into());
        }
        let head = git::run(dir, &["rev-parse", "HEAD"])?.trim().to_owned();
        let index = git::run(dir, &["write-tree"]).map_err(|_| "Resolve the conflicts first.".to_string())?.trim().to_owned();
        // Unpushed commits (no upstream: the last 20 on no remote), stopping at a merge or the root.
        let range = git::run(dir, &["log", "--format=%H%x1f%P%x1f%s", "@{upstream}..HEAD", "--"])
            .or_else(|_| git::run(dir, &["log", "--format=%H%x1f%P%x1f%s", "-n", "20", "HEAD", "--not", "--remotes", "--"]))?;
        let cands: Vec<(String, String)> = range
            .lines()
            .map(|l| l.splitn(3, '\x1f').collect::<Vec<_>>())
            .take_while(|f| f.len() == 3 && f[1].split(' ').count() == 1 && !f[1].is_empty())
            .map(|f| (f[0].to_owned(), f[2].to_owned()))
            .collect();
        let ids: Vec<&str> = cands.iter().map(|c| c.0.as_str()).collect();
        let diff = ["diff", "--cached", "-U0", "--no-color", "--no-ext-diff", "--no-textconv", "--no-renames", "--submodule=short", "--src-prefix=a/", "--dst-prefix=b/"];
        let files = git::parse_diff(&git::run(dir, &diff)?);
        let names = git::run(dir, &["diff", "--cached", "--name-only", "-z", "--no-renames"])?;
        let mut rows = vec![];
        for (fi, (f, named)) in files.iter().zip(git::file_paths(&files, &names)).enumerate() {
            let Some((path, typechange)) = named else { continue };
            let stay = |why: &str| Row { path: path.into(), what: String::new(), target: Err(why.into()), at: None };
            let new = f.header.iter().any(|h| h.starts_with("new file"));
            // Whole deletions stay too: the fixup could empty the commit that added the
            // file, and a rename's two halves stay together.
            let gone = f.header.iter().any(|h| h.starts_with("deleted file"));
            if typechange || f.hunks.is_empty() || f.binary || new || gone || f.hunks.iter().flat_map(|h| &h.lines).any(|l| l.text.contains('\u{FFFD}')) {
                let why = if typechange { "type change" } else if new { "new file" } else if gone { "deleted file" } else if f.hunks.is_empty() || f.binary { "binary or mode change" } else { "not UTF-8 text" };
                rows.push(stay(why));
                continue;
            }
            // Removed lines, plus the one above and below, all in one blame.
            let span = |h: &git::Hunk| {
                let n = h.lines.iter().filter(|l| l.kind == b'-').count() as u32;
                let start = if n == 0 { h.old_start } else { h.old_start - 1 };
                (start.max(1), h.old_start + n.max(1))
            };
            let mut args = vec!["blame".to_owned(), "--porcelain".into()];
            for h in &f.hunks {
                let (a, b) = span(h);
                args.extend(["-L".into(), format!("{a},{b}")]);
            }
            args.extend(["HEAD".into(), "--".into(), path.into()]);
            let blame = match git::run(dir, &args.iter().map(String::as_str).collect::<Vec<_>>()) {
                Ok(b) => parse_blame(&b),
                Err(_) => {
                    rows.push(stay("git blame failed"));
                    continue;
                }
            };
            for (hi, h) in f.hunks.iter().enumerate() {
                let (minus, plus) = (h.lines.iter().filter(|l| l.kind == b'-').count(), h.lines.iter().filter(|l| l.kind == b'+').count());
                let owner = |n: u32| blame.get(&n).map(String::as_str);
                let removed: Vec<&str> = (0..minus as u32).filter_map(|i| owner(h.old_start + i)).collect();
                let (above, below) = if minus == 0 { (h.old_start, h.old_start + 1) } else { (h.old_start - 1, h.old_start + minus as u32) };
                let around: Vec<&str> = [above, below].into_iter().filter(|&n| n > 0).filter_map(owner).collect();
                let line = if minus == 0 { h.new_start } else { h.old_start };
                rows.push(Row {
                    path: path.into(),
                    what: format!("line {line}: +{plus} −{minus}"),
                    target: attribute(&removed, &around, &ids),
                    at: Some((fi, hi)),
                });
            }
        }
        if rows.is_empty() {
            return Err("Nothing is staged.".into());
        }
        // A fixup that exactly reverts its commit would empty it, which stops the autosquash.
        let pid = |patch: &str| git::run_input(dir, &["patch-id", "--stable"], Some(patch.as_bytes())).map(|o| o.split(' ').next().unwrap_or("").to_owned());
        let mut ts: Vec<usize> = rows.iter().filter_map(|r| r.target.clone().ok()).collect();
        ts.sort_unstable();
        ts.dedup();
        for t in ts {
            let mine = |r: &Row| r.target == Ok(t);
            let patch: String = files.iter().enumerate().filter_map(|(fi, f)| git::partial_patch(f, |hi, _| rows.iter().any(|r| r.at == Some((fi, hi)) && mine(r)), false)).collect();
            let id = &cands[t].0;
            let revert = git::run(dir, &[&diff[..1], &diff[2..], &[id.as_str(), &format!("{id}^")]].concat())?;
            if pid(&patch)? == pid(&revert)? {
                rows.iter_mut().filter(|r| mine(r)).for_each(|r| r.target = Err(format!("it would undo all of {}", short(id))));
            }
        }
        Ok(Plan { rows, cands, files, head, index })
    }

    /// Candidates that receive hunks, oldest first.
    pub fn targets(&self) -> Vec<usize> {
        let mut t: Vec<usize> = self.rows.iter().filter_map(|r| r.target.as_ref().ok().copied()).collect();
        t.sort_unstable_by(|a, b| b.cmp(a));
        t.dedup();
        t
    }

    /// One fixup commit per target (each built on the original HEAD's tree plus
    /// the hunks so far), autosquash, then the staged tree back as the index.
    /// Undo (`--soft`) restores HEAD and leaves the original index.
    pub fn op(&self, dir: &Path, git_dir: &Path) -> Res<Op> {
        let changed = git::run(dir, &["rev-parse", "HEAD"])?.trim() != self.head || git::run(dir, &["write-tree"]).unwrap_or_default().trim() != self.index;
        if changed {
            return Err("HEAD or the staged changes changed since the preview; run Absorb again.".into());
        }
        let targets = self.targets();
        let oldest = *targets.first().ok_or("No hunk has a clear target commit.")?;
        super::rebase::pending_autosquash(self.cands[..=oldest].iter().map(|c| c.1.as_str()))?;
        let n = targets.len();
        let mut op = Op::new(format!("Absorb into {n} commit{}", if n == 1 { "" } else { "s" }), &["read-tree", "--reset", &self.head]);
        let rank = |t: usize| targets.iter().position(|&x| x == t).unwrap();
        for (k, &t) in targets.iter().enumerate() {
            let mut patch = String::new();
            for (fi, f) in self.files.iter().enumerate() {
                let upto = |hi: usize| self.rows.iter().any(|r| r.at == Some((fi, hi)) && r.target.as_ref().is_ok_and(|&t| rank(t) <= k));
                patch += &git::partial_patch(f, |hi, _| upto(hi), false).unwrap_or_default();
            }
            let file = git_dir.join(format!("glasir-absorb-{k}.patch"));
            std::fs::write(&file, patch).map_err(|e| e.to_string())?;
            let file = file.to_string_lossy();
            if k > 0 {
                op = op.then(&["read-tree", "--reset", &self.head]);
            }
            op = op
                .then(&["apply", "--cached", "--unidiff-zero", &file])
                // `fixup! <sha>`: `--fixup` writes the subject, which autosquash matches to the oldest commit with it.
                .then(&["commit", "-q", "--no-verify", "-m", &format!("fixup! {}", self.cands[t].0)]);
        }
        // No commutation check beyond adjacent lines. A failure (a conflicting autosquash)
        // rolls everything back: leftovers' staged versions may exist only in `self.index`.
        // Nothing before the rebase touches the worktree, and `--abort` reapplies its autostash.
        let base = format!("{}^", self.cands[oldest].0);
        let mut op = op
            .then(&["-c", "sequence.editor=true", "-c", "core.editor=true", "rebase", "-q", "-i", "--autosquash", "--autostash", &base])
            .then(&["read-tree", "--reset", &self.index])
            .undo(Undo::Soft);
        op.on_fail = [&[super::rebase::ABORT_ANY][..], &["reset", "-q", "--soft", &self.head], &["read-tree", "--reset", &self.index]]
            .iter()
            .map(|a| a.iter().map(|s| s.to_string()).collect())
            .collect();
        Ok(op)
    }
}

impl RepoTab {
    /// Plan in the background; the preview dialog opens when it's ready.
    pub(super) fn start_absorb(&mut self, ctx: &egui::Context) {
        let dir = self.dir.clone();
        self.absorb_task = Some(Task::spawn(ctx, move || Plan::load(&dir)));
    }

    pub(super) fn poll_absorb(&mut self) {
        match Task::poll(&mut self.absorb_task) {
            Some(Ok(p)) => self.absorb = Some(p),
            Some(Err(e)) => self.notes.push(Msg { text: format!("Absorb: {e}"), error: true }),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::testutil::{identity, scratch, sh};

    #[test]
    fn blame_porcelain() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let out = format!("{a} 1 1 2\nauthor T\nsummary x y z\n\tone\n{a} 2 2\n\ttwo\n{b} 5 3 1\nfilename f\n\tthree\n");
        let m = parse_blame(&out);
        assert_eq!((m[&1].as_str(), m[&2].as_str(), m[&3].as_str()), (a.as_str(), a.as_str(), b.as_str()));
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn attribution_rules() {
        let c = ["c3", "c2", "c1"]; // newest first
        assert_eq!(attribute(&["c2", "c2"], &["c1", "c2"], &c), Ok(1));
        assert!(attribute(&["c2", "c1"], &[], &c).is_err()); // several owners
        assert!(attribute(&["old"], &[], &c).is_err()); // pushed
        assert!(attribute(&["c1"], &["c3"], &c).is_err()); // next to a later change
        assert_eq!(attribute(&[], &["c1", "c3"], &c), Ok(0)); // addition: newer neighbour
        assert_eq!(attribute(&[], &["old", "c2"], &c), Ok(1));
        assert!(attribute(&[], &["old"], &c).is_err());
        assert!(attribute(&[], &[], &c).is_err());
    }

    fn lines(n: usize, f: impl Fn(usize) -> String) -> String {
        (1..=n).map(|i| f(i) + "\n").collect()
    }

    /// 3 commits, 4 hunks to the right commits, an ambiguous one stays staged;
    /// undo (soft reset) gives back the original index.
    #[test]
    fn absorb_end_to_end() {
        let dir = scratch("absorb");
        let g = |a: &[&str]| sh(&dir, a);
        g(&["init", "-q", "-b", "main"]);
        identity(&dir);
        let write = |f: &str, s: &str| std::fs::write(dir.join(f), s).unwrap();
        let base = |i: usize| format!("l{i}");
        write("README", "root\n"); // the root commit can't be rewritten
        g(&["add", "README"]);
        g(&["commit", "-q", "-m", "init"]);
        write("f", &lines(20, base));
        g(&["add", "f"]);
        g(&["commit", "-q", "-m", "c1"]);
        write("f", &lines(20, |i| if i == 3 { "c2-3".into() } else { base(i) }));
        write("g", "g1\ng2\n");
        g(&["add", "f", "g"]);
        g(&["commit", "-q", "-m", "c2"]);
        let v3 = |i: usize| match i {
            3 => "c2-3".into(),
            10 | 16 => format!("c3-{i}"),
            _ => base(i),
        };
        write("f", &lines(20, v3));
        write("other", "x\n");
        g(&["add", "f", "other"]);
        g(&["commit", "-q", "-m", "c3"]);
        let ids: Vec<String> = g(&["rev-list", "HEAD"]).lines().map(String::from).collect();
        let orig = ids[0].clone();

        // Staged: line 3 (c2), line 6 (c1), line 10 (c3), g (c2), lines 16-17 (c3 + c1: ambiguous).
        let staged = |i: usize| match i {
            3 | 6 | 10 => format!("fix-{i}"),
            16 | 17 => format!("both-{i}"),
            _ => v3(i),
        };
        write("f", &lines(20, staged));
        write("g", "g1\ng2 fixed\n");
        g(&["add", "f", "g"]);
        write("other", "unstaged\n"); // must survive
        let before = g(&["diff", "--cached"]);

        let plan = Plan::load(&dir).unwrap();
        let got: Vec<(String, Result<String, ()>)> = plan
            .rows
            .iter()
            .map(|r| (format!("{} {}", r.path, r.what), r.target.as_ref().map(|&t| plan.cands[t].1.clone()).map_err(|_| ())))
            .collect();
        let ok = |s: &str, c: &str| (s.to_owned(), Ok(c.to_owned()));
        assert_eq!(
            got,
            [
                ok("f line 3: +1 −1", "c2"),
                ok("f line 6: +1 −1", "c1"),
                ok("f line 10: +1 −1", "c3"),
                ("f line 16: +2 −2".into(), Err(())),
                ok("g line 2: +1 −1", "c2"),
            ]
        );
        let snap = git::snapshot(&dir).unwrap();
        let op = plan.op(&dir, &git::git_dir(&dir).unwrap()).unwrap();
        crate::repo::op::exec(&dir, &op).unwrap();

        assert_eq!(g(&["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["c3", "c2", "c1", "init"]);
        let show = |rev: &str| g(&["show", rev]);
        assert!(show("HEAD~2:f").contains("fix-6") && !show("HEAD~2:f").contains("fix-3"));
        assert!(show("HEAD~1:f").contains("fix-3") && !show("HEAD~1:f").contains("fix-10"));
        assert_eq!(show("HEAD~1:g"), "g1\ng2 fixed\n");
        assert!(show("HEAD:f").contains("fix-10") && !show("HEAD:f").contains("both-16"));
        // The ambiguous hunk is still staged; the unstaged edit survived.
        let left = g(&["diff", "--cached"]);
        assert!(left.contains("+both-16") && !left.contains("fix-"), "{left}");
        assert_eq!(std::fs::read_to_string(dir.join("other")).unwrap(), "unstaged\n");

        git::restore(&dir, &snap, "--soft").unwrap();
        assert_eq!(g(&["rev-parse", "HEAD"]).trim(), orig);
        assert_eq!(g(&["diff", "--cached"]), before);

        // With an upstream at c1, c1 is pushed: its hunk stays staged.
        g(&["remote", "add", "origin", "https://example.invalid/r.git"]);
        g(&["update-ref", "refs/remotes/origin/main", &ids[2]]);
        g(&["config", "branch.main.remote", "origin"]);
        g(&["config", "branch.main.merge", "refs/heads/main"]);
        let plan = Plan::load(&dir).unwrap();
        assert_eq!(plan.cands.len(), 2);
        assert!(plan.rows[1].target.as_ref().is_err_and(|e| e.contains("pushed")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A conflicting autosquash rolls back to the original HEAD, index and worktree,
    /// so a staged-only version of a leftover file isn't lost; the user's own
    /// pending squash! commit is refused rather than folded in.
    #[test]
    fn conflict_rolls_back_and_pending_squash_refused() {
        let dir = scratch("absorb-conflict");
        let g = |a: &[&str]| sh(&dir, a);
        g(&["init", "-q", "-b", "main"]);
        identity(&dir);
        let write = |f: &str, s: &str| std::fs::write(dir.join(f), s).unwrap();
        write("README", "root\n");
        g(&["add", "README"]);
        g(&["commit", "-q", "-m", "init"]);
        let ten = lines(10, |i| format!("l{i}"));
        write("f", &ten);
        g(&["add", "f"]);
        g(&["commit", "-q", "-m", "c1"]);
        write("f", &ten.replace("l5\n", ""));
        g(&["commit", "-q", "-am", "c2"]);
        // HEAD's blame can't see the deleted l5, so the fix goes to c1 and conflicts.
        write("f", &ten.replace("l5\n", "").replace("l4\n", "l4 fixed\n"));
        write("README", "root\nroot staged\n");
        g(&["add", "f", "README"]);
        write("README", "root\nroot staged, then edited more\n");
        let (head, index) = (g(&["rev-parse", "HEAD"]), g(&["write-tree"]));
        let plan = Plan::load(&dir).unwrap();
        assert_eq!(plan.rows.iter().map(|r| r.target.is_ok()).collect::<Vec<_>>(), [false, true]);
        assert!(crate::repo::op::exec(&dir, &plan.op(&dir, &git::git_dir(&dir).unwrap()).unwrap()).is_err());
        assert_eq!(git::state(&git::git_dir(&dir).unwrap()), git::State::Clean);
        assert_eq!((g(&["rev-parse", "HEAD"]), g(&["write-tree"])), (head.clone(), index.clone()));
        assert_eq!(std::fs::read_to_string(dir.join("README")).unwrap(), "root\nroot staged, then edited more\n");
        // A fixup commit that fails (before any rebase) rolls back too.
        g(&["config", "commit.gpgSign", "true"]);
        g(&["config", "gpg.program", "false"]);
        let e = crate::repo::op::exec(&dir, &plan.op(&dir, &git::git_dir(&dir).unwrap()).unwrap()).unwrap_err();
        assert!(e.contains("Rolled back"), "{e}");
        assert_eq!((g(&["rev-parse", "HEAD"]), g(&["write-tree"])), (head, index));
        g(&["config", "--unset", "commit.gpgSign"]);

        g(&["reset", "-q", "--hard"]);
        write("g", "wip\n");
        g(&["add", "g"]);
        g(&["commit", "-q", "-m", "squash! c1"]);
        write("f", &ten.replace("l5\n", "").replace("l1\n", "l1 fixed\n"));
        g(&["add", "f"]);
        let plan = Plan::load(&dir).unwrap();
        assert!(plan.op(&dir, &git::git_dir(&dir).unwrap()).is_err_and(|e| e.contains("squash!")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pure insertions (middle and end of a file) land where they were staged; two
    /// commits with one subject each get their own fixup; with no upstream a commit
    /// on a remote branch is no target.
    #[test]
    fn insertions_same_subjects_no_upstream() {
        let dir = scratch("absorb-ins");
        let g = |a: &[&str]| sh(&dir, a);
        g(&["init", "-q", "-b", "main"]);
        identity(&dir);
        let write = |f: &str, s: &str| std::fs::write(dir.join(f), s).unwrap();
        for (f, text, msg) in [("README", "root\n", "init"), ("p", "p1\n", "published")] {
            write(f, text);
            g(&["add", f]);
            g(&["commit", "-q", "-m", msg]);
        }
        g(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        g(&["checkout", "-q", "-b", "feature"]);
        let ten = lines(10, |i| format!("l{i}"));
        for (f, text) in [("f", ten.as_str()), ("g", "g1\n")] {
            write(f, text);
            g(&["add", f]);
            g(&["commit", "-q", "-m", "wip"]);
        }
        let fixed = ten.replace("l5\n", "l5\nadded\n") + "eleven\n";
        write("f", &fixed);
        write("g", "g1 fixed\n");
        write("p", "p1 fixed\n");
        g(&["add", "f", "g", "p"]);
        g(&["sparse-checkout", "set", "--no-cone", "/*", "!/README"]); // must stay out of the checkout

        let plan = Plan::load(&dir).unwrap();
        assert_eq!(plan.cands.len(), 2);
        let got: Vec<(String, Result<usize, ()>)> = plan.rows.iter().map(|r| (format!("{} {}", r.path, r.what), r.target.clone().map_err(|_| ()))).collect();
        assert_eq!(got, [("f line 6: +1 −0".into(), Ok(1)), ("f line 12: +1 −0".into(), Ok(1)), ("g line 1: +1 −1".into(), Ok(0)), ("p line 1: +1 −1".into(), Err(()))]);
        crate::repo::op::exec(&dir, &plan.op(&dir, &git::git_dir(&dir).unwrap()).unwrap()).unwrap();
        assert_eq!(g(&["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["wip", "wip", "published", "init"]);
        assert_eq!(g(&["show", "HEAD~1:f"]), fixed);
        assert_eq!(g(&["show", "HEAD:g"]), "g1 fixed\n");
        assert_eq!(g(&["diff", "--cached", "--name-only"]), "p\n");
        assert_eq!((g(&["ls-files", "-t", "README"]), g(&["status", "--porcelain"])), ("S README\n".into(), "M  p\n".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fixups that would empty their commit stay staged (a deleted file, an exact
    /// revert); nothing is planned during a merge.
    #[test]
    fn emptying_fixups_and_merges() {
        let dir = scratch("absorb-empty");
        let g = |a: &[&str]| sh(&dir, a);
        g(&["init", "-q", "-b", "main"]);
        identity(&dir);
        let write = |f: &str, s: &str| std::fs::write(dir.join(f), s).unwrap();
        for (f, text, msg) in [("README", "root\n", "init"), ("x", "a\nb\n", "add x"), ("README", "root\nmore\n", "add more"), ("y", "y\n", "add y")] {
            write(f, text);
            g(&["add", f]);
            g(&["commit", "-q", "-m", msg]);
        }
        g(&["mv", "x", "z"]);
        write("README", "root\n");
        g(&["add", "README"]);
        let plan = Plan::load(&dir).unwrap();
        let got: Vec<(String, String)> = plan.rows.iter().map(|r| (r.path.clone(), r.target.clone().unwrap_err())).collect();
        let row = |p: &str, why: &str| (p.to_owned(), why.to_owned());
        let more = g(&["rev-parse", "--short=7", "HEAD~1"]);
        assert_eq!(got, [row("README", &format!("it would undo all of {}", more.trim())), row("x", "deleted file"), row("z", "new file")]);

        g(&["reset", "-q", "--hard"]);
        g(&["checkout", "-q", "-b", "side"]);
        write("s", "s\n");
        g(&["add", "s"]);
        g(&["commit", "-q", "-m", "side"]);
        g(&["checkout", "-q", "main"]);
        g(&["merge", "-q", "--no-ff", "--no-commit", "side"]);
        assert!(Plan::load(&dir).is_err_and(|e| e.contains("merge")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
