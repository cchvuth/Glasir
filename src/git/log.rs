//! In-memory log of the last 500 git commands (for the command-log window),
//! with credentials in URLs redacted.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

/// One finished git invocation. Never holds stdin (commit messages, patches).
#[derive(Clone)]
pub struct Logged {
    /// `git`, or the shell a custom command ran in.
    pub prog: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub ms: u128,
    /// None when killed by a signal (cancelled).
    pub code: Option<i32>,
    /// Last lines of stderr.
    pub stderr: String,
}

impl Logged {
    /// Paste-able into a POSIX shell.
    pub fn shell(&self) -> String {
        let cwd = self.cwd.to_string_lossy();
        if self.prog != "git" {
            let line = std::iter::once(self.prog.as_str()).chain(self.args.iter().map(String::as_str)).map(shell_quote).collect::<Vec<_>>().join(" ");
            return format!("cd {} && {line}", shell_quote(&cwd));
        }
        ["git", "-C", &cwd].into_iter().chain(self.args.iter().map(String::as_str)).map(shell_quote).collect::<Vec<_>>().join(" ")
    }
}

pub static LOG: Mutex<std::collections::VecDeque<Logged>> = Mutex::new(std::collections::VecDeque::new());

pub(super) fn record(dir: &Path, prog: &str, args: &[&str], start: Instant, code: Option<i32>, stderr: &str) {
    let lines: Vec<&str> = stderr.trim_end().lines().collect();
    let entry = Logged {
        prog: prog.into(),
        args: args.iter().map(|a| redact(a)).collect(),
        cwd: dir.to_path_buf(),
        ms: start.elapsed().as_millis(),
        code,
        stderr: lines[lines.len().saturating_sub(20)..].join("\n"),
    };
    let mut log = LOG.lock().unwrap();
    if log.len() >= 500 {
        log.pop_front();
    }
    log.push_back(entry);
}

/// Hides credentials embedded in a URL: `https://user:token@host` -> `https://***@host`.
fn redact(arg: &str) -> String {
    if let Some(i) = arg.find("://")
        && let Some(at) = arg[i + 3..].find('@')
        && !arg[i + 3..i + 3 + at].contains('/')
    {
        return format!("{}://***{}", &arg[..i], &arg[i + 3 + at..]);
    }
    arg.to_owned()
}

pub(super) fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,@+%^".contains(c)) {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_log_redacts_and_quotes() {
        assert_eq!(redact("https://me:ghp_secret@github.com/o/r.git"), "https://***@github.com/o/r.git");
        assert_eq!(redact("git@github.com:o/r.git"), "git@github.com:o/r.git");
        assert_eq!(redact("https://example.com/o/r@v1"), "https://example.com/o/r@v1");
        let e = Logged { prog: "git".into(), args: vec!["log".into(), "--format=%H %s".into(), "it's".into()], cwd: "/tmp/a b".into(), ms: 0, code: Some(0), stderr: String::new() };
        assert_eq!(e.shell(), r"git -C '/tmp/a b' log '--format=%H %s' 'it'\''s'");
        let e = Logged { prog: "sh".into(), args: vec!["-c".into(), "echo hi".into()], ..e };
        assert_eq!(e.shell(), "cd '/tmp/a b' && sh -c 'echo hi'");
    }
}
