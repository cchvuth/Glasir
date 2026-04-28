//! Interactive rebase engine. Builds a todo list ([`Editor`]) and hands it to
//! git's own `rebase -i` (via `sequence.editor`) as an [`Op`], so conflicts,
//! --continue/--abort and hooks behave exactly like on the command line; plus
//! one-shot rewrites (quick, amend_into). The window is `crate::ui::rebase_editor`.

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
    /// HEAD (symbolic ref and commit) when loaded; the todo is only valid for it.
    head: String,
    dir: std::path::PathBuf,
}

impl Editor {
    pub fn load(dir: &Path, base: &str, label: &str) -> Res<Editor> {
        git::run(dir, &["merge-base", "--is-ancestor", base, "HEAD"])
            .map_err(|_| format!("{label} is not an ancestor of HEAD."))?;
        let out = git::run(dir, &["log", "-z", "--format=%H%x1f%P%x1f%an%x1f%B", &format!("{base}..HEAD"), "--"])?;
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
        if items.is_empty() {
            return Err(format!("No commits after {label} on this branch."));
        }
        Ok(Editor { base: base.into(), label: label.into(), items, sel: 0, head: head_state(dir), dir: dir.into() })
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
        let mut todo = String::new();
        for (n, it) in self.items.iter().enumerate().rev() {
            let verb = match it.act {
                Act::Pick | Act::Reword => "pick",
                Act::Squash => "squash",
                Act::Fixup => "fixup",
                Act::Drop => "drop",
            };
            todo += &format!("{verb} {}\n", it.id);
            if it.act == Act::Reword {
                let msg = git_dir.join(format!("glasir-msg-{n}"));
                std::fs::write(&msg, format!("{}\n", it.message.trim())).map_err(|e| e.to_string())?;
                todo += &format!("exec git commit --amend --allow-empty -F {}\n", sh_quote(&msg));
            }
        }
        let path = git_dir.join("glasir-rebase-todo");
        std::fs::write(&path, todo).map_err(|e| e.to_string())?;
        let editor = format!("sequence.editor=cat {} >", sh_quote(&path));
        Ok(Op::new(format!("Rebase onto {}", self.label), &["-c", &editor, "-c", "core.editor=true", "rebase", "-i", &self.base])
            .undo(Undo::Keep))
    }
}

/// One-step history edit of a single commit on the current branch
/// (reword / squash into parent / drop), reusing the rebase engine.
pub fn quick(dir: &Path, git_dir: &Path, commit: &str, act: Act, message: Option<String>) -> Res<Op> {
    // Squash/fixup meld into the parent, so the parent must be in the todo too.
    let back = if matches!(act, Act::Squash | Act::Fixup) { "~2" } else { "^" };
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

/// Commit the staged changes as a fixup of `commit` and fold it in (autosquash).
pub fn amend_into(dir: &Path, commit: &str, is_head: bool) -> Res<Op> {
    let short = &commit[..commit.len().min(7)];
    if is_head {
        return Ok(Op::new(format!("Amend {short}"), &["commit", "--amend", "--no-edit"]).undo(Undo::Soft));
    }
    // Same safety checks as the editor: an ancestor of HEAD, with no merges in between.
    Editor::load(dir, &format!("{commit}^"), short)?;
    Ok(Op::new(format!("Amend staged into {short}"), &["commit", "--fixup", commit])
        .then(&["-c", "sequence.editor=true", "-c", "core.editor=true", "rebase", "-i", "--autosquash", "--autostash", &format!("{commit}^")])
        .undo(Undo::Keep))
}

/// `refs/heads/x <sha>` (or just the sha when detached).
fn head_state(dir: &Path) -> String {
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
        Editor { base: "base".into(), label: "base".into(), items, sel: 0, head: String::new(), dir: Default::default() }
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
        assert_eq!(lines[2], "pick c1");
        assert!(lines[3].starts_with("exec git commit --amend --allow-empty -F '") && lines[3].ends_with("glasir-msg-1'"));
        assert_eq!(lines[4], "fixup c0");
        assert_eq!(std::fs::read_to_string(dir.join("glasir-msg-1")).unwrap(), "new message\n");
        assert!(op.cmds[0].iter().any(|a| a.starts_with("sequence.editor=cat '")));
        assert_eq!(op.cmds[0].last().unwrap(), "base");
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
        let op = e.op(&git::git_dir(&dir).unwrap()).unwrap();
        for cmd in &op.cmds {
            let a: Vec<&str> = cmd.iter().map(String::as_str).collect();
            git::run(&dir, &a).unwrap();
        }
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
        for cmd in &op.cmds {
            let a: Vec<&str> = cmd.iter().map(String::as_str).collect();
            git::run(dir, &a).unwrap();
        }
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
    fn rejects_squash_into_nothing() {
        assert!(ed(&[Act::Pick, Act::Squash]).problem().is_some());
        assert!(ed(&[Act::Squash, Act::Pick]).problem().is_none());
        assert!(ed(&[Act::Pick, Act::Fixup, Act::Drop]).problem().is_some()); // dropped oldest doesn't count
        assert!(ed(&[Act::Drop, Act::Drop]).problem().is_some());
    }
}
