//! The process environment git runs in: importing the login shell's env on
//! Unix and checking that the installed git is new enough.

use super::run;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::process::{Command, Stdio};

/// Oldest git we support (`merge-tree --write-tree`, `--update-refs`).
pub const MIN_GIT: (u32, u32) = (2, 38);

/// `git version 2.39.3 (Apple Git-146)` / `git version 2.45.1.windows.1` -> (2, 39) / (2, 45).
pub fn parse_version(s: &str) -> Option<(u32, u32)> {
    let mut it = s.trim().strip_prefix("git version ")?.split(|c: char| !c.is_ascii_digit());
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// What to tell the user when git is missing or too old; None when it is fine.
pub fn check_git() -> Option<String> {
    let install = if cfg!(target_os = "macos") {
        "Install the Xcode Command Line Tools (xcode-select --install) or Homebrew git (brew install git), then restart Glasir."
    } else if cfg!(windows) {
        "Install Git for Windows from git-scm.com/download/win, then restart Glasir."
    } else {
        "Install a current git with your distribution's package manager (e.g. sudo apt install git), then restart Glasir."
    };
    let (want, have) = (MIN_GIT, run(&std::env::temp_dir(), &["--version"]));
    match have.as_deref().map(parse_version) {
        Err(_) => Some(format!("Git was not found. Glasir needs git {}.{} or newer. {install}", want.0, want.1)),
        Ok(Some(v)) if v < want => Some(format!("Git {}.{} is too old: Glasir needs {}.{} or newer. {install}", v.0, v.1, want.0, want.1)),
        Ok(_) => None,
    }
}

/// Apps started from Finder, the Dock or a desktop launcher get a bare environment:
/// no Homebrew PATH (git, git-lfs), ssh-agent socket or GPG settings. Copy the login
/// shell's. Call before any other thread starts.
#[cfg(unix)]
pub fn import_login_env() {
    use std::io::IsTerminal;
    if std::env::var_os("TERM").is_some() && std::io::stdin().is_terminal() {
        return; // started from a terminal: the env is already right
    }
    let Some(shell) = std::env::var_os("SHELL") else { return };
    let Ok(mut child) = Command::new(shell)
        .args(["-ilc", "echo __GLASIR_ENV__; env -0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let mut out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut v = vec![];
        let _ = out.read_to_end(&mut v);
        let _ = tx.send(v);
    });
    // A slow or hanging shell profile must not keep the app from starting.
    let Ok(out) = rx.recv_timeout(std::time::Duration::from_secs(10)) else {
        let _ = child.kill();
        return;
    };
    let _ = (reader.join(), child.wait());
    for (k, v) in parse_env(&out) {
        // SAFETY: called from main before eframe or any git thread starts; the reader thread has exited.
        unsafe { std::env::set_var(k, v) };
    }
}

/// `env -0` output after our marker, minus shell-session bookkeeping.
#[cfg(unix)]
fn parse_env(out: &[u8]) -> Vec<(String, String)> {
    let s = String::from_utf8_lossy(out);
    let Some((_, env)) = s.split_once("__GLASIR_ENV__\n") else { return vec![] };
    env.split('\0')
        .filter_map(|kv| kv.split_once('='))
        .filter(|(k, _)| !k.is_empty() && !matches!(*k, "PWD" | "OLDPWD" | "SHLVL" | "_" | "TERM"))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("git version 2.39.3 (Apple Git-146)\n"), Some((2, 39)));
        assert_eq!(parse_version("git version 2.45.1.windows.1"), Some((2, 45)));
        assert_eq!(parse_version("zsh: command not found: git"), None);
        assert!(parse_version("git version 2.37.1").unwrap() < MIN_GIT);
    }

    #[cfg(unix)]
    #[test]
    fn login_env_parsing() {
        let env = parse_env(b"motd noise__GLASIR_ENV__\nPATH=/opt/homebrew/bin:/usr/bin\0PWD=/x\0SHLVL=2\0_=/usr/bin/env\0A=b=c\0\0");
        assert_eq!(env, [("PATH".to_owned(), "/opt/homebrew/bin:/usr/bin".to_owned()), ("A".to_owned(), "b=c".to_owned())]);
        assert!(parse_env(b"PATH=/x\0").is_empty(), "no marker: the shell failed");
    }
}
