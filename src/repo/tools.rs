//! External tools: open the repo or a file in the user's editor, a terminal or
//! the file manager, and the user's custom commands ({repo} {branch} {sha} {path}).

use super::op::{Backup, Op, Undo};
use super::DiffSrc;
use crate::git::Res;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

/// Marks an op command that is a shell line rather than git args (`op::exec`).
pub(super) const SHELL: &str = "!shell";

/// Marks an op command that starts git and returns at once (a merge / diff tool's window).
pub(super) const DETACH: &str = "!detach";

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

pub(super) fn detach(dir: &Path, args: &[&str]) -> Res<String> {
    let mut c = crate::git::cmd(dir);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = c.spawn().map_err(|e| format!("Could not start git: {e}"))?;
    std::thread::spawn(move || child.wait()); // reap it
    Ok(String::new())
}

/// `git mergetool` for one conflicted file. It rebuilds the working file from the index
/// stages, dropping any hand edits, so the file is backed up first (⌘Z).
pub fn mergetool_op(path: &str) -> Op {
    Op::new("Open in external merge tool", &[DETACH, "--literal-pathspecs", "mergetool", "--no-prompt", "--", path]).light().backup(Backup::Paths(vec![path.into()]))
}

/// `git difftool` for the file a diff shows; `None` where there is nothing to compare
/// (untracked files, root commits).
pub fn difftool_op(src: &DiffSrc) -> Option<Op> {
    let mut a = vec![DETACH, "--literal-pathspecs", "difftool", "--no-prompt"];
    let c = src.change();
    match src {
        DiffSrc::Worktree { change, staged } => {
            if change.code == '?' {
                return None;
            }
            a.extend(staged.then_some("--cached"));
        }
        DiffSrc::Commit { id, parent, .. } => a.extend([parent.as_deref()?, id.as_str()]),
    }
    a.push("--");
    a.extend(c.old_path.as_deref());
    a.push(&c.path);
    Some(Op::new("Open in external diff tool", &a).light())
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
        sh(&dir, &["config", "diff.tool", "fake"]);
        for kv in [["mergetool.fake.cmd", "cp \"$REMOTE\" \"$MERGED\""], ["mergetool.fake.trustExitCode", "true"], ["mergetool.keepBackup", "false"]] {
            sh(&dir, &["config", kv[0], kv[1]]);
        }
        assert_eq!(load_all(&dir, 100, View::default()).unwrap().cfg("merge.tool"), Some("fake"));

        // The merge tool runs detached on a backed-up file and takes the incoming side here.
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

        // Diff tool: working-tree file against the index, run detached.
        std::fs::write(dir.join("b.txt"), "edited\n").unwrap();
        let out = dir.join("seen.out");
        sh(&dir, &["config", "difftool.fake.cmd", &format!("cp \"$LOCAL\" '{}'", out.display())]);
        let st = crate::git::status(&dir).unwrap();
        let change = st.unstaged.iter().find(|c| c.path == "b.txt").unwrap().clone();
        let op = difftool_op(&DiffSrc::Worktree { change: change.clone(), staged: false }).unwrap();
        exec(&dir, &op).unwrap();
        assert!(wait(&|| out.exists()));
        assert_eq!(&op.cmds[0][2..], ["difftool", "--no-prompt", "--", "b.txt"]);
        let staged = difftool_op(&DiffSrc::Worktree { change: change.clone(), staged: true }).unwrap();
        assert_eq!(staged.cmds[0][4], "--cached");
        let new = crate::git::Change { code: '?', ..change.clone() };
        assert!(difftool_op(&DiffSrc::Worktree { change: new, staged: false }).is_none());
        let id = "abc".to_owned();
        assert!(difftool_op(&DiffSrc::Commit { id: id.clone(), parent: None, change: change.clone() }).is_none());
        let c = difftool_op(&DiffSrc::Commit { id, parent: Some("p".into()), change }).unwrap();
        assert_eq!(&c.cmds[0][3..], ["--no-prompt", "p", "abc", "--", "b.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
