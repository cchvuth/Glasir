//! In-memory log of the last 500 git commands (for the command-log window),
//! with credentials in URLs and `core.sshCommand` values redacted.

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
    let ssh = |k: &str| k.eq_ignore_ascii_case("core.sshCommand");
    let entry = Logged {
        prog: prog.into(),
        args: args
            .iter()
            .enumerate()
            .map(|(i, a)| match a.split_once('=') {
                Some((k, v)) if ssh(k) => format!("{k}={}", super::url::redact_command(v)),
                _ if i > 0 && ssh(args[i - 1]) => super::url::redact_command(a),
                _ => super::url::redact(a),
            })
            .collect(),
        cwd: dir.to_path_buf(),
        ms: start.elapsed().as_millis(),
        code,
        stderr: lines[lines.len().saturating_sub(20)..].join("\n"),
    };
    if entry.ms >= 30 && std::env::var_os("GLASIR_PERF").is_some() {
        eprintln!("perf: {} ms  {}", entry.ms, entry.shell());
    }
    let mut log = LOG.lock().unwrap();
    if log.len() >= 500 {
        log.pop_front();
    }
    log.push_back(entry);
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
        let dir = std::env::temp_dir().join(format!("glasir-log-redact-{}", std::process::id()));
        for a in ["https://me:ghp_secret@github.com/o/r.git", "git fetch https://a.example/x; git push https://u:ghp_tok@b.example/y", "https://u:p@ss@host/r", "git@github.com:o/r.git", "https://example.com/o/r@v1"] {
            record(&dir, "sh", &["-c", a], Instant::now(), Some(0), "");
        }
        let logged: Vec<String> = LOG.lock().unwrap().iter().filter(|e| e.cwd == dir).map(|e| e.args[1].clone()).collect();
        assert_eq!(logged, ["https://***@github.com/o/r.git", "git fetch https://a.example/x; git push https://***@b.example/y", "https://***@host/r", "git@github.com:o/r.git", "https://example.com/o/r@v1"]);
        let e = Logged { prog: "git".into(), args: vec!["log".into(), "--format=%H %s".into(), "it's".into()], cwd: "/tmp/a b".into(), ms: 0, code: Some(0), stderr: String::new() };
        assert_eq!(e.shell(), r"git -C '/tmp/a b' log '--format=%H %s' 'it'\''s'");
        let e = Logged { prog: "sh".into(), args: vec!["-c".into(), "echo hi".into()], ..e };
        assert_eq!(e.shell(), "cd '/tmp/a b' && sh -c 'echo hi'");
    }

    #[test]
    fn command_log_hides_ssh_command_passwords() {
        let dir = std::env::temp_dir().join(format!("glasir-log-ssh-{}", std::process::id()));
        record(&dir, "git", &["config", "core.sshCommand", "sshpass -p hunter2 ssh"], Instant::now(), Some(0), "");
        record(&dir, "git", &["-c", "core.sshcommand=sshpass -p hunter2 ssh", "fetch"], Instant::now(), Some(0), "");
        let shown: Vec<String> = LOG.lock().unwrap().iter().filter(|e| e.cwd == dir).map(Logged::shell).collect();
        assert!(shown.len() == 2 && shown.iter().all(|s| s.contains("sshpass -p *** ssh") && !s.contains("hunter2")), "{shown:?}");
    }
}
