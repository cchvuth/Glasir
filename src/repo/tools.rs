//! External tools: open the repo or a file in the user's editor, a terminal or
//! the file manager, and the user's custom commands ({repo} {branch} {sha} {path}).

use super::op::{Op, Undo};
use crate::git::Res;
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

/// The op that runs `c`: plain `git …` lines run without a shell (one word per argument),
/// anything else through `sh -c` / PowerShell with the values quoted. Ref changes undo with ⌘Z.
pub fn op(c: &Custom, dir: &Path, v: &Vars) -> Op {
    let repo = dir.to_string_lossy();
    let vals = [("repo", &*repo), ("branch", &*v.branch), ("sha", &*v.sha), ("path", &*v.path)];
    let line = c.command.trim();
    let op = match line.strip_prefix("git ") {
        Some(rest) if !rest.contains(['|', '&', ';', '<', '>', '$', '`', '"', '\'', '*', '\\']) => {
            let args: Vec<String> = rest.split_whitespace().map(|w| fill(w, &vals, str::to_owned)).collect();
            Op::new(c.name.trim(), &args.iter().map(String::as_str).collect::<Vec<_>>())
        }
        _ => Op::new(c.name.trim(), &[SHELL, &fill(line, &vals, quote)]),
    };
    Op { custom: true, ..op.undo(Undo::Keep) }
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
        let o = op(&Custom { name: "Gerrit".into(), command: "git push origin HEAD:refs/for/{branch}".into() }, dir, &v);
        assert_eq!(o.cmds, vec![vec!["push", "origin", "HEAD:refs/for/feat/x"]]);
        assert_eq!(o.undo, Undo::Keep);
        let o = op(&Custom { name: "Log".into(), command: "git log -1 -- {path}".into() }, dir, &v);
        assert_eq!(o.cmds[0].last().unwrap(), "a b.txt");
        let o = op(&Custom { name: "Count".into(), command: "git log --oneline | wc -l".into() }, dir, &v);
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
        let o = op(&Custom { name: "Tag".into(), command: "git tag mark-{branch} {sha}".into() }, &dir, &v);
        super::super::op::exec(&dir, &o).unwrap();
        assert_eq!(sh(&dir, &["rev-parse", "mark-main^{commit}"]).trim(), head);
        // A shell line; the quote in {path} stays data.
        if !cfg!(windows) {
            let o = op(&Custom { name: "Note".into(), command: "echo {branch} > {path} && cat {path}".into() }, &dir, &v);
            assert_eq!(super::super::op::exec(&dir, &o).unwrap().trim(), "main");
            assert_eq!(std::fs::read_to_string(dir.join("it's here.txt")).unwrap(), "main\n");
            let log = crate::git::LOG.lock().unwrap();
            assert!(log.iter().any(|e| e.cwd == dir && e.prog == "sh" && e.stderr.trim() == "main"));
        }
        let _ = std::fs::remove_dir_all(&dir);
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
}
