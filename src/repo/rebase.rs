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
    orig: Vec<String>,
    dir: std::path::PathBuf,
}

impl Editor {
    /// `base` need not be an ancestor of HEAD: `base..HEAD` is what `rebase -i <base>` replays onto it.
    pub fn load(dir: &Path, base: &str, label: &str) -> Res<Editor> {
        let items = items(dir, &[&format!("{base}..HEAD")])?;
        if items.is_empty() {
            return Err(format!("No commits after {label} on this branch."));
        }
        Ok(Editor { base: base.into(), label: label.into(), orig: items.iter().map(|i| i.id.clone()).collect(), items, sel: 0, cherry: false, head: head_state(dir), dir: dir.into() })
    }

    /// Cherry-pick `ids` (newest first) onto HEAD with pick/reword/squash/drop and
    /// reordering: an interactive rebase of HEAD onto itself whose todo picks them.
    pub fn cherry_pick(dir: &Path, ids: &[String]) -> Res<Editor> {
        let head = git::run(dir, &["rev-parse", "HEAD"])?.trim().to_owned();
        let label = git::run(dir, &["symbolic-ref", "-q", "--short", "HEAD"]).map_or_else(|_| "HEAD".into(), |b| b.trim().to_owned());
        let mut args = vec!["--no-walk=unsorted"];
        args.extend(ids.iter().map(String::as_str));
        let items = items(dir, &args)?;
        Ok(Editor { base: head, label, orig: items.iter().map(|i| i.id.clone()).collect(), items, sel: 0, cherry: true, head: head_state(dir), dir: dir.into() })
    }

    pub fn edited(&self) -> bool {
        self.items.iter().any(|it| it.act != Act::Pick) || !self.items.iter().map(|it| &it.id).eq(&self.orig)
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
        // A stopped rebase's pending todo reads the message files written below.
        if !matches!(git::state(git_dir), git::State::Clean | git::State::Bisecting) {
            return Err("Finish or abort the operation in progress first.".into());
        }
        if !self.head.is_empty() && head_state(&self.dir) != self.head {
            return Err("The branch changed since this list was loaded; reopen the rebase editor.".into());
        }
        // Stacked branches pointing into the range stay on their (rewritten) commits.
        let stacked = match self.cherry || !super::stack::follows() {
            true => vec![],
            false => stacked_refs(&self.dir, |oid| self.items.iter().any(|i| i.id == oid), self.head.rsplit(' ').next().unwrap_or_default()),
        };
        let (mut todo, mut refs) = (String::new(), String::new());
        for (n, it) in self.items.iter().enumerate().rev() {
            if matches!(it.act, Act::Pick | Act::Reword | Act::Edit) {
                todo += &std::mem::take(&mut refs);
            }
            refs.extend(stacked.iter().filter(|(_, oid)| *oid == it.id).map(|(r, _)| format!("update-ref {r}\n")));
            let verb = match it.act {
                Act::Pick | Act::Reword => "pick",
                Act::Edit => "edit",
                Act::Squash => "squash",
                Act::Fixup => "fixup",
                Act::Drop => "drop",
            };
            // Settings → "(cherry picked from commit …)": `pick` has no -x, so the trailer is
            // added by amending the fresh pick (or written into a reword's message).
            let x = self.cherry && super::merge_opts::pick_x();
            let note = format!("(cherry picked from commit {})", it.id);
            if it.act != Act::Reword && !(x && it.act == Act::Pick) {
                todo += &format!("{verb} {}\n", it.id);
                continue;
            }
            let msg = git_dir.join(format!("glasir-msg-{n}"));
            let pre = git_dir.join(format!("glasir-pre-{n}"));
            let reword = it.act == Act::Reword;
            // A pick's message is read when it runs; a reword's is fixed now.
            let text = if x { format!("{}\n\n{note}\n", it.message.trim()) } else { format!("{}\n", it.message.trim()) };
            if reword {
                std::fs::write(&msg, text).map_err(|e| e.to_string())?;
            }
            // Amend only if the pick made a commit: one that became empty and was
            // skipped would leave the amend to reword the commit before it.
            let (msg, pre) = (sh_quote(&msg), sh_quote(&pre));
            let amend = match reword {
                true => format!("git commit --amend --allow-empty -F {msg}"),
                false => format!("git log -1 --format=%B > {msg} && printf '\\n%s\\n' '{note}' >> {msg} && git commit --amend --allow-empty -F {msg}"),
            };
            todo += &format!("exec git rev-parse HEAD > {pre}\n{verb} {}\n", it.id);
            todo += &format!("exec if test \"$(git rev-parse HEAD)\" != \"$(cat {pre})\"; then {amend}; fi\n");
        }
        todo += &refs;
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
        } else {
            // A pick already applied upstream is dropped, as plain `rebase -i` does.
            args.push("--empty=drop");
        }
        args.push(&self.base);
        // Its picks put files where ignored ones may sit: what they hold counts as named.
        let revs = std::iter::once(&self.base).chain(self.items.iter().map(|i| &i.id)).cloned().collect();
        let mut op = Op::new(label, &args).undo(Undo::Keep).backup(super::Backup::Tree { revs, untracked: false });
        // Checked again when it runs: a confirm dialog may wait while HEAD moves.
        if !self.head.is_empty() {
            op.cmds.insert(0, vec![HEAD_IS.into(), self.head.clone()]);
        }
        if !stacked.is_empty() {
            op.cmds.insert(0, [REFS_AT.to_owned()].into_iter().chain(stacked.into_iter().flat_map(|(r, o)| [r, o])).collect());
        }
        Ok(op)
    }

    /// What Start runs: [`Self::op`], a rebase asking first when it rewrites pushed commits: those from the
    /// plan's first change up (git keeps the unchanged picks below it; a squash or fixup rewrites the one it
    /// melds into, the next one below that isn't dropped).
    pub fn start(&self, git_dir: &Path) -> Res<super::Action> {
        let op = self.op(git_dir)?;
        let changed = self.items.iter().zip(&self.orig).rposition(|(it, id)| it.id != *id || it.act != Act::Pick);
        let meld = |i: usize| (i + 1..self.items.len()).find(|&k| self.items[k].act != Act::Drop);
        let first = changed.map(|j| (0..=j).filter(|&i| matches!(self.items[i].act, Act::Squash | Act::Fixup)).filter_map(meld).fold(j, usize::max));
        let first = first.and_then(|j| self.orig.get(j)).cloned();
        Ok(super::Action::RunRebase { op, onto: self.base.clone(), branch: None, first })
    }
}

/// Local branches (full name, commit) pointing at a commit the rewrite takes (`rewritten`) below the tip's
/// commit `head` (one at `head` is a copy, like a backup): not a trunk or protected branch, not the remote's
/// default, and not checked out anywhere (HEAD's moves anyway; git refuses another worktree's).
fn stacked_refs(dir: &Path, rewritten: impl Fn(&str) -> bool, head: &str) -> Vec<(String, String)> {
    let fmt = "--format=%(refname)%09%(objectname)%09%(symref:lstrip=3)%09%(worktreepath)";
    let out = git::run(dir, &["for-each-ref", fmt, "refs/heads", "refs/remotes/origin/HEAD"]).unwrap_or_default();
    let rows: Vec<Vec<&str>> = out.lines().map(|l| l.splitn(4, '\t').collect()).collect();
    let default = rows.iter().find(|f| f[0] == "refs/remotes/origin/HEAD").and_then(|f| f.get(2).copied());
    rows.iter()
        .filter_map(|f| match f[..] {
            [r, oid, _, ""] if oid != head && rewritten(oid) => {
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
    let first = if matches!(act, Act::Squash | Act::Fixup) { format!("{commit}^") } else { commit.to_owned() };
    Ok(confirm_pushed(dir, &first, ("HEAD", "@{u}"), op))
}

/// Asks first when rewriting `commit` up to `tip` (HEAD or `refs/heads/<branch>`) reaches
/// commits already on the upstream (`HEAD@{u}` or `<branch>@{u}`), or else on any remote branch
/// (no upstream or a gone one, a fork's branch, one pushed without -u).
fn confirm_pushed(dir: &Path, commit: &str, (tip, upstream): (&str, &str), op: Op) -> Op {
    // The commits the rewrite replaces (`commit` and its descendants up to HEAD) that the upstream
    // has: those up to the merge bases (an extra `^@{u}` would cut the ancestry path short).
    let (ancestry, parents) = (format!("--ancestry-path={commit}"), format!("^{commit}^@"));
    if let Ok(up) = git::run(dir, &["rev-parse", "--abbrev-ref", upstream]) {
        let bases = git::run(dir, &["merge-base", "--all", tip, upstream]).unwrap_or_default();
        let mut args = vec!["rev-list", "--count", &ancestry, &parents];
        args.extend(bases.lines());
        let n = match bases.trim().is_empty() {
            true => 0,
            false => git::run(dir, &args).ok().and_then(|n| n.trim().parse::<usize>().ok()).unwrap_or(0),
        };
        if n > 0 {
            return op.confirm(rewrites_pushed(n, Some(up.trim())));
        }
    }
    let all = git::run(dir, &["rev-list", &ancestry, tip, &parents]).unwrap_or_default();
    let local = git::run(dir, &["rev-list", tip, &parents, "--not", "--remotes"]).unwrap_or_default();
    let local: std::collections::HashSet<&str> = local.lines().collect();
    match all.lines().filter(|c| !local.contains(c)).count() {
        0 => op,
        n => op.confirm(rewrites_pushed(n, None)),
    }
}

pub(super) fn rewrites_pushed(n: usize, up: Option<&str>) -> String {
    let s = if n == 1 { "" } else { "s" };
    match up {
        Some(up) => format!("This rewrites {n} commit{s} already pushed to {up}. They get new hashes, so the branch will need a force push, and anyone who has them will have to reconcile."),
        None => format!("This rewrites {n} commit{s} already on a remote branch. They get new hashes, so merging this branch later brings in duplicates of them."),
    }
}

/// [`confirm_pushed`] for `op`, a rebase of `branch` (None: HEAD's) onto `onto`: from `first` up when
/// the branch is on top of `onto` already (a plain rebase there changes nothing without merges), else
/// every commit since the merge base.
pub fn confirm_rebase(dir: &Path, onto: &str, branch: Option<&str>, first: Option<&str>, op: Op) -> Op {
    let (tip, up) = branch.map_or(("HEAD".to_owned(), "@{u}".to_owned()), |b| (format!("refs/heads/{b}"), format!("{b}@{{u}}")));
    let range = format!("{onto}..{tip}");
    let on_top = git::run(dir, &["merge-base", "--is-ancestor", onto, &tip]).is_ok();
    let linear = || git::run(dir, &["rev-list", "-n1", "--min-parents=2", &range]).is_ok_and(|m| m.trim().is_empty());
    let first = match first {
        Some(c) if on_top => Some(c.to_owned()),
        _ if on_top && linear() => None,
        _ => git::run(dir, &["rev-list", "--topo-order", "--reverse", &range]).ok().and_then(|r| r.lines().next().map(String::from)),
    };
    match first {
        Some(c) => confirm_pushed(dir, &c, (&tip, &up), op),
        None => op,
    }
}

/// Replace the message of `commit` (HEAD: amend, message only; older with no merge from its
/// parent up to HEAD: a rebase that rewords it; else, merges or the root: [`REWRITE`]).
/// Asks first when the rewrite reaches commits that are already pushed.
pub fn reword(dir: &Path, git_dir: &Path, commit: &str, is_head: bool, branch: Option<&str>, message: String) -> Res<Op> {
    let short = &commit[..commit.len().min(7)];
    if let Some(b) = branch {
        let full = format!("refs/heads/{b}");
        let op = Op::new(format!("Reword {short} on {b}"), &[REWRITE, commit, &full]).input(message).undo(Undo::Soft);
        return Ok(confirm_pushed(dir, commit, (&full, &format!("{b}@{{u}}")), op));
    }
    let straight = || git::run(dir, &["rev-list", "-n1", "--min-parents=2", &format!("{commit}^..HEAD")]).is_ok_and(|m| m.trim().is_empty());
    let op = if is_head {
        let sym = git::run(dir, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default();
        Op::new("Reword HEAD", &[HEAD_IS, &format!("{} {commit}", sym.trim())]).then(&["commit", "--amend", "--only", "-F", "-"]).input(message).undo(Undo::Soft)
    } else if straight() {
        let mut op = quick(dir, git_dir, commit, Act::Reword, Some(message))?;
        // A reword changes no tree, so uncommitted work can ride along (git refuses a dirty rebase otherwise).
        if let Some(rebase) = op.cmds.last_mut() {
            rebase.insert(rebase.len() - 1, "--autostash".into());
        }
        return Ok(op); // quick() already asked
    } else {
        on_branch(dir, commit)?;
        Op::new(format!("Reword {short}"), &[HEAD_IS, &head_state(dir)]).then(&[REWRITE, commit]).input(message).undo(Undo::Soft)
    };
    Ok(confirm_pushed(dir, commit, ("HEAD", "@{u}"), op))
}

/// `exec` pseudo-command: `REWRITE commit` gives `commit` the message on stdin by writing it and
/// every commit after it up to HEAD again (same trees, authors and parents, merges too; signatures
/// dropped), then moving HEAD's branch there, and the stacked branches on rewritten commits to their
/// copies ([`stacked_refs`]), in one transaction. No tree changes: the index and working tree stay.
pub(super) const REWRITE: &str = "!rewrite";

pub(super) fn rewrite(dir: &Path, commit: &str, tip: &str, message: &[u8]) -> Res<String> {
    let head = git::run(dir, &["rev-parse", tip])?.trim().to_owned();
    // Parents first: `commit` and its descendants up to HEAD.
    let ids = git::run(dir, &["rev-list", "--topo-order", "--reverse", &format!("--ancestry-path={commit}"), &head, &format!("^{commit}^@")])?;
    let raw = git::run_raw(dir, &["cat-file", "--batch"], Some(ids.as_bytes()))?;
    let committer = git::run(dir, &["var", "GIT_COMMITTER_IDENT"])?;
    let mut new = std::collections::HashMap::new();
    let mut rest = &raw[..];
    for id in ids.lines() {
        let (obj, more) = batch_object(rest).ok_or("Unexpected git cat-file output.")?;
        rest = more;
        let (hdr, msg) = obj.windows(2).position(|w| w == b"\n\n").map_or((obj, &[][..]), |i| (&obj[..i], &obj[i + 2..]));
        let (mut out, mut skip) = (vec![], false);
        for line in hdr.split(|&b| b == b'\n') {
            // A line starting with a space continues the header above it.
            skip = if line.starts_with(b" ") { skip } else { line.starts_with(b"gpgsig") || (id == commit && line.starts_with(b"encoding ")) };
            match line.strip_prefix(b"parent ").map(String::from_utf8_lossy) {
                _ if skip => continue,
                Some(p) => out.extend(format!("parent {}", new.get(p.as_ref()).map_or(p.as_ref(), String::as_str)).as_bytes()),
                None if line.starts_with(b"committer ") => out.extend(format!("committer {}", committer.trim()).as_bytes()),
                None => out.extend(line),
            }
            out.push(b'\n');
        }
        out.push(b'\n');
        match id == commit {
            true => out.extend(format!("{}\n", String::from_utf8_lossy(message).trim()).as_bytes()),
            false => out.extend(msg),
        }
        // ponytail: one hash-object per commit; a fast-import stream if rewording far back gets slow.
        let oid = git::run_input(dir, &["hash-object", "-t", "commit", "-w", "--stdin"], Some(&out))?;
        new.insert(id.to_owned(), oid.trim().to_owned());
    }
    new.get(&head).ok_or("Commit is not on the current branch.")?;
    let stacked = if super::stack::follows() { stacked_refs(dir, |oid| new.contains_key(oid), &head) } else { vec![] };
    let moves: String = [(tip.to_owned(), head)].into_iter().chain(stacked).map(|(r, old)| format!("update {r} {} {old}\n", new[&old])).collect();
    git::run_input(dir, &["update-ref", "-m", &format!("reword {}", &commit[..commit.len().min(7)]), "--stdin"], Some(moves.as_bytes()))?;
    Ok(String::new())
}

/// The first object in `git cat-file --batch` output (`<oid> <type> <size>\n<object>\n`), and the rest.
fn batch_object(out: &[u8]) -> Option<(&[u8], &[u8])> {
    let nl = out.iter().position(|&b| b == b'\n')?;
    let size: usize = std::str::from_utf8(&out[..nl]).ok()?.rsplit(' ').next()?.parse().ok()?;
    Some((out.get(nl + 1..nl + 1 + size)?, out.get(nl + 2 + size..)?))
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
    Ok(confirm_pushed(dir, oldest, ("HEAD", "@{u}"), op))
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
        let sym = git::run(dir, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default();
        let op = Op::new(format!("Amend {short}"), &[HEAD_IS, &format!("{} {commit}", sym.trim())]).then(&["commit", "--amend", "--no-edit"]).undo(Undo::Soft);
        return Ok(confirm_pushed(dir, commit, ("HEAD", "@{u}"), op));
    }
    // Same safety checks as the editor: nothing in progress, on this branch, with no merges after it.
    if !matches!(git::state(&git::git_dir(dir)?), git::State::Clean | git::State::Bisecting) {
        return Err("Finish or abort the operation in progress first.".into());
    }
    on_branch(dir, commit)?;
    pending_autosquash(Editor::load(dir, &format!("{commit}^"), short)?.items.iter().map(|i| i.message.as_str()))?;
    // Soft: undo puts the folded-in changes back in the index (`--keep` would drop them).
    // Backed up: the replayed commits may put files where ignored ones sit.
    let base = format!("{commit}^");
    let op = Op::new(format!("Amend staged into {short}"), &[HEAD_IS, &head_state(dir)])
        .then(&["commit", "-m", &format!("fixup! {commit}")])
        .then(&["-c", "sequence.editor=true", "-c", "core.editor=true", "rebase", "-i", "--autosquash", "--autostash", &base])
        .undo(Undo::Soft)
        .backup(super::Backup::Tree { revs: vec![base], untracked: false });
    Ok(confirm_pushed(dir, commit, ("HEAD", "@{u}"), op))
}

/// Uncommit `commit` so it can be split: HEAD is just reset; an older commit
/// gets a rebase that stops there (`edit`), then the reset. `rebase --continue`
/// replays the rest once the parts are committed.
pub fn split(dir: &Path, git_dir: &Path, commit: &str, is_head: bool) -> Res<Op> {
    let short = &commit[..commit.len().min(7)];
    if is_head {
        return Ok(confirm_pushed(dir, commit, ("HEAD", "@{u}"), Op::new(format!("Split {short}"), &["reset", "-q", "HEAD^"]).undo(Undo::Mixed)));
    }
    on_branch(dir, commit)?;
    let base = git::run(dir, &["rev-parse", "-q", "--verify", &format!("{commit}^")]).map_err(|_| "The root commit can't be split.".to_string())?;
    let mut ed = Editor::load(dir, base.trim(), short)?;
    ed.items.iter_mut().find(|i| i.id == commit).ok_or("Commit is not on the current branch.")?.act = Act::Edit;
    let mut op = ed.op(git_dir)?.then(&["reset", "-q", "HEAD^"]);
    op.label = format!("Split {short}");
    Ok(confirm_pushed(dir, commit, ("HEAD", "@{u}"), op))
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
        Editor { base: "base".into(), label: "base".into(), items, sel: 0, cherry: false, head: String::new(), orig: vec![], dir: Default::default() }
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

    /// ⌘Z of an interactive cherry-pick that carried an uncommitted edit of a picked file puts
    /// the file back as it was: the edit kept, the pick gone (never merged or refused).
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
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), lines(19, "nineteen"), "{:?}", msgs.iter().map(|m| &m.text).collect::<Vec<_>>());
        assert_eq!(g(&["diff", "--cached", "--name-only"]), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_mid_rebase_and_drops_applied_upstream() {
        let (dir, g) = repo("applied");
        let gd = git::git_dir(&dir).unwrap();
        // feat = c3 + A + B; main gets A cherry-picked on top of a new commit.
        g(&["checkout", "-q", "-b", "feat"]);
        for f in ["fa", "fb"] {
            std::fs::write(dir.join(f), f).unwrap();
            g(&["add", f]);
            g(&["commit", "-q", "-m", &f[1..].to_uppercase()]);
        }
        let a = g(&["rev-parse", "HEAD~1"]).trim().to_owned();
        g(&["checkout", "-q", "main"]);
        std::fs::write(dir.join("fm"), "M").unwrap();
        g(&["add", "fm"]);
        g(&["commit", "-q", "-m", "M"]);
        g(&["cherry-pick", &a]);
        g(&["checkout", "-q", "feat"]);
        let ed = Editor::load(&dir, "main", "main").unwrap();
        // A stopped rebase blocks a second editor before it writes any file.
        std::fs::create_dir_all(gd.join("rebase-merge")).unwrap();
        assert!(ed.op(&gd).is_err());
        assert!(!gd.join("glasir-rebase-todo").exists());
        std::fs::remove_dir_all(gd.join("rebase-merge")).unwrap();
        run_op(&dir, &ed.op(&gd).unwrap());
        assert_eq!(g(&["log", "--format=%s", "-3"]).lines().collect::<Vec<_>>(), ["B", "A", "M"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rewrites_of_pushed_commits_ask_first() {
        let (dir, g) = repo("pushed");
        let gd = git::git_dir(&dir).unwrap();
        let bare = dir.with_extension("bare");
        let _ = std::fs::remove_dir_all(&bare);
        git::run(&dir, &["init", "-q", "--bare", bare.to_str().unwrap()]).unwrap();
        g(&["remote", "add", "origin", bare.to_str().unwrap()]);
        g(&["push", "-q", "-u", "origin", "main"]);
        let c2 = g(&["rev-parse", "HEAD~1"]).trim().to_owned();
        assert!(quick(&dir, &gd, &c2, Act::Drop, None).unwrap().confirm.is_some());
        let ids: Vec<String> = g(&["rev-list", "HEAD"]).lines().map(String::from).collect();
        assert!(multi(&dir, &gd, &ids[..2], Act::Drop).unwrap().confirm.is_some());
        assert!(amend_into(&dir, &ids[0], true).unwrap().confirm.is_some());
        std::fs::write(dir.join("d"), "d\n").unwrap();
        g(&["add", "d"]);
        g(&["commit", "-q", "-m", "c4"]);
        let c4 = g(&["rev-parse", "HEAD"]).trim().to_owned();
        assert!(quick(&dir, &gd, &c4, Act::Reword, Some("c4 better".into())).unwrap().confirm.is_none());
        assert!(amend_into(&dir, &c4, true).unwrap().confirm.is_none());
        for act in [Act::Squash, Act::Fixup] {
            let ask = quick(&dir, &gd, &c4, act, None).unwrap().confirm;
            assert!(ask.as_deref().is_some_and(|c| c.contains("1 commit already pushed to origin/main")), "{act:?}: {ask:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&bare);
    }

    #[test]
    fn rewrites_of_commits_on_a_remote_ask_first_without_an_upstream() {
        let (dir, g) = repo("pushed-no-upstream");
        let gd = git::git_dir(&dir).unwrap();
        g(&["update-ref", "refs/remotes/origin/main", "HEAD~1"]);
        g(&["checkout", "-q", "-b", "feature"]);
        std::fs::write(dir.join("d"), "d\n").unwrap();
        g(&["add", "d"]);
        g(&["commit", "-q", "-m", "c4"]);
        let rev = |r: &str| g(&["rev-parse", r]).trim().to_owned();
        let ask = quick(&dir, &gd, &rev("HEAD~2"), Act::Drop, None).unwrap().confirm;
        assert!(ask.as_deref().is_some_and(|c| c.contains("1 commit already on a remote branch")), "{ask:?}");
        assert!(quick(&dir, &gd, &rev("HEAD~1"), Act::Drop, None).unwrap().confirm.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Triangular: tracking upstream/main, pushed to origin/feature. Commits on a remote branch
    /// other than the upstream ask first too.
    #[test]
    fn rewrites_of_commits_on_another_remote_branch_ask_first() {
        let (dir, g) = repo("pushed-triangular");
        let gd = git::git_dir(&dir).unwrap();
        for r in ["upstream", "origin"] {
            g(&["remote", "add", r, "https://example.invalid/r.git"]);
        }
        g(&["update-ref", "refs/remotes/upstream/main", "HEAD~2"]);
        g(&["checkout", "-q", "-b", "feature"]);
        g(&["branch", "-q", "--set-upstream-to=upstream/main"]);
        g(&["update-ref", "refs/remotes/origin/feature", "HEAD"]);
        std::fs::write(dir.join("d"), "d\n").unwrap();
        g(&["add", "d"]);
        g(&["commit", "-q", "-m", "c4"]);
        let rev = |r: &str| g(&["rev-parse", r]).trim().to_owned();
        let ask = quick(&dir, &gd, &rev("HEAD~1"), Act::Drop, None).unwrap().confirm;
        assert!(ask.as_deref().is_some_and(|c| c.contains("1 commit already on a remote branch")), "{ask:?}");
        assert!(quick(&dir, &gd, &rev("HEAD"), Act::Reword, Some("c4 better".into())).unwrap().confirm.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A squash melding past a dropped commit rewrites the pushed one below the drop: Start asks.
    #[test]
    fn start_asks_when_a_squash_melds_past_a_drop_into_a_pushed_commit() {
        let (dir, g) = repo("start-meld");
        std::fs::write(dir.join("d"), "d\n").unwrap();
        g(&["add", "d"]);
        g(&["commit", "-q", "-m", "c4"]);
        g(&["update-ref", "refs/remotes/origin/main", "HEAD~2"]);
        let mut ed = Editor::load(&dir, g(&["rev-parse", "HEAD~3"]).trim(), "c1").unwrap(); // c4, c3, c2
        ed.items[0].act = Act::Squash;
        ed.items[1].act = Act::Drop;
        let Ok(super::super::Action::RunRebase { op, onto, branch, first }) = ed.start(&git::git_dir(&dir).unwrap()) else { panic!("a rebase") };
        assert_eq!(first.as_deref(), Some(g(&["rev-parse", "HEAD~2"]).trim()));
        let ask = confirm_rebase(&dir, &onto, branch.as_deref(), first.as_deref(), op).confirm;
        assert!(ask.as_deref().is_some_and(|c| c.contains("1 commit already on a remote branch")), "{ask:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reword_head_refuses_once_head_moved() {
        let (dir, g) = repo("reword-head-moved");
        let gd = git::git_dir(&dir).unwrap();
        let head = g(&["rev-parse", "HEAD"]).trim().to_owned();
        let op = reword(&dir, &gd, &head, true, None, "c3 better".into()).unwrap();
        std::fs::write(dir.join("d"), "d\n").unwrap();
        g(&["add", "d"]);
        g(&["commit", "-q", "-m", "new work"]);
        assert!(crate::repo::op::exec(&dir, &op).is_err());
        let stale = reword(&dir, &gd, &head, true, None, "c3 better".into()).unwrap();
        assert!(crate::repo::op::exec(&dir, &stale).is_err());
        assert_eq!(g(&["log", "--format=%s"]).lines().collect::<Vec<_>>(), ["new work", "c3", "c2", "c1"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn amend_into_head_refuses_once_head_moved() {
        let (dir, g) = repo("amend-head-moved");
        let head = g(&["rev-parse", "HEAD"]).trim().to_owned();
        std::fs::write(dir.join("a"), "fixed\n").unwrap();
        g(&["add", "a"]);
        let op = amend_into(&dir, &head, true).unwrap();
        g(&["commit", "-q", "-m", "new work"]);
        let tip = g(&["rev-parse", "HEAD"]);
        std::fs::write(dir.join("b"), "more\n").unwrap();
        g(&["add", "b"]);
        assert!(crate::repo::op::exec(&dir, &op).is_err());
        assert_eq!(g(&["rev-parse", "HEAD"]), tip);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Another branch checked out while the amend into an older commit waited: nothing lands on it.
    #[test]
    fn amend_into_older_refuses_once_head_moved() {
        let (dir, g) = repo("amend-older-moved");
        let c2 = g(&["rev-parse", "HEAD~1"]).trim().to_owned();
        std::fs::write(dir.join("b"), "c2 fixed\n").unwrap();
        g(&["add", "b"]);
        let op = amend_into(&dir, &c2, false).unwrap();
        g(&["checkout", "-q", "-b", "other"]);
        let tip = g(&["rev-parse", "HEAD"]);
        assert!(crate::repo::op::exec(&dir, &op).is_err());
        assert_eq!((g(&["rev-parse", "other"]), g(&["diff", "--cached", "--name-only"])), (tip, "b\n".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// During a merge the fixup commit would be the merge commit: refused.
    #[test]
    fn amend_into_older_refuses_mid_merge() {
        let (dir, g) = repo("amend-mid-merge");
        g(&["checkout", "-q", "-b", "side", "HEAD~1"]);
        std::fs::write(dir.join("s"), "s\n").unwrap();
        g(&["add", "s"]);
        g(&["commit", "-q", "-m", "s"]);
        g(&["checkout", "-q", "main"]);
        g(&["merge", "-q", "--no-ff", "--no-commit", "side"]);
        let c2 = g(&["rev-parse", "HEAD~1"]).trim().to_owned();
        assert!(amend_into(&dir, &c2, false).is_err_and(|e| e.contains("in progress")));
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

    /// Settings -> "(cherry picked from commit ...)" applies to the interactive cherry-pick too:
    /// to plain picks and to rewords, not to drops.
    #[test]
    fn interactive_cherry_pick_honours_x() {
        let (dir, g) = repo("cherry-x");
        g(&["checkout", "-q", "-b", "side", "HEAD~2"]);
        for f in ["s1", "s2", "s3"] {
            std::fs::write(dir.join(f), f).unwrap();
            g(&["add", f]);
            g(&["commit", "-q", "-m", f]);
        }
        let side: Vec<String> = g(&["rev-list", "-3", "HEAD"]).lines().map(String::from).collect(); // s3, s2, s1
        g(&["checkout", "-q", "main"]);
        let mut e = Editor::cherry_pick(&dir, &side).unwrap();
        e.items[0].act = Act::Reword;
        e.items[0].message = "s3 better".into();
        e.items[1].act = Act::Drop;
        crate::repo::merge_opts::set_pick_x(true);
        let op = e.op(&git::git_dir(&dir).unwrap());
        crate::repo::merge_opts::set_pick_x(false);
        run_op(&dir, &op.unwrap());
        assert_eq!(g(&["log", "-1", "--format=%B"]).trim(), format!("s3 better\n\n(cherry picked from commit {})", side[0]));
        assert_eq!(g(&["log", "-2", "--format=%B"]).lines().filter(|l| !l.is_empty()).last().unwrap(), format!("(cherry picked from commit {})", side[2]));
        assert!(!g(&["log", "--format=%B"]).contains(&side[1]));
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
        settle(&ctx, &mut t);
        let op = t.confirm.take().expect("drop asks first");
        commit(&dir, "e", "new work");
        t.apply(&ctx, Action::Git(Op { confirm: None, ..op }));
        settle(&ctx, &mut t);
        assert_eq!(sh(&dir, &["log", "--format=%s"]), "new work\nd\nc\nb\na\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// main ← a (A1, A2) ← b (B1, B2): squashing B1 into its parent moves `a` onto the meld, so it
    /// stays below `b` (as `--update-refs --autosquash` does).
    #[test]
    fn squash_into_a_stacked_tip_moves_its_branch_onto_the_meld() {
        use crate::repo::testutil::*;
        let dir = scratch("stack-meld");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "m", "m");
        sh(&dir, &["checkout", "-q", "-b", "a"]);
        commit(&dir, "a1", "A1");
        commit(&dir, "a2", "A2");
        sh(&dir, &["checkout", "-q", "-b", "b"]);
        commit(&dir, "b1", "B1");
        commit(&dir, "b2", "B2");
        let b1 = sh(&dir, &["rev-parse", "b~1"]).trim().to_owned();
        run_op(&dir, &quick(&dir, &git::git_dir(&dir).unwrap(), &b1, Act::Squash, None).unwrap());
        assert_eq!(sh(&dir, &["rev-parse", "a"]), sh(&dir, &["rev-parse", "b~1"]));
        assert_eq!(sh(&dir, &["show", "a:b1"]), "B1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Rewording through a merge (`REWRITE`) moves the stacked branch on the way along; ⌘Z puts it back.
    #[test]
    fn reword_through_a_merge_moves_stacked_branches() {
        use crate::repo::testutil::*;
        use crate::repo::{Action, RepoTab, View};
        let dir = scratch("reword-stacked");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "m", "m");
        sh(&dir, &["checkout", "-q", "-b", "a"]);
        commit(&dir, "a1", "A1");
        sh(&dir, &["checkout", "-q", "-b", "side", "main"]);
        commit(&dir, "s", "S");
        sh(&dir, &["checkout", "-q", "a"]);
        sh(&dir, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
        sh(&dir, &["checkout", "-q", "-b", "b"]);
        commit(&dir, "b1", "B1");
        let id = |r: &str| sh(&dir, &["rev-parse", r]).trim().to_owned();
        let before = ["a", "b", "side"].map(&id);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Reword { commit: id("a~1"), message: "A1 better".into() });
        settle(&ctx, &mut t);
        assert_eq!((id("a"), id("side")), (id("b~1"), before[2].clone()));
        assert_eq!(sh(&dir, &["log", "-1", "--format=%s", "a^1"]), "A1 better\n");
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(["a", "b", "side"].map(&id), before);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
