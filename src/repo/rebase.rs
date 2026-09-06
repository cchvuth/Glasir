//! Interactive rebase engine. Builds a todo list ([`Editor`]) and hands it to
//! git's own `rebase -i` (via `sequence.editor`) as an [`Op`], so conflicts,
//! --continue/--abort and hooks behave exactly like on the command line; plus
//! one-shot rewrites (quick, multi, amend_into, split) and interactive
//! cherry-pick ([`Editor::cherry_pick`]). The window is `crate::ui::rebase_editor`.

use crate::git::{self, Res};
use super::{Op, Undo};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Act {
    Pick,
    Reword,
    Squash,
    Fixup,
    Drop,
    /// Stop after picking (used by `split`; not offered in the editor).
    Edit,
}

pub struct Item {
    pub id: String,
    pub message: String,
    pub author: String,
    pub act: Act,
}

pub struct Editor {
    /// Commits after `base` are rewritten; shown newest first like the graph.
    pub base: String,
    pub label: String,
    pub items: Vec<Item>,
    pub(crate) sel: usize,
    /// Cherry-pick mode: `items` come from elsewhere and are picked onto HEAD (`base`).
    pub cherry: bool,
    /// HEAD (symbolic ref and commit) when loaded; the todo is only valid for it.
    head: String,
    dir: std::path::PathBuf,
}

impl Editor {
    /// `base` need not be an ancestor of HEAD: `base..HEAD` is what `rebase -i <base>` replays onto it.
    pub fn load(dir: &Path, base: &str, label: &str) -> Res<Editor> {
        let items = items(dir, &[&format!("{base}..HEAD")])?;
        if items.is_empty() {
            return Err(format!("No commits after {label} on this branch."));
        }
        Ok(Editor { base: base.into(), label: label.into(), items, sel: 0, cherry: false, head: head_state(dir), dir: dir.into() })
    }

    /// Cherry-pick `ids` (newest first) onto HEAD with pick/reword/squash/drop and
    /// reordering: an interactive rebase of HEAD onto itself whose todo picks them.
    pub fn cherry_pick(dir: &Path, ids: &[String]) -> Res<Editor> {
        let head = git::run(dir, &["rev-parse", "HEAD"])?.trim().to_owned();
        let label = git::run(dir, &["symbolic-ref", "-q", "--short", "HEAD"]).map_or_else(|_| "HEAD".into(), |b| b.trim().to_owned());
        let mut args = vec!["--no-walk=unsorted"];
        args.extend(ids.iter().map(String::as_str));
        let items = items(dir, &args)?;
        Ok(Editor { base: head, label, items, sel: 0, cherry: true, head: head_state(dir), dir: dir.into() })
    }

    pub(crate) fn problem(&self) -> Option<&'static str> {
        match self.items.iter().rev().find(|i| i.act != Act::Drop) {
            None => Some("All commits are dropped (use Reset to move the branch instead)."),
            Some(i) if matches!(i.act, Act::Squash | Act::Fixup) => {
                Some("The oldest commit can't be squashed or fixed up — there is nothing below it to meld into.")
            }
            _ => None,
        }
    }

    /// The todo (oldest first) and the op that runs it.
    pub fn op(&self, git_dir: &Path) -> Res<Op> {
        if !self.head.is_empty() && head_state(&self.dir) != self.head {
            return Err("The branch changed since this list was loaded; reopen the rebase editor.".into());
        }
        // Stacked branches pointing into the range stay on their (rewritten) commits.
        let stacked = match self.cherry || !super::stack::follows() {
            true => vec![],
            false => stacked_refs(&self.dir, &self.items, self.head.rsplit(' ').next().unwrap_or_default()),
        };
        let mut todo = String::new();
        for (n, it) in self.items.iter().enumerate().rev() {
            let refs = stacked.iter().filter(|(_, oid)| *oid == it.id).map(|(r, _)| format!("update-ref {r}\n")).collect::<String>();
            let verb = match it.act {
                Act::Pick | Act::Reword => "pick",
                Act::Edit => "edit",
                Act::Squash => "squash",
                Act::Fixup => "fixup",
                Act::Drop => "drop",
            };
            if it.act != Act::Reword {
                todo += &format!("{verb} {}\n{refs}", it.id);
                continue;
            }
            let msg = git_dir.join(format!("glasir-msg-{n}"));
            let pre = git_dir.join(format!("glasir-pre-{n}"));
            std::fs::write(&msg, format!("{}\n", it.message.trim())).map_err(|e| e.to_string())?;
            // Amend only if the pick made a commit: one that became empty and was
            // skipped would leave the amend to reword the commit before it.
            let (msg, pre) = (sh_quote(&msg), sh_quote(&pre));
            todo += &format!("exec git rev-parse HEAD > {pre}\n{verb} {}\n", it.id);
            todo += &format!("exec if test \"$(git rev-parse HEAD)\" != \"$(cat {pre})\"; then git commit --amend --allow-empty -F {msg}; fi\n{refs}");
        }
        let path = git_dir.join("glasir-rebase-todo");
        std::fs::write(&path, todo).map_err(|e| e.to_string())?;
        let editor = format!("sequence.editor=cat {} >", sh_quote(&path));
        let label = match self.cherry {
            true => format!("Cherry-pick {} commits", self.items.iter().filter(|i| i.act != Act::Drop).count()),
            false => format!("Rebase onto {}", self.label),
        };
        // Cherry-picking onto HEAD carries uncommitted work along, like a cherry-pick would.
        let mut args = vec!["-c", &editor, "-c", "core.editor=true", "rebase", "-i"];
        if self.cherry {
            args.push("--autostash");
        }
        args.push(&self.base);
        let mut op = Op::new(label, &args).undo(Undo::Keep);
        // Checked again when it runs: a confirm dialog may wait while HEAD moves.
        if !self.head.is_empty() {
            op.cmds.insert(0, vec![HEAD_IS.into(), self.head.clone()]);
        }
        if !stacked.is_empty() {
            op.cmds.insert(0, [REFS_AT.to_owned()].into_iter().chain(stacked.into_iter().flat_map(|(r, o)| [r, o])).collect());
        }
        Ok(op)
    }
}

/// Local branches (full name, commit) pointing at one of `items` below HEAD's commit `head`
/// (one at `head` is a copy, like a backup): not a trunk or protected branch, not the remote's
/// default, and not checked out anywhere (HEAD's moves anyway; git refuses another worktree's).
fn stacked_refs(dir: &Path, items: &[Item], head: &str) -> Vec<(String, String)> {
    let fmt = "--format=%(refname)%09%(objectname)%09%(symref:lstrip=3)%09%(worktreepath)";
    let out = git::run(dir, &["for-each-ref", fmt, "refs/heads", "refs/remotes/origin/HEAD"]).unwrap_or_default();
    let rows: Vec<Vec<&str>> = out.lines().map(|l| l.splitn(4, '\t').collect()).collect();
    let default = rows.iter().find(|f| f[0] == "refs/remotes/origin/HEAD").and_then(|f| f.get(2).copied());
    rows.iter()
        .filter_map(|f| match f[..] {
            [r, oid, _, ""] if oid != head && items.iter().any(|i| i.id == oid) => {
                let name = r.strip_prefix("refs/heads/")?;
                (!super::stack::protected(name) && Some(name) != default).then(|| (r.to_owned(), oid.to_owned()))
            }
            _ => None,
        })
        .collect()
}

/// `git log` records as todo items (pick); merges are refused.
fn items(dir: &Path, revs: &[&str]) -> Res<Vec<Item>> {
    let mut args = vec!["log", "-z", "--format=%H%x1f%P%x1f%an%x1f%B"];
    args.extend(revs);
    args.push("--");
    let out = git::run(dir, &args)?;
    let mut items = vec![];
    for rec in out.split('\0').filter(|r| !r.trim().is_empty()) {
        let f: Vec<&str> = rec.trim_start_matches('\n').splitn(4, '\x1f').collect();
        if f.len() < 4 {
            continue;
        }
        if f[1].split_whitespace().count() > 1 {
            return Err("The range contains merge commits; interactive rebase over merges isn't supported.".into());
        }
        items.push(Item { id: f[0].into(), author: f[2].into(), message: f[3].trim_end().into(), act: Act::Pick });
    }
    Ok(items)
}

/// One-step history edit of a single commit on the current branch
/// (reword / squash into parent / drop), reusing the rebase engine.
pub fn quick(dir: &Path, git_dir: &Path, commit: &str, act: Act, message: Option<String>) -> Res<Op> {
    // Squash/fixup meld into the parent, so the parent must be in the todo too.
    let back = if matches!(act, Act::Squash | Act::Fixup) { "~2" } else { "^" };
    on_branch(dir, commit)?;
    let base = git::run(dir, &["rev-parse", "-q", "--verify", &format!("{commit}{back}")])
        .map_err(|_| "Commits next to the root can't be rewritten this way; use the interactive rebase editor.".to_string())?;
    let short = &commit[..commit.len().min(7)];
    let mut ed = Editor::load(dir, base.trim(), short)?;
    let it = ed.items.iter_mut().find(|i| i.id == commit).ok_or("Commit is not on the current branch.")?;
    it.act = act;
    if let Some(m) = message {
        it.message = m;
    }
    if let Some(p) = ed.problem() {
        return Err(p.into());
    }
    let mut op = ed.op(git_dir)?;
    op.label = match act {
        Act::Reword => format!("Reword {short}"),
        Act::Squash => format!("Squash {short}"),
        Act::Drop => format!("Drop {short}"),
        _ => op.label,
    };
    Ok(op)
}

/// Replace the message of `commit` (HEAD: amend, message only; older: a rebase that rewords it).
/// Asks first when the rewrite reaches commits that are already on the upstream.
pub fn reword(dir: &Path, git_dir: &Path, commit: &str, is_head: bool, message: String) -> Res<Op> {
    let mut op = if is_head {
        Op::new("Reword HEAD", &["commit", "--amend", "--only", "-F", "-"]).input(message).undo(Undo::Soft)
    } else {
        let mut op = quick(dir, git_dir, commit, Act::Reword, Some(message))?;
        // A reword changes no tree, so uncommitted work can ride along (git refuses a dirty rebase otherwise).
        if let Some(rebase) = op.cmds.last_mut() {
            rebase.insert(rebase.len() - 1, "--autostash".into());
        }
        op
    };
    // The commits the rewrite replaces (`commit` up to HEAD), and those of them the upstream lacks.
    let mut range = vec!["rev-list".to_owned(), "--count".into(), "HEAD".into()];
    if git::run(dir, &["rev-parse", "-q", "--verify", &format!("{commit}^")]).is_ok() {
        range.push(format!("^{commit}^"));
    }
    let count = |extra: &[&str]| {
        let args: Vec<&str> = range.iter().map(String::as_str).chain(extra.iter().copied()).collect();
        git::run(dir, &args).ok().and_then(|n| n.trim().parse::<usize>().ok())
    };
    if let (Some(total), Some(unpushed), Ok(up)) = (count(&[]), count(&["^@{u}"]), git::run(dir, &["rev-parse", "--abbrev-ref", "@{u}"]))
        && total > unpushed
    {
        let n = total - unpushed;
        op = op.confirm(format!(
            "This rewrites {n} commit{} already pushed to {}. They get new hashes, so the branch will need a force push, and anyone who has them will have to reconcile.",
            if n == 1 { "" } else { "s" },
            up.trim()
        ));
    }
    Ok(op)
}

/// Squash or drop several commits of the current branch in one rebase.
/// Squash needs a contiguous run; everything melds into the oldest selected.
pub fn multi(dir: &Path, git_dir: &Path, ids: &[String], act: Act) -> Res<Op> {
    // Base = parent of the oldest selected commit (the one furthest from HEAD).
    // ids are newest first by graph row (--date-order lists parents after children).
    let oldest = ids.last().ok_or("Nothing selected.")?;
    on_branch(dir, oldest)?;
    let parent = git::run(dir, &["rev-parse", "-q", "--verify", &format!("{oldest}^")])
        .map_err(|_| "The root commit can't be rewritten this way.".to_string())?;
    let mut ed = Editor::load(dir, parent.trim(), &oldest[..oldest.len().min(7)])?;
    let e = &mut ed;
    let pos: Vec<usize> = ids
        .iter()
        .map(|id| e.items.iter().position(|i| &i.id == id).ok_or_else(|| format!("{} is not on the current branch.", &id[..id.len().min(7)])))
        .collect::<Res<_>>()?;
    if act == Act::Squash {
        let (lo, hi) = (*pos.iter().min().unwrap(), *pos.iter().max().unwrap());
        if hi - lo + 1 != pos.len() {
            return Err("Select consecutive commits to squash them.".into());
        }
        for i in lo..hi {
            e.items[i].act = Act::Squash; // newest..second-oldest meld into the oldest (at hi)
        }
    } else {
        for &i in &pos {
            e.items[i].act = act;
        }
    }
    let mut op = e.op(git_dir)?;
    op.label = format!("{} {} commits", if act == Act::Squash { "Squash" } else { "Drop" }, ids.len());
    Ok(op)
}

/// The todo for dragging `src` onto `dst` in the graph: both unpushed, on the
/// current branch, with no merges from the older one up to HEAD.
pub fn drag_editor(dir: &Path, src: &str, dst: &str) -> Res<Editor> {
    on_branch(dir, src)?;
    on_branch(dir, dst)?;
    let oldest = if git::run(dir, &["merge-base", "--is-ancestor", src, dst]).is_ok() { src } else { dst };
    // Its descendants up to HEAD are then unpushed too.
    if git::run(dir, &["rev-list", "-n1", oldest, "--not", "--remotes"])?.trim().is_empty() {
        return Err("Pushed commits can't be reordered or squashed by dragging (that would rewrite shared history).".into());
    }
    let base = git::run(dir, &["rev-parse", "-q", "--verify", &format!("{oldest}^")])
        .map_err(|_| "The root commit can't be moved by dragging; use the interactive rebase editor.".to_string())?;
    Editor::load(dir, base.trim(), &oldest[..oldest.len().min(7)])
}

/// Drag `src` onto `dst`: `Pick` moves it to `dst`'s place (like a list drag),
/// `Squash`/`Fixup` meld it into `dst`. One rebase, one undo step.
pub fn drag(dir: &Path, git_dir: &Path, src: &str, dst: &str, act: Act) -> Res<Op> {
    let mut ed = drag_editor(dir, src, dst)?;
    let pos = |ed: &Editor, id: &str| ed.items.iter().position(|i| i.id == id).ok_or("Commit is not on the current branch.");
    let (si, di) = (pos(&ed, src)?, pos(&ed, dst)?);
    let mut it = ed.items.remove(si);
    // Items are newest first: a meld goes right above (newer than) its target.
    let at = if act == Act::Pick { di } else { pos(&ed, dst)? };
    it.act = act;
    ed.items.insert(at, it);
    if let Some(p) = ed.problem() {
        return Err(p.into());
    }
    let mut op = ed.op(git_dir)?;
    let (s, d) = (&src[..src.len().min(7)], &dst[..dst.len().min(7)]);
    op.label = match act {
        Act::Squash => format!("Squash {s} into {d}"),
        Act::Fixup => format!("Fixup {s} into {d}"),
        _ => format!("Move {s}"),
    };
    Ok(op)
}

/// Commit the staged changes as a fixup of `commit` and fold it in (autosquash).
/// `fixup! <sha>`, not `--fixup` (`fixup! <subject>`): autosquash would pick the oldest commit with that subject.
pub fn amend_into(dir: &Path, commit: &str, is_head: bool) -> Res<Op> {
    let short = &commit[..commit.len().min(7)];
    if is_head {
        return Ok(Op::new(format!("Amend {short}"), &["commit", "--amend", "--no-edit"]).undo(Undo::Soft));
    }
    // Same safety checks as the editor: on this branch, with no merges after it.
    on_branch(dir, commit)?;
    pending_autosquash(Editor::load(dir, &format!("{commit}^"), short)?.items.iter().map(|i| i.message.as_str()))?;
    // Soft: undo puts the folded-in changes back in the index (`--keep` would drop them).
    Ok(Op::new(format!("Amend staged into {short}"), &["commit", "-m", &format!("fixup! {commit}")])
        .then(&["-c", "sequence.editor=true", "-c", "core.editor=true", "rebase", "-i", "--autosquash", "--autostash", &format!("{commit}^")])
        .undo(Undo::Soft))
}

/// Uncommit `commit` so it can be split: HEAD is just reset; an older commit
/// gets a rebase that stops there (`edit`), then the reset. `rebase --continue`
/// replays the rest once the parts are committed.
pub fn split(dir: &Path, git_dir: &Path, commit: &str, is_head: bool) -> Res<Op> {
    let short = &commit[..commit.len().min(7)];
    if is_head {
        return Ok(Op::new(format!("Split {short}"), &["reset", "-q", "HEAD^"]).undo(Undo::Mixed));
    }
    on_branch(dir, commit)?;
    let base = git::run(dir, &["rev-parse", "-q", "--verify", &format!("{commit}^")]).map_err(|_| "The root commit can't be split.".to_string())?;
    let mut ed = Editor::load(dir, base.trim(), short)?;
    ed.items.iter_mut().find(|i| i.id == commit).ok_or("Commit is not on the current branch.")?.act = Act::Edit;
    let mut op = ed.op(git_dir)?.then(&["reset", "-q", "HEAD^"]);
    op.label = format!("Split {short}");
    Ok(op)
}

/// `rebase --autosquash` would also fold the user's own fixup!/squash!/amend!
/// commits among `messages` (the commits it replays), beyond what the op says.
pub(super) fn pending_autosquash<'a>(mut messages: impl Iterator<Item = &'a str>) -> Res<()> {
    match messages.find(|m| ["fixup! ", "squash! ", "amend! "].iter().any(|p| m.starts_with(p))) {
        Some(m) => Err(format!("\"{}\" is a pending autosquash commit; fold or reword it in the rebase editor first.", m.lines().next().unwrap_or(m))),
        None => Ok(()),
    }
}

fn on_branch(dir: &Path, commit: &str) -> Res<()> {
    git::run(dir, &["merge-base", "--is-ancestor", commit, "HEAD"]).map(drop).map_err(|_| "Commit is not on the current branch.".into())
}

/// `exec` pseudo-command: `HEAD_IS state` fails unless [`head_state`] is still `state`.
pub(super) const HEAD_IS: &str = "!head-is";

/// `exec` pseudo-command: `REFS_AT (ref oid)..` fails unless every ref still points at its oid.
pub(super) const REFS_AT: &str = "!refs-at";

pub(super) fn refs_at(dir: &Path, pairs: &[&str]) -> Res<String> {
    for p in pairs.chunks(2) {
        let now = git::run(dir, &["rev-parse", "-q", "--verify", p[0]]).unwrap_or_default();
        if p.get(1).is_none_or(|o| now.trim() != *o) {
            let name = p[0].strip_prefix("refs/heads/").unwrap_or(p[0]);
            return Err(format!("{name} moved since this was prepared, so nothing was changed. Check it and try again."));
        }
    }
    Ok(String::new())
}

/// `exec` pseudo-command: `rebase --abort` if a rebase is in progress, else nothing.
pub(super) const ABORT_ANY: &str = "!abort-rebase";

/// `refs/heads/x <sha>` (or just the sha when detached).
pub(super) fn head_state(dir: &Path) -> String {
    let sym = git::run(dir, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default();
    format!("{} {}", sym.trim(), git::run(dir, &["rev-parse", "HEAD"]).unwrap_or_default().trim())
}

/// Single-quote for git's POSIX shell (also used by Git for Windows).
fn sh_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    #[cfg(windows)]
    let s = std::borrow::Cow::<str>::Owned(s.replace('\\', "/"));
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(acts: &[Act]) -> Editor {
        let items = acts
            .iter()
            .enumerate()
            .map(|(i, &act)| Item { id: format!("c{i}"), message: format!("msg {i}\n\nbody"), author: "a".into(), act })
            .collect();
        Editor { base: "base".into(), label: "base".into(), items, sel: 0, cherry: false, head: String::new(), dir: Default::default() }
    }

    #[test]
    fn todo_is_oldest_first_with_reword_exec() {
        let dir = std::env::temp_dir().join(format!("glasir-rebase-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = ed(&[Act::Fixup, Act::Reword, Act::Drop, Act::Pick]); // newest .. oldest
        e.items[1].message = "new message".into();
        let op = e.op(&dir).unwrap();
        let todo = std::fs::read_to_string(dir.join("glasir-rebase-todo")).unwrap();
        let lines: Vec<&str> = todo.lines().collect();
        assert_eq!(lines[0], "pick c3");
        assert_eq!(lines[1], "drop c2");
        assert!(lines[2].starts_with("exec git rev-parse HEAD > '") && lines[2].ends_with("glasir-pre-1'"));
        assert_eq!(lines[3], "pick c1");
        assert!(lines[4].starts_with("exec if test ") && lines[4].contains("git commit --amend --allow-empty -F '") && lines[4].ends_with("glasir-msg-1'; fi"));
        assert_eq!(lines[5], "fixup c0");
        assert_eq!(std::fs::read_to_string(dir.join("glasir-msg-1")).unwrap(), "new message\n");
        let rebase = op.cmds.last().unwrap();
        assert!(rebase.iter().any(|a| a.starts_with("sequence.editor=cat '")));
        assert_eq!(rebase.last().unwrap(), "base");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rebase_end_to_end() {
        let dir = std::env::temp_dir().join(format!("glasir-rebase-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| git::run(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "T"]);
        g(&["config", "commit.gpgsign", "false"]);
        for (n, f) in [("c1", "a"), ("c2", "b"), ("c3", "c"), ("c4", "d")] {
            std::fs::write(dir.join(f), n).unwrap();
            g(&["add", f]);
            g(&["commit", "-q", "-m", n]);
        }
        let base = g(&["rev-parse", "HEAD~3"]).trim().to_owned();
        let mut e = Editor::load(&dir, &base, "c1").unwrap();
        assert_eq!(e.items.len(), 3);
        e.items[0].act = Act::Fixup; // c4 melds into c3
        e.items[1].act = Act::Reword;
        e.items[1].message = "c3 reworded".into();
        e.items[2].act = Act::Drop;
        run_op(&dir, &e.op(&git::git_dir(&dir).unwrap()).unwrap());
        let log = g(&["log", "--format=%s"]);
        assert_eq!(log.lines().collect::<Vec<_>>(), ["c3 reworded", "c1"]);
        assert!(dir.join("d").exists() && dir.join("c").exists() && !dir.join("b").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn repo(name: &str) -> (std::path::PathBuf, impl Fn(&[&str]) -> String) {
        let dir = std::env::temp_dir().join(format!("glasir-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.clone();
        let g = move |args: &[&str]| git::run(&d, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "T"]);
        g(&["config", "commit.gpgsign", "false"]);
        for (n, f) in [("c1", "a"), ("c2", "b"), ("c3", "c")] {
            std::fs::write(dir.join(f), format!("{n}\n")).unwrap();
            g(&["add", f]);
            g(&["commit", "-q", "-m", n]);
        }
        (dir, g)
    }

    fn run_op(dir: &Path, op: &Op) {
        crate::repo::op::exec(dir, op).unwrap();
    }

    #[test]
    fn amend_staged_into_older_commit() {
        let (dir, g) = repo("amend");
        let c2 = g(&["rev-parse", "HEAD~1"]).trim().to_owned();
        std::fs::write(dir.join("b"), "c2 fixed\n").unwrap();
        std::fs::write(dir.join("c"), "unstaged edit\n").unwrap(); // must survive (autostash)
        g(&["add", "b"]);
        run_op(&dir, &amend_into(&dir, &c2, false).unwrap());
        assert_eq!(g(&["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["c3", "c2", "c1"]);
        assert_eq!(g(&["show", "HEAD~1:b"]), "c2 fixed\n");
        assert_eq!(std::fs::read_to_string(dir.join("c")).unwrap(), "unstaged edit\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reworded pick that became empty and was skipped rewords nothing else.
    #[test]
    fn skipped_reword_leaves_head_alone() {
        let (dir, g) = repo("reword-skip");
        g(&["checkout", "-q", "-b", "side", "HEAD~2"]);
        std::fs::write(dir.join("x"), "x\n").unwrap();
        g(&["add", "x"]);
        g(&["commit", "-q", "-m", "side x"]);
        let side = g(&["rev-parse", "HEAD"]).trim().to_owned();
        g(&["checkout", "-q", "main"]);
        std::fs::write(dir.join("x"), "x\n").unwrap();
        g(&["add", "x"]);
        g(&["commit", "-q", "-m", "my important message"]);
        let mut e = Editor::cherry_pick(&dir, &[side]).unwrap();
        e.items[0].act = Act::Reword;
        e.items[0].message = "picked, reworded".into();
        let op = e.op(&git::git_dir(&dir).unwrap()).unwrap();
        assert!(crate::repo::op::exec(&dir, &op).is_err(), "the now-empty pick stops");
        g(&["-c", "core.editor=true", "rebase", "--skip"]);
        assert_eq!(g(&["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["my important message", "c3", "c2", "c1"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The user's own pending squash!/fixup!/amend! commits aren't folded in on the side.
    #[test]
    fn amend_into_refuses_pending_autosquash() {
        let (dir, g) = repo("amend-pending");
        let c2 = g(&["rev-parse", "HEAD~1"]).trim().to_owned();
        std::fs::write(dir.join("c"), "wip\n").unwrap();
        g(&["commit", "-q", "-am", "squash! c3"]);
        let tip = g(&["rev-parse", "HEAD"]);
        std::fs::write(dir.join("b"), "c2 fixed\n").unwrap();
        g(&["add", "b"]);
        assert!(amend_into(&dir, &c2, false).is_err());
        assert_eq!(g(&["rev-parse", "HEAD"]), tip);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Through the app: ⌘Z keeps the amended-in changes staged; another branch's commit is refused.
    #[test]
    fn amend_into_undo_and_other_branch() {
        use super::super::testutil::settle;
        use super::super::{Action, RepoTab, View};
        let (dir, g) = repo("amend-undo");
        let (c2, tip) = (g(&["rev-parse", "HEAD~1"]).trim().to_owned(), g(&["rev-parse", "HEAD"]));
        std::fs::write(dir.join("b"), "c2 fixed\n").unwrap();
        g(&["add", "b"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::AmendInto(c2));
        settle(&ctx, &mut t);
        assert_eq!(g(&["show", "HEAD~1:b"]), "c2 fixed\n");
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(g(&["rev-parse", "HEAD"]), tip);
        assert_eq!(g(&["diff", "--cached", "--name-only"]), "b\n");
        assert_eq!(std::fs::read_to_string(dir.join("b")).unwrap(), "c2 fixed\n");
        g(&["reset", "-q", "--hard"]);
        g(&["checkout", "-q", "-b", "side", "HEAD~2"]);
        std::fs::write(dir.join("s"), "s").unwrap();
        g(&["add", "s"]);
        g(&["commit", "-q", "-m", "s"]);
        let s = g(&["rev-parse", "HEAD"]).trim().to_owned();
        g(&["checkout", "-q", "main"]);
        assert_eq!(amend_into(&dir, &s, false).unwrap_err(), "Commit is not on the current branch.");
        // The editor still opens onto a diverged branch: its commits after the merge base.
        assert_eq!(Editor::load(&dir, "side", "side").unwrap().items.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ⌘Z of an interactive cherry-pick keeps an uncommitted edit of a picked file as it is
    /// (never merged or refused), and says so.
    #[test]
    fn cherry_pick_undo_keeps_overlapping_edit() {
        use super::super::testutil::settle;
        use super::super::{Action, RepoTab, View};
        let (dir, g) = repo("cherry-undo");
        let lines = |edit: usize, to: &str| (1..=20).map(|i| if i == edit { to.to_owned() } else { i.to_string() }).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.join("f"), lines(0, "")).unwrap();
        g(&["add", "f"]);
        g(&["commit", "-q", "-m", "f"]);
        g(&["checkout", "-q", "-b", "side"]);
        std::fs::write(dir.join("f"), lines(2, "two")).unwrap();
        g(&["commit", "-q", "-am", "side"]);
        let side = g(&["rev-parse", "HEAD"]).trim().to_owned();
        g(&["checkout", "-q", "main"]);
        let tip = g(&["rev-parse", "HEAD"]);
        std::fs::write(dir.join("f"), lines(19, "nineteen")).unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let op = Editor::cherry_pick(&dir, &[side]).unwrap().op(&git::git_dir(&dir).unwrap()).unwrap();
        t.apply(&ctx, Action::Git(op));
        settle(&ctx, &mut t);
        assert_ne!(g(&["rev-parse", "HEAD"]), tip);
        t.apply(&ctx, Action::Undo);
        let msgs = settle(&ctx, &mut t);
        assert_eq!(g(&["rev-parse", "HEAD"]), tip);
        let both = lines(19, "nineteen").replace("\n2\n", "\ntwo\n");
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), both);
        assert!(msgs.iter().any(|m| m.text.contains("Kept your changes to f")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quick_reword_and_drop() {
        let (dir, g) = repo("quick");
        let gd = git::git_dir(&dir).unwrap();
        let c2 = g(&["rev-parse", "HEAD~1"]).trim().to_owned();
        run_op(&dir, &quick(&dir, &gd, &c2, Act::Reword, Some("c2 better".into())).unwrap());
        assert_eq!(g(&["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["c3", "c2 better", "c1"]);
        let c2 = g(&["rev-parse", "HEAD~1"]).trim().to_owned();
        run_op(&dir, &quick(&dir, &gd, &c2, Act::Drop, None).unwrap());
        assert_eq!(g(&["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["c3", "c1"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multi_squash_drop_and_errors() {
        let (dir, g) = repo("multi");
        let gd = git::git_dir(&dir).unwrap();
        let ids: Vec<String> = g(&["rev-list", "HEAD"]).lines().map(String::from).collect(); // c3, c2, c1
        // Squash c3+c2 (consecutive) into c2.
        run_op(&dir, &multi(&dir, &gd, &ids[..2], Act::Squash).unwrap());
        assert_eq!(g(&["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["c2", "c1"]);
        assert!(g(&["log", "-1", "--format=%B"]).contains("c3")); // messages combined
        assert!(dir.join("c").exists());
        // Non-consecutive squash is refused.
        let (dir2, g2) = repo("multi2");
        let ids2: Vec<String> = g2(&["rev-list", "HEAD"]).lines().map(String::from).collect();
        assert!(multi(&dir2, &git::git_dir(&dir2).unwrap(), &[ids2[0].clone(), ids2[2].clone()], Act::Squash).is_err());
        // Drop two of three.
        run_op(&dir2, &multi(&dir2, &git::git_dir(&dir2).unwrap(), &ids2[..2], Act::Drop).unwrap());
        assert_eq!(g2(&["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["c1"]);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// Foreign commits picked onto HEAD reordered, reworded and dropped in one rebase.
    #[test]
    fn interactive_cherry_pick() {
        let (dir, g) = repo("cherry");
        g(&["checkout", "-q", "-b", "side", "HEAD~2"]);
        for f in ["s1", "s2", "s3"] {
            std::fs::write(dir.join(f), f).unwrap();
            g(&["add", f]);
            g(&["commit", "-q", "-m", f]);
        }
        let side: Vec<String> = g(&["rev-list", "-3", "HEAD"]).lines().map(String::from).collect(); // s3, s2, s1
        g(&["checkout", "-q", "main"]);
        let mut e = Editor::cherry_pick(&dir, &side).unwrap();
        assert_eq!((e.items.len(), e.items[0].message.as_str(), e.label.as_str()), (3, "s3", "main"));
        e.items.swap(0, 2); // newest first: s1, s2, s3 -> s1 is applied last
        e.items[0].act = Act::Reword;
        e.items[0].message = "s1 better".into();
        e.items[1].act = Act::Drop;
        let op = e.op(&git::git_dir(&dir).unwrap()).unwrap();
        assert_eq!(op.label, "Cherry-pick 2 commits");
        std::fs::write(dir.join("a"), "local edit\n").unwrap(); // carried along (autostash)
        g(&["add", "a"]);
        run_op(&dir, &op);
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "local edit\n");
        assert_eq!(g(&["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["s1 better", "s3", "c3", "c2", "c1"]);
        assert!(!dir.join("s2").exists());
        assert_eq!(g(&["rev-parse", "side"]).trim(), side[0]); // the source branch is untouched
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Split an older commit (rebase stops there, changes uncommitted) and HEAD.
    #[test]
    fn split_commit() {
        let (dir, g) = repo("split");
        for f in ["x", "y"] {
            std::fs::write(dir.join(f), f).unwrap();
        }
        g(&["add", "x", "y"]);
        g(&["commit", "-q", "-m", "x and y"]);
        std::fs::write(dir.join("z"), "z").unwrap();
        g(&["add", "z"]);
        g(&["commit", "-q", "-m", "z"]);
        let gd = git::git_dir(&dir).unwrap();
        let xy = g(&["rev-parse", "HEAD~1"]).trim().to_owned();
        run_op(&dir, &split(&dir, &gd, &xy, false).unwrap());
        assert_eq!(git::state(&gd), git::State::Rebasing);
        assert_eq!(g(&["log", "-1", "--format=%s"]).trim(), "c3");
        assert!(g(&["status", "--porcelain"]).contains("?? x"));
        for f in ["x", "y"] {
            g(&["add", f]);
            g(&["commit", "-q", "-m", f]);
        }
        g(&["-c", "core.editor=true", "rebase", "--continue"]);
        assert_eq!(g(&["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["z", "y", "x", "c3", "c2", "c1"]);
        // HEAD: just a reset, no rebase.
        let head = g(&["rev-parse", "HEAD"]).trim().to_owned();
        run_op(&dir, &split(&dir, &gd, &head, true).unwrap());
        assert_eq!(git::state(&gd), git::State::Clean);
        assert!(g(&["status", "--porcelain"]).contains("?? z"));
        assert!(split(&dir, &gd, &g(&["rev-list", "--max-parents=0", "HEAD"]).trim().to_owned(), false).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Dragging commits in the graph: reorder, ⌥-squash and cherry-pick, one ⌘Z each;
    /// pushed commits and other branches are refused.
    #[test]
    fn drag_commits_in_the_graph() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, RepoTab, View};
        let dir = scratch("drag");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for f in ["a", "b", "c", "d"] {
            commit(&dir, f, f);
        }
        sh(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD~2"]); // a and b are pushed
        sh(&dir, &["branch", "side", "HEAD~3"]);
        let id = |r: &str| sh(&dir, &["rev-parse", r]).trim().to_owned();
        let log = || sh(&dir, &["log", "--format=%s"]);
        let tip = id("HEAD");
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let menu = |t: &mut RepoTab, src: &str, dst: Option<&str>, branch: Option<&str>| {
            t.apply(&ctx, Action::DropCommit { src: id(src), dst: dst.map(id), branch: branch.map(String::from), squash: false });
            settle(&ctx, t);
            t.commit_drop.take().expect("a drop menu").1
        };

        // Reorder: d dropped onto c goes below it.
        let items = menu(&mut t, "HEAD", Some("HEAD~1"), None);
        assert_eq!(items.iter().map(|i| i.0.as_str()).collect::<Vec<_>>()[..2], ["Move below \"c\"", "Squash into \"c\""]);
        t.apply(&ctx, items[0].1.clone().unwrap());
        settle(&ctx, &mut t);
        assert_eq!(log(), "c\nd\nb\na\n");
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(id("HEAD"), tip);

        // ⌥-drop c onto d: squashed into it right away, both messages kept.
        t.apply(&ctx, Action::DropCommit { src: id("HEAD~1"), dst: Some(id("HEAD")), branch: None, squash: true });
        assert!(t.commit_drop.is_none());
        settle(&ctx, &mut t);
        assert_eq!(log(), "d\nb\na\n");
        assert_eq!(sh(&dir, &["log", "-1", "--format=%B"]).trim(), "d\n\nc");
        assert!(dir.join("c").exists() && dir.join("d").exists());
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(id("HEAD"), tip);

        // Pushed commits are refused, also as the far end of a drag.
        for (src, dst) in [("HEAD~2", "HEAD"), ("HEAD", "HEAD~2")] {
            let items = menu(&mut t, src, Some(dst), None);
            assert!(items.len() == 1 && items[0].1.is_none() && items[0].0.starts_with("Pushed commits"), "{:?}", items[0].0);
        }

        // Cherry-pick: only onto HEAD's branch, and not of a commit already on it.
        let items = menu(&mut t, "HEAD", None, Some("refs/heads/side"));
        assert!(items[0].1.is_none() && items[0].0.starts_with("Check out side"));
        sh(&dir, &["checkout", "-q", "side"]);
        t.apply(&ctx, Action::Reload);
        settle(&ctx, &mut t);
        let items = menu(&mut t, "main", None, Some("refs/heads/side"));
        assert_eq!(items[0].0, "Cherry-pick onto side");
        t.apply(&ctx, items[0].1.clone().unwrap());
        settle(&ctx, &mut t);
        assert_eq!(log(), "d\na\n");
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(log(), "a\n");
        assert_eq!(id("main"), tip);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_squash_into_nothing() {
        assert!(ed(&[Act::Pick, Act::Squash]).problem().is_some());
        assert!(ed(&[Act::Squash, Act::Pick]).problem().is_none());
        assert!(ed(&[Act::Pick, Act::Fixup, Act::Drop]).problem().is_some()); // dropped oldest doesn't count
        assert!(ed(&[Act::Drop, Act::Drop]).problem().is_some());
    }

    /// A drop waiting in its confirm dialog while a commit lands elsewhere: refuses
    /// instead of running the stale todo (which would drop the new commit too).
    #[test]
    fn a_confirmed_rewrite_refuses_once_head_moved() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, MultiOp, RepoTab, View};
        let dir = scratch("stale-drop");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for f in ["a", "b", "c", "d"] {
            commit(&dir, f, f);
        }
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let ids = ["HEAD", "HEAD~1"].map(|r| sh(&dir, &["rev-parse", r]).trim().to_owned()).to_vec();
        t.apply(&ctx, Action::MultiOp(MultiOp::Drop, ids));
        let op = t.confirm.take().expect("drop asks first");
        commit(&dir, "e", "new work");
        t.apply(&ctx, Action::Git(Op { confirm: None, ..op }));
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["log", "--format=%s"]), "new work\nd\nc\nb\na\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
