//! External tools: open the repo or a file in the user's editor, a terminal or
//! the file manager, and the user's custom commands ({repo} {branch} {sha} {path}).

use super::op::{Backup, Op, Undo};
use super::undo::{Files, UndoEntry, backed_up, backup};
use super::{DiffSrc, Msg, RepoTab, Task};
use crate::git::{self, Res};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

/// Marks an op command that is a shell line rather than git args (`op::exec`).
pub(super) const SHELL: &str = "!shell";

/// A user-defined command: git args (`git push origin HEAD:refs/for/{branch}`) or a shell line.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Custom {
    pub name: String,
    pub command: String,
}

impl Custom {
    pub fn uses(&self, key: &str) -> bool {
        self.command.contains(&format!("{{{key}}}"))
    }
}

/// Settings → Tools. Empty editor / terminal = detected default.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tools {
    pub editor: String,
    pub terminal: String,
    pub commands: Vec<Custom>,
}

/// Mirrors Settings → Tools (set by the app every frame), like `ai::enabled`.
static TOOLS: Mutex<Tools> = Mutex::new(Tools { editor: String::new(), terminal: String::new(), commands: Vec::new() });

pub fn set(t: &Tools) {
    let mut g = TOOLS.lock().unwrap();
    if *g != *t {
        g.clone_from(t);
    }
}

/// Custom commands that take `{key}`; with `None`, those without a `{path}` (palette).
pub fn commands(key: Option<&str>) -> Vec<Custom> {
    let g = TOOLS.lock().unwrap();
    let ok = |c: &&Custom| !c.name.trim().is_empty() && !c.command.trim().is_empty() && key.map_or(!c.uses("path"), |k| c.uses(k));
    g.commands.iter().filter(ok).cloned().collect()
}

/// Placeholder values for one run ({repo} is the tab's folder).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vars {
    pub branch: String,
    pub sha: String,
    pub path: String,
}

/// Replaces `{key}` with `quote(value)` in one pass, so values are never re-expanded.
fn fill(s: &str, vals: &[(&str, &str)], quote: impl Fn(&str) -> String) -> String {
    let (mut out, mut rest) = (String::new(), s);
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        match vals.iter().find(|(k, _)| rest.strip_prefix(k).is_some_and(|r| r.starts_with('}'))) {
            Some((k, v)) => {
                out.push_str(&quote(v));
                rest = &rest[k.len() + 1..];
            }
            None => out.push('{'),
        }
    }
    out + rest
}

/// One shell word: `sh` single quotes, or PowerShell's (which also treat curly quotes as quotes).
fn quote(s: &str) -> String {
    if cfg!(windows) {
        format!("'{}'", s.replace(['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'], "''"))
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The first `{key}` inside quotes the user wrote or right after an escape: there ours would
/// end theirs (`''$(…)''`) or be literal (`"'$(…)'"`, `\'$(…)'`), leaving the value open to the shell.
fn quoted_key<'a>(line: &str, keys: &[&'a str]) -> Option<&'a str> {
    let esc = if cfg!(windows) { '`' } else { '\\' };
    let key = |i: usize| keys.iter().find(|k| line[i + 1..].strip_prefix(**k).is_some_and(|r| r.starts_with('}'))).copied();
    let (mut open, mut chars) = (None, line.char_indices());
    while let Some((i, c)) = chars.next() {
        let c = match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' if cfg!(windows) => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' if cfg!(windows) => '"',
            c => c,
        };
        match (open, c) {
            (Some('\''), '\'') | (Some('"'), '"') => open = None,
            (Some(_), '{') if key(i).is_some() => return key(i),
            (Some('\'' | '$'), _) => {}
            (_, c) if c == esc => {
                if let Some((j, '{')) = chars.next()
                    && key(j).is_some()
                {
                    return key(j);
                }
            }
            // ANSI-C `$'…'`: where it ends depends on the shell (`\'`), so every later key counts as quoted.
            (None, '$') if !cfg!(windows) && line[i + 1..].starts_with('\'') => {
                chars.next();
                open = Some('$');
            }
            (None, '\'' | '"') => open = Some(c),
            _ => {}
        }
    }
    None
}

/// The op that runs `c`: plain `git …` lines run without a shell (one word per argument),
/// anything else through `sh -c` / PowerShell with the values quoted. Ref changes undo with ⌘Z.
/// Refuses quoted placeholders in shell lines ([`quoted_key`]).
pub fn op(c: &Custom, dir: &Path, v: &Vars) -> Res<Op> {
    let repo = dir.to_string_lossy();
    let vals = [("repo", &*repo), ("branch", &*v.branch), ("sha", &*v.sha), ("path", &*v.path)];
    let line = c.command.trim();
    let op = match line.strip_prefix("git ") {
        Some(rest) if !rest.contains(['|', '&', ';', '<', '>', '$', '`', '"', '\'', '*', '\\']) => {
            let args: Vec<String> = rest.split_whitespace().map(|w| fill(w, &vals, str::to_owned)).collect();
            Op::new(c.name.trim(), &args.iter().map(String::as_str).collect::<Vec<_>>())
        }
        _ => {
            if let Some(k) = quoted_key(line, &vals.map(|(k, _)| k)) {
                return Err(format!("Remove the quotes or escape around {{{k}}} in custom command '{}': Glasir quotes values itself.", c.name.trim()));
            }
            Op::new(c.name.trim(), &[SHELL, &fill(line, &vals, quote)])
        }
    };
    Ok(Op { custom: true, ..op.undo(Undo::Keep) })
}

/// The tool `git mergetool --gui` / `difftool --gui` runs, or why it can't run from here.
pub fn tool(d: &super::RepoData, diff: bool) -> Result<&str, String> {
    let (kind, keys): (_, &[_]) = if diff { ("diff", &["diff.guitool", "merge.guitool", "diff.tool", "merge.tool"]) } else { ("merge", &["merge.guitool", "merge.tool"]) };
    let t = keys.iter().find_map(|k| d.cfg(k)).ok_or_else(|| format!("No {kind} tool configured: git config --global {kind}.tool <name>"))?;
    // These need a terminal (we have none): git would exit at once.
    if t.starts_with("vimdiff") || t.starts_with("nvimdiff") || t == "emerge" {
        return Err(format!("{t} runs in a terminal: set a windowed tool with git config --global {kind}.guitool <name>"));
    }
    Ok(t)
}

const MERGE_TOOL: &str = "Open in external merge tool";
const DIFF_TOOL: &str = "Open in external diff tool";

/// For a tool op's label: the kind of tool window ("merge" / "diff") it opens.
pub(super) fn kind(label: &str) -> Option<&'static str> {
    match label {
        MERGE_TOOL => Some("merge"),
        DIFF_TOOL => Some("diff"),
        _ => None,
    }
}

/// Put a conflicted file's markers back (the WIP conflict menu).
pub fn restore_markers_op(path: &str) -> Op {
    Op::new("Restore conflict", &["--literal-pathspecs", "checkout", "-m", "--", path]).light().backup(Backup::Paths(vec![path.into()]))
}

/// `git mergetool` for one conflicted file. It runs beside other ops ([`Open`]); the file is
/// backed up before the tool starts, and ⌘Z restores it if the tool changed it. So is
/// `<file>.orig`, where `mergetool.keepBackup` (on by default) moves git's backup over any
/// file there; its temporary files have the pid in their names.
pub fn mergetool_op(path: &str) -> Op {
    let paths = vec![path.into(), format!("{path}.orig")];
    Op::new(MERGE_TOOL, &["--literal-pathspecs", "mergetool", "--gui", "--no-prompt", "--", path]).light().backup(Backup::Paths(paths))
}

/// `git difftool` for the file a diff shows; `None` where there is nothing to compare
/// (untracked files, root commits). Watched like a merge tool, so its errors show.
pub fn difftool_op(src: &DiffSrc) -> Option<Op> {
    let mut a = vec!["--literal-pathspecs", "difftool", "--gui", "--no-prompt"];
    let c = src.change();
    match src {
        DiffSrc::Worktree { change, staged } => {
            if change.code == '?' {
                return None;
            }
            a.extend(staged.then_some("--cached"));
        }
        DiffSrc::Commit { id, parent, .. } => {
            a.push(parent.as_deref()?);
            // No second revision: the working tree.
            a.extend((id != crate::git::WORKTREE).then_some(id.as_str()));
        }
    }
    a.push("--");
    a.extend(c.old_path.as_deref());
    a.push(&c.path);
    Some(Op::new(DIFF_TOOL, &a).light())
}

/// An external merge / diff tool window, watched outside the op slot so other ops run meanwhile.
pub struct Open {
    pub kind: &'static str,
    pub path: String,
    cancel: git::Cancel,
    task: Task<(Op, Res<String>, Result<Option<Files>, String>)>,
}

/// Commands that rewrite the working tree beyond the paths they name.
const WHOLE_TREE: &[&str] = &["checkout", "switch", "reset", "restore", "rebase", "merge", "pull", "stash", "cherry-pick", "revert", "am", "clean", "apply", "mergetool", super::stack::RESTACK, super::merge_opts::PICK_NEW, super::rebase::ABORT_ANY, super::op::STASH_POP, super::op::STASH_APPLY, super::op::STASH_UNPOP];

/// Whether `op` would change `path`: it names it (or a folder holding it), or rewrites the tree
/// without naming paths (a hard reset, checkout, rebase, discard all).
pub(super) fn touches(op: &Op, path: &str) -> bool {
    let named = |p: &str| p == path || p == "." || p == ":/" || path.strip_prefix(p.trim_end_matches('/')).is_some_and(|r| r.starts_with('/'));
    let backup = match &op.backup {
        Some(Backup::Paths(ps) | Backup::Staged(ps)) => ps.iter().any(|p| named(p)),
        Some(Backup::Tree { .. } | Backup::Stash(_)) => true,
        None => false,
    };
    let input = op.input.as_deref().is_some_and(|i| i.split(|&b| b == 0).any(|p| std::str::from_utf8(p).is_ok_and(named)));
    backup
        || input
        || op.cmds.iter().any(|c| {
            let dashes = c.iter().position(|a| a == "--");
            let paths = dashes.is_some() || c.iter().any(|a| a.starts_with("--pathspec-from-file"));
            // The subcommand: past global options (`-c key=value`, `--literal-pathspecs`).
            let mut args = c.iter();
            let sub = std::iter::from_fn(|| {
                let a = args.next()?;
                if a == "-c" || a == "-C" {
                    args.next();
                }
                Some(a)
            })
            .find(|a| !a.starts_with('-'));
            dashes.is_some_and(|i| c[i + 1..].iter().any(|p| named(p))) || (!paths && sub.is_some_and(|s| WHOLE_TREE.contains(&s.as_str())))
        })
}

impl RepoTab {
    /// Starts a merge / diff tool op beside the op slot. The backup is taken before the tool
    /// starts and kept only if the file changed by the time it exits.
    pub(super) fn open_tool(&mut self, ctx: &egui::Context, op: Op, kind: &'static str) {
        let path = op.cmds[0].last().cloned().unwrap_or_default();
        let (dir, cancel) = (self.dir.clone(), git::Cancel::default());
        let c = cancel.clone();
        let task = Task::spawn(ctx, move || {
            let saved = match op.backup.as_ref().map(|b| backup(&dir, b, &op.label)).transpose() {
                Ok(s) => s.flatten(),
                Err(e) => return (op, Err(e), Ok(None)),
            };
            let out = c.scope(|| super::op::exec(&dir, &op));
            let files = op.backup.as_ref().map_or(Ok(None), |b| backed_up(&dir, b, saved, None, &op.label));
            (op, out, files)
        });
        self.ext_tools.push(Open { kind, path, cancel, task });
    }

    pub(super) fn close_tool(&self, path: &str) {
        self.ext_tools.iter().filter(|t| t.path == path).for_each(|t| t.cancel.cancel());
    }

    /// Whether a merge tool is open on `path`: it would overwrite what the merge editor saves.
    pub fn merge_tool_open(&self, path: &str) -> bool {
        self.ext_tools.iter().any(|t| t.kind == "merge" && t.path == path)
    }

    /// The path of an open merge tool that `touched` says an op would change: a note says why it waits.
    pub(super) fn tool_blocks(&mut self, label: &str, touched: impl Fn(&str) -> bool) -> bool {
        let Some(t) = self.ext_tools.iter().find(|t| t.kind == "merge" && touched(&t.path)) else { return false };
        let text = format!("'{label}' skipped: the external merge tool is open on {}. Close it (or press Cancel in the conflict banner) first.", t.path);
        self.notes.push(Msg { text, error: true });
        true
    }

    /// Finishes the tools that closed: errors toast, a changed file's backup becomes an undo step.
    /// Waits while an op, undo or redo runs: a new step then would clear the redo it moves.
    pub(super) fn poll_tools(&mut self, ctx: &egui::Context, msgs: &mut Vec<Msg>) {
        if self.running.is_some() {
            return;
        }
        let mut done = vec![];
        self.ext_tools.retain(|t| match t.task.0.try_recv() {
            Ok(r) => {
                done.push(r);
                false
            }
            Err(e) => e == std::sync::mpsc::TryRecvError::Empty,
        });
        for (op, res, files) in done {
            match res {
                Err(e) if e == git::CANCELLED => msgs.push(Msg { text: format!("{} cancelled", op.label), error: false }),
                Err(e) => msgs.push(Msg { text: format!("{} failed:\n{e}", op.label), error: true }),
                Ok(_) => {}
            }
            match files {
                Ok(Some(f)) => {
                    self.undo.push(UndoEntry::new(op.label.clone(), None, vec![f], None));
                    self.clear_redo();
                    msgs.push(Msg { text: format!("{} ✔ Undo restores the file as it was before", op.label), error: false });
                }
                Ok(None) => {}
                Err(note) => msgs.push(Msg { text: note, error: true }),
            }
            self.refresh_diff(ctx);
            self.reload(ctx, false);
        }
    }
}

/// `prog` on PATH (with Windows' .exe / .cmd), or as given when it is a path.
fn which(prog: &str) -> Option<PathBuf> {
    if prog.contains(['/', '\\']) {
        return Path::new(prog).is_file().then(|| prog.into());
    }
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    // Not `is_file`: it follows reparse points, and Windows' app aliases (WindowsApps\wt.exe) can't be followed.
    let file = |p: &PathBuf| p.symlink_metadata().is_ok_and(|m| !m.is_dir());
    std::env::split_paths(&std::env::var_os("PATH")?).flat_map(|d| exts.iter().map(move |e| d.join(format!("{prog}{e}")))).find(file)
}

/// Editors that need a terminal: useless when launched from a GUI.
fn gui_editor(cmd: &str) -> bool {
    let prog = cmd.split_whitespace().next().unwrap_or("");
    let name = prog.rsplit(['/', '\\']).next().unwrap_or(prog);
    !cmd.trim().is_empty() && !["vi", "vim", "nvim", "nano", "pico", "micro", "hx", "helix", "kak", "ed", "emacs", "mg", "joe", "ne"].contains(&name)
}

/// $VISUAL / $EDITOR when it is a GUI editor, else VS Code, Cursor, Zed or Sublime Text on PATH.
pub fn default_editor() -> String {
    let env = ["VISUAL", "EDITOR"].into_iter().filter_map(|v| std::env::var(v).ok()).filter(|v| gui_editor(v));
    let found = env.chain(["code", "cursor", "zed", "subl"].map(String::from)).find(|c| which(c.split_whitespace().next().unwrap_or("")).is_some());
    match found {
        Some(c) => c,
        // No `code` shell command installed: the app itself, else the default text editor.
        None if cfg!(target_os = "macos") && Path::new("/Applications/Visual Studio Code.app").exists() => "open -b com.microsoft.VSCode".into(),
        None if cfg!(target_os = "macos") => "open -t".into(),
        None => "code".into(),
    }
}

/// The platform's terminal: iTerm or Terminal (macOS), Windows Terminal or PowerShell,
/// x-terminal-emulator (or a common one) on Linux.
pub fn default_terminal() -> &'static str {
    if cfg!(target_os = "macos") {
        if Path::new("/Applications/iTerm.app").exists() { "open -a iTerm" } else { "open -a Terminal" }
    } else if cfg!(windows) {
        if which("wt").is_some() { "wt -d" } else { "powershell" }
    } else {
        ["x-terminal-emulator", "gnome-terminal", "konsole", "xfce4-terminal", "xterm"].into_iter().find(|t| which(t).is_some()).unwrap_or("x-terminal-emulator")
    }
}

/// Starts `template` (split on whitespace; `{path}` replaced, else `path` appended) in `cwd`.
fn launch(template: &str, path: &Path, cwd: &Path, append: bool) -> std::io::Result<()> {
    let p = path.to_string_lossy();
    let mut words: Vec<String> = template.split_whitespace().map(|w| w.replace("{path}", &p)).collect();
    if append && !template.contains("{path}") {
        words.push(p.into_owned());
    }
    let Some(first) = words.first() else { return Err(std::io::Error::other("no command set")) };
    // A console shell given our null stdio would read NUL and exit: `start` gives it a
    // new console window with its own handles (cmd itself stays windowless).
    let console = cfg!(windows) && matches!(first.to_lowercase().trim_end_matches(".exe"), "powershell" | "pwsh");
    if console {
        words.splice(0..0, ["cmd", "/c", "start", ""].map(String::from)); // "" = window title
    }
    let prog = which(&words[0]).unwrap_or_else(|| words[0].clone().into());
    let mut c = Command::new(prog);
    c.args(&words[1..]).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    if console {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = c.spawn()?;
    std::thread::spawn(move || child.wait()); // reap it
    Ok(())
}

/// Opens `path` (a file, or the repo folder) in the editor from Settings → Tools or the detected one.
pub fn open_editor(dir: &Path, path: &Path) -> Res<()> {
    let set = TOOLS.lock().unwrap().editor.clone();
    let cmd = if set.trim().is_empty() { default_editor() } else { set };
    launch(&cmd, path, dir, true).map_err(|e| format!("Could not open the editor ({cmd}): {e}\nSet your editor in Settings → Tools."))
}

/// Opens a terminal in `dir` (Settings → Tools, `{path}` = the folder, or the platform's).
pub fn open_terminal(dir: &Path) -> Res<()> {
    let set = TOOLS.lock().unwrap().terminal.clone();
    let custom = !set.trim().is_empty();
    let cmd = if custom { set } else { default_terminal().to_owned() };
    // macOS `open -a` and `wt -d` take the folder as an argument; the others start in it.
    let append = cmd.starts_with("open ") || cmd.starts_with("wt ");
    launch(&cmd, dir, dir, append).map_err(|e| format!("Could not open a terminal ({cmd}): {e}\nSet a terminal command in Settings → Tools."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::testutil::{commit, identity, scratch, sh};

    #[test]
    fn placeholders_fill_once() {
        let vals = [("branch", "{sha}"), ("sha", "abc")];
        assert_eq!(fill("x {branch} {sha} {nope} {", &vals, str::to_owned), "x {sha} abc {nope} {");
        assert_eq!(fill("{sha}{sha}", &vals, |s| format!("<{s}>")), "<abc><abc>");
        if !cfg!(windows) {
            assert_eq!(quote("it's"), r"'it'\''s'");
        }
    }

    #[test]
    fn git_lines_skip_the_shell() {
        let v = Vars { branch: "feat/x".into(), sha: "abc".into(), path: "a b.txt".into() };
        let dir = Path::new("/r");
        let o = op(&Custom { name: "Gerrit".into(), command: "git push origin HEAD:refs/for/{branch}".into() }, dir, &v).unwrap();
        assert_eq!(o.cmds, vec![vec!["push", "origin", "HEAD:refs/for/feat/x"]]);
        assert_eq!(o.undo, Undo::Keep);
        let o = op(&Custom { name: "Log".into(), command: "git log -1 -- {path}".into() }, dir, &v).unwrap();
        assert_eq!(o.cmds[0].last().unwrap(), "a b.txt");
        let o = op(&Custom { name: "Count".into(), command: "git log --oneline | wc -l".into() }, dir, &v).unwrap();
        assert_eq!(o.cmds[0][0], SHELL);
        assert!(!gui_editor("vim") && !gui_editor("/usr/bin/nano -w") && gui_editor("code --wait") && !gui_editor(" "));
    }

    #[test]
    fn custom_commands_run_in_the_repo() {
        let dir = scratch("custom-cmd");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "one\n");
        let head = sh(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        let v = Vars { branch: "main".into(), sha: head.clone(), path: "it's here.txt".into() };
        // A git line: a tag named after the branch at {sha}.
        let o = op(&Custom { name: "Tag".into(), command: "git tag mark-{branch} {sha}".into() }, &dir, &v).unwrap();
        super::super::op::exec(&dir, &o).unwrap();
        assert_eq!(sh(&dir, &["rev-parse", "mark-main^{commit}"]).trim(), head);
        // A shell line; the quote in {path} stays data.
        if !cfg!(windows) {
            let o = op(&Custom { name: "Note".into(), command: "echo {branch} > {path} && cat {path}".into() }, &dir, &v).unwrap();
            assert_eq!(super::super::op::exec(&dir, &o).unwrap().trim(), "main");
            assert_eq!(std::fs::read_to_string(dir.join("it's here.txt")).unwrap(), "main\n");
            let log = crate::git::LOG.lock().unwrap();
            assert!(log.iter().any(|e| e.cwd == dir && e.prog == "sh" && e.stderr.trim() == "main"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Quotes around a placeholder would undo ours (`"'$(…)'"`, `''$(…)''`): refused.
    #[test]
    fn quoted_placeholders_are_refused() {
        let v = Vars { branch: "x$(touch${IFS}PWNED)".into(), sha: "abc".into(), path: "$(touch PWNED2).txt".into() };
        let dir = Path::new("/r");
        let run = |command: &str| op(&Custom { name: "C".into(), command: command.into() }, dir, &v);
        let esc = if cfg!(windows) { "`" } else { "\\" };
        // An escape right before ours turns its opening quote into a literal one.
        let escaped = [format!("echo {esc}{{path}}{esc}'"), format!("echo \"{esc}{{path}}\"")];
        for command in ["echo \"{branch}\"", "prettier --write '{path}'", "echo \"a $(cat '{sha}')\"", &escaped[0], &escaped[1]] {
            assert!(run(command).is_err_and(|e| e.contains("Remove the quotes")), "{command}");
        }
        // ANSI-C quoting: `\'` ends it in some shells only, so ours may land inside.
        #[cfg(unix)]
        for command in ["echo $'\\'{path}", "echo $'\\'' {path}'"] {
            assert!(run(command).is_err(), "{command}");
        }
        let o = run(&format!("echo {{branch}} \"{{nope}}\" {esc}\"{{sha}}")).unwrap();
        assert_eq!(o.cmds[0][1], format!("echo 'x$(touch${{IFS}}PWNED)' \"{{nope}}\" {esc}\"'abc'"));
    }

    /// A custom command's free-text name must not trigger the label-keyed follow-ups.
    #[test]
    fn custom_names_dont_act_like_builtin_ops() {
        use crate::repo::testutil::settle;
        use crate::repo::{Action, RepoTab, View};
        let dir = scratch("custom-label");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "one\n");
        std::fs::write(dir.join("a.txt"), "staged\n").unwrap();
        sh(&dir, &["add", "a.txt"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.summary = "draft".into();
        for (name, command) in [("Commit lint", "git status"), ("Compose up", "git no-such-command")] {
            t.apply(&ctx, Action::Custom(Custom { name: name.into(), command: command.into() }, Vars::default()));
            settle(&ctx, &mut t);
        }
        assert_eq!(t.summary, "draft");
        assert_eq!(sh(&dir, &["show", ":a.txt"]), "staged\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ops_touching_a_tools_file() {
        let p = "src/a.txt";
        let op = |args: &[&str]| Op::new("x", args);
        for o in [
            op(&["--literal-pathspecs", "add", "--", "src/a.txt"]),
            op(&["checkout", "--", "src"]),
            op(&["-c", "core.editor=true", "merge", "--continue"]),
            op(&["reset", "--hard", "HEAD"]),
            op(&["stash", "push", "-u"]),
            op(&["add", "-A", "--pathspec-from-file=-", "--pathspec-file-nul"]).input("b\0src/a.txt\0"),
            op(&["clean", "-fd"]).backup(Backup::Tree { revs: vec![], untracked: true }),
        ] {
            assert!(touches(&o, p), "{:?}", o.cmds);
        }
        for o in [
            op(&["--literal-pathspecs", "add", "--", "src/ab.txt", "z"]),
            op(&["reset", "-q", "--pathspec-from-file=-", "--pathspec-file-nul"]).input("b\0"),
            op(&["fetch", "--all"]),
            op(&["branch", "-f", "x", "HEAD"]),
            op(&["commit", "-F", "-"]),
        ] {
            assert!(!touches(&o, p), "{:?}", o.cmds);
        }
    }

    /// The merge tool runs beside other ops: only ops on its file (or the whole tree) wait.
    /// Its backup undoes what the tool wrote, an unchanged file leaves no undo step, and a
    /// failing tool shows its error.
    #[cfg(unix)]
    #[test]
    fn merge_tool_runs_beside_ops_and_undoes() {
        use crate::repo::testutil::settle;
        use crate::repo::{Action, Msg, RepoTab, View};
        let dir = scratch("mergetool-undo");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "feature"]);
        commit(&dir, "a.txt", "feature\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "a.txt", "main\n");
        assert!(crate::git::run(&dir, &["merge", "feature"]).is_err());
        std::fs::write(dir.join("a.txt"), "hand edit\n").unwrap();
        for kv in [["merge.tool", "fake"], ["mergetool.fake.cmd", "sleep 2; cp \"$REMOTE\" \"$MERGED\""], ["mergetool.fake.trustExitCode", "true"], ["mergetool.keepBackup", "false"]] {
            sh(&dir, &["config", kv[0], kv[1]]);
        }
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        let texts = |m: &[Msg]| m.iter().map(|m| m.text.clone()).collect::<Vec<_>>();
        let closed = |t: &mut RepoTab| {
            let mut msgs = vec![];
            assert!((0..400).any(|_| {
                t.poll(&ctx, &mut msgs);
                t.ext_tools.is_empty() || { std::thread::sleep(std::time::Duration::from_millis(25)); false }
            }));
            msgs.extend(settle(&ctx, t));
            msgs
        };
        t.apply(&ctx, Action::Git(mergetool_op("a.txt")));
        assert!(t.busy().is_none() && t.ext_tools.len() == 1);
        assert!(t.merge_tool_open("a.txt") && !t.merge_tool_open("z"));
        std::fs::write(dir.join("z"), "z\n").unwrap();
        t.apply(&ctx, Action::Git(crate::repo::Op::new("Stage z", &["--literal-pathspecs", "add", "--", "z"]).light()));
        let msgs = settle(&ctx, &mut t);
        assert!(msgs.iter().all(|m| !m.error) && t.notes.is_empty(), "{:?} {:?}", texts(&msgs), texts(&t.notes));
        assert!(sh(&dir, &["status", "--porcelain"]).contains("A  z"));
        // The tool's file, or the whole tree: refused while it is open.
        t.apply(&ctx, Action::Git(crate::repo::Op::new("Mark resolved", &["--literal-pathspecs", "add", "--", "a.txt"]).light()));
        t.apply(&ctx, Action::Git(crate::repo::sequencer_op("merge", true)));
        assert!(t.ext_tools.len() == 1 && t.busy().is_none());
        let notes = texts(&std::mem::take(&mut t.notes));
        assert!(notes.len() == 2 && notes.iter().all(|n| n.contains("skipped") && n.contains("merge tool is open on a.txt")), "{notes:?}");
        let msgs = closed(&mut t);
        assert!(!t.merge_tool_open("a.txt"));
        assert!(msgs.iter().all(|m| !m.error), "{:?}", texts(&msgs));
        let read = || std::fs::read_to_string(dir.join("a.txt")).unwrap();
        assert_eq!(read(), "feature\n");
        t.apply(&ctx, Action::Undo);
        let msgs = settle(&ctx, &mut t);
        assert!(msgs.iter().all(|m| !m.error), "{:?}", texts(&msgs));
        assert_eq!(read(), "hand edit\n");
        assert!(sh(&dir, &["status", "--porcelain"]).contains("UU a.txt"));

        // Closed without saving: the file is as it was, but the tool marked it resolved: ⌘Z
        // brings the conflict back.
        sh(&dir, &["config", "mergetool.fake.cmd", "true"]);
        t.apply(&ctx, Action::Git(mergetool_op("a.txt")));
        closed(&mut t);
        assert_eq!((t.undo_label(), read()), (Some(MERGE_TOOL), "hand edit\n".into()));
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert!(sh(&dir, &["status", "--porcelain"]).contains("UU a.txt"));
        assert_eq!(read(), "hand edit\n");
        sh(&dir, &["checkout", "-m", "--", "a.txt"]);
        // Nothing changed at all: nothing to undo.
        sh(&dir, &["config", "mergetool.fake.trustExitCode", "false"]);
        let undo = t.undo_label().map(str::to_owned);
        t.apply(&ctx, Action::Git(mergetool_op("a.txt")));
        closed(&mut t);
        assert_eq!(t.undo_label().map(str::to_owned), undo);
        sh(&dir, &["config", "mergetool.fake.trustExitCode", "true"]);

        sh(&dir, &["config", "mergetool.fake.cmd", "echo tool broke >&2; exit 3"]);
        t.apply(&ctx, Action::Git(mergetool_op("a.txt")));
        let msgs = closed(&mut t);
        assert!(msgs.iter().any(|m| m.error && m.text.contains("tool broke")), "{:?}", texts(&msgs));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `git mergetool` keeps its backup as `<file>.orig` (keepBackup defaults to true), over
    /// one of the user's own: ⌘Z puts that back too.
    #[cfg(unix)]
    #[test]
    fn merge_tool_undo_restores_an_orig_file() {
        use crate::repo::testutil::settle;
        use crate::repo::{Action, RepoTab, View};
        let dir = scratch("mergetool-orig");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        commit(&dir, "a.txt", "base\n");
        sh(&dir, &["checkout", "-q", "-b", "feature"]);
        commit(&dir, "a.txt", "feature\n");
        sh(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "a.txt", "main\n");
        assert!(crate::git::run(&dir, &["merge", "feature"]).is_err());
        for kv in [["merge.tool", "fake"], ["mergetool.fake.cmd", "cp \"$REMOTE\" \"$MERGED\""], ["mergetool.fake.trustExitCode", "true"]] {
            sh(&dir, &["config", kv[0], kv[1]]);
        }
        std::fs::write(dir.join("a.txt.orig"), "MINE\n").unwrap();
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        settle(&ctx, &mut t);
        t.apply(&ctx, Action::Git(mergetool_op("a.txt")));
        let mut msgs = vec![];
        assert!((0..400).any(|_| {
            t.poll(&ctx, &mut msgs);
            t.ext_tools.is_empty() || { std::thread::sleep(std::time::Duration::from_millis(25)); false }
        }));
        settle(&ctx, &mut t);
        let read = |p: &str| std::fs::read_to_string(dir.join(p)).unwrap();
        assert_ne!(read("a.txt.orig"), "MINE\n");
        t.apply(&ctx, Action::Undo);
        settle(&ctx, &mut t);
        assert_eq!(read("a.txt.orig"), "MINE\n");
        assert!(sh(&dir, &["status", "--porcelain"]).contains("UU a.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn conflict_kinds_bulk_resolve_and_external_tools() {
        use crate::repo::data::load_all;
        use crate::repo::op::exec;
        use crate::repo::View;
        let dir = scratch("conflict-tools");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        for f in ["a.txt", "b.txt", "c.txt"] {
            commit(&dir, f, "base\n");
        }
        sh(&dir, &["checkout", "-q", "-b", "feature"]);
        for f in ["a.txt", "b.txt"] {
            commit(&dir, f, "feature\n");
        }
        sh(&dir, &["rm", "-q", "c.txt"]);
        sh(&dir, &["commit", "-q", "-m", "drop c"]);
        sh(&dir, &["checkout", "-q", "main"]);
        for f in ["a.txt", "b.txt", "c.txt"] {
            commit(&dir, f, "main\n");
        }
        assert!(crate::git::run(&dir, &["merge", "feature"]).is_err());

        let d = load_all(&dir, 100, View::default()).unwrap();
        let kinds: Vec<_> = ["a.txt", "b.txt", "c.txt"].map(|f| crate::git::conflict_kind(&d.status.conflict_xy[f]).0).to_vec();
        assert_eq!(kinds, ["Both modified", "Both modified", "Deleted by them"]);
        assert!(d.conflict_sides[0].starts_with("main ") && d.conflict_sides[1].starts_with("feature "), "{:?}", d.conflict_sides);
        assert!(d.resolvable.is_empty(), "every file still has markers or a deleted side");

        // b.txt is resolved by hand: only it can be bulk-marked.
        std::fs::write(dir.join("b.txt"), "both\n").unwrap();
        let d = load_all(&dir, 100, View::default()).unwrap();
        assert_eq!(d.resolvable, ["b.txt"]);
        sh(&dir, &["config", "merge.tool", "fake"]);
        for kv in [["mergetool.fake.cmd", "cp \"$REMOTE\" \"$MERGED\""], ["mergetool.fake.trustExitCode", "true"], ["mergetool.keepBackup", "false"]] {
            sh(&dir, &["config", kv[0], kv[1]]);
        }
        let d2 = load_all(&dir, 100, View::default()).unwrap();
        // difftool falls back to merge.tool; terminal tools are refused.
        assert_eq!((tool(&d2, false), tool(&d2, true)), (Ok("fake"), Ok("fake")));
        sh(&dir, &["config", "merge.guitool", "vimdiff"]);
        let d2 = load_all(&dir, 100, View::default()).unwrap();
        assert!(tool(&d2, true).is_err_and(|e| e.contains("terminal")));
        sh(&dir, &["config", "--unset", "merge.guitool"]);

        // The merge tool runs on a backed-up file and takes the incoming side here.
        let op = mergetool_op("a.txt");
        assert!(op.backup.is_some());
        exec(&dir, &op).unwrap();
        let wait = |ok: &dyn Fn() -> bool| (0..400).any(|_| ok() || { std::thread::sleep(std::time::Duration::from_millis(25)); false });
        let unmerged = || crate::git::status(&dir).unwrap().conflicted.iter().map(|c| c.path.clone()).collect::<Vec<_>>();
        assert!(wait(&|| !unmerged().contains(&"a.txt".to_owned())), "{:?}", unmerged());
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "feature\n");

        // Bulk resolve stages just the ready file.
        let mut a = vec!["--literal-pathspecs", "add", "--"];
        a.extend(d.resolvable.iter().map(String::as_str));
        exec(&dir, &Op::new("Mark all resolved", &a)).unwrap();
        assert_eq!(unmerged(), ["c.txt"]);

        // Diff tool: working-tree file against the index.
        std::fs::write(dir.join("b.txt"), "edited\n").unwrap();
        let out = dir.join("seen.out");
        sh(&dir, &["config", "difftool.fake.cmd", &format!("cp \"$LOCAL\" '{}'", out.display())]);
        let st = crate::git::status(&dir).unwrap();
        let change = st.unstaged.iter().find(|c| c.path == "b.txt").unwrap().clone();
        let op = difftool_op(&DiffSrc::Worktree { change: change.clone(), staged: false }).unwrap();
        exec(&dir, &op).unwrap();
        assert!(wait(&|| out.exists()));
        assert_eq!(&op.cmds[0][1..], ["difftool", "--gui", "--no-prompt", "--", "b.txt"]);
        let staged = difftool_op(&DiffSrc::Worktree { change: change.clone(), staged: true }).unwrap();
        assert_eq!(staged.cmds[0][4], "--cached");
        let new = crate::git::Change { code: '?', ..change.clone() };
        assert!(difftool_op(&DiffSrc::Worktree { change: new, staged: false }).is_none());
        let id = "abc".to_owned();
        assert!(difftool_op(&DiffSrc::Commit { id: id.clone(), parent: None, change: change.clone() }).is_none());
        let c = difftool_op(&DiffSrc::Commit { id, parent: Some("p".into()), change: change.clone() }).unwrap();
        assert_eq!(&c.cmds[0][3..], ["--no-prompt", "p", "abc", "--", "b.txt"]);
        // Working-tree compare: `<base> -- path` diffs the base against the file on disk.
        let wt = difftool_op(&DiffSrc::Commit { id: crate::git::WORKTREE.into(), parent: Some("HEAD".into()), change }).unwrap();
        assert_eq!(&wt.cmds[0][3..], ["--no-prompt", "HEAD", "--", "b.txt"]);
        std::fs::remove_file(&out).unwrap();
        exec(&dir, &wt).unwrap();
        assert!(wait(&|| out.exists()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
