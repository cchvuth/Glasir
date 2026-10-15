//! Running git: the `Command` every call starts from, stdout/stderr capture,
//! cancellation of slow commands ([`Cancel`]) and `--progress` parsing.

use super::Res;
use super::log::record;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub fn cmd(dir: &Path) -> Command {
    let mut c = base("git", dir);
    // Neutralize user config and translations that would change the output we parse.
    c.args(["-c", "log.showSignature=false", "-c", "diff.noprefix=false", "-c", "diff.mnemonicPrefix=false"])
        .args(["-c", "diff.srcPrefix=a/", "-c", "diff.dstPrefix=b/"])
        .args(["-c", "core.quotepath=off", "-c", "color.ui=never", "-c", "color.diff=never"])
        .env("LANGUAGE", "en");
    c
}

/// Runs a user's shell line (custom commands) in `dir`: `sh -c`, or PowerShell on
/// Windows. Logged and cancellable like git commands; git it runs prompts through askpass.
pub fn run_shell(dir: &Path, line: &str) -> Res<String> {
    #[cfg(windows)]
    let (prog, args) = ("powershell", ["-NoProfile", "-Command", line]);
    #[cfg(not(windows))]
    let (prog, args) = ("sh", ["-c", line]);
    exec(base(prog, dir), dir, &args, None).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// A `prog` command in `dir` that never blocks on a terminal.
fn base(prog: &str, dir: &Path) -> Command {
    let mut c = Command::new(prog);
    c.current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Inherited from a parent git (e.g. launched from a hook) these would point us at another repo.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        // Our -c core.editor / sequence.editor must win.
        .env_remove("GIT_EDITOR")
        .env_remove("GIT_SEQUENCE_EDITOR")
        .stdin(Stdio::null());
    // Credential prompts go through our own askpass window; never block on a TTY.
    if let Ok(exe) = std::env::current_exe() {
        c.env("GIT_ASKPASS", &exe)
            .env("SSH_ASKPASS", &exe)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("GLASIR_ASKPASS", "1");
    }
    c.env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    // Own process group, so Cancel also stops the git subcommands it runs (pull -> fetch).
    // Nothing in it reads the TTY: stdin is null/piped and prompts go through askpass.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut c, 0);
    c
}

pub fn run(dir: &Path, args: &[&str]) -> Res<String> {
    run_input(dir, args, None)
}

pub fn run_input(dir: &Path, args: &[&str], input: Option<&[u8]>) -> Res<String> {
    run_raw(dir, args, input).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// The error of a command stopped through [`Cancel`].
pub const CANCELLED: &str = "Cancelled";

/// Stops the git command running under [`Cancel::scope`] (e.g. a slow fetch):
/// (cancelled, pid of the running git), plus the latest `--progress` line.
#[derive(Clone, Default)]
pub struct Cancel(Arc<Mutex<(bool, Option<u32>)>>, Arc<Mutex<String>>);

thread_local! {
    static SCOPE: std::cell::RefCell<Option<Cancel>> = const { std::cell::RefCell::new(None) };
}

impl Cancel {
    pub fn cancel(&self) {
        let mut g = self.0.lock().unwrap();
        g.0 = true;
        let Some(pid) = g.1 else { return };
        drop(g);
        let pid = pid.to_string();
        // SIGTERM to git's whole process group lets git remove its lock files.
        #[cfg(unix)]
        let mut c = Command::new("kill");
        #[cfg(unix)]
        c.args(["-TERM", "--", &format!("-{pid}")]);
        // taskkill /T also ends git-remote-https / ssh, which hold our pipes.
        #[cfg(windows)]
        let mut c = Command::new("taskkill");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.args(["/T", "/F", "/PID", &pid]).creation_flags(0x0800_0000);
        }
        // Off the caller's thread: search cancels on every keystroke, and taskkill is slow.
        std::thread::spawn(move || c.stdout(Stdio::null()).stderr(Stdio::null()).status());
    }

    fn active() -> Cancel {
        SCOPE.with_borrow(Clone::clone).unwrap_or_default()
    }

    fn is_cancelled(&self) -> bool {
        self.0.lock().unwrap().0
    }

    fn attach(&self, pid: u32) {
        let mut g = self.0.lock().unwrap();
        g.1 = Some(pid);
        let already_cancelled = g.0;
        drop(g);
        if already_cancelled {
            self.cancel();
        }
    }

    fn detach(&self) -> bool {
        let mut g = self.0.lock().unwrap();
        g.1 = None;
        g.0
    }

    /// Last progress line of a `--progress` command running under this scope.
    pub fn progress(&self) -> String {
        self.1.lock().unwrap().clone()
    }

    /// Runs `f` with every git command it starts on this thread cancellable through `self`.
    pub fn scope<T>(&self, f: impl FnOnce() -> T) -> T {
        let outer = SCOPE.replace(Some(self.clone()));
        let r = f();
        SCOPE.set(outer);
        r
    }
}

/// Reads `r` to the end into the buffer it returns, then drops `done`; with `progress`, keeps it
/// set to the latest progress line.
fn read_all(mut r: impl Read + Send + 'static, progress: Option<Arc<Mutex<String>>>, done: Sender<()>) -> Arc<Mutex<Vec<u8>>> {
    let v = Arc::new(Mutex::new(vec![]));
    let w = v.clone();
    std::thread::spawn(move || {
        let (_done, mut buf) = (done, vec![0u8; 1 << 16]);
        loop {
            let n = match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let mut v = w.lock().unwrap();
            v.extend_from_slice(&buf[..n]);
            if let Some(p) = &progress
                && let Some(l) = last_progress(&String::from_utf8_lossy(&v[v.len().saturating_sub(1024)..]))
            {
                *p.lock().unwrap() = l.to_owned();
            }
        }
    });
    v
}

/// Latest complete line of `--progress` output (git rewrites lines with `\r`).
fn last_progress(s: &str) -> Option<&str> {
    s.split(['\r', '\n']).rev().skip(1).map(|l| l.trim_start_matches("remote:").trim()).find(|l| !l.is_empty())
}

/// Keeps only the final state of each `\r`-rewritten progress line.
fn squash_progress(s: &str) -> String {
    s.lines().map(|l| l.rsplit('\r').find(|p| !p.trim().is_empty()).unwrap_or("")).collect::<Vec<_>>().join("\n")
}

/// `Receiving objects:  45% (450/1000), 1.2 MiB | 2 MiB/s` -> ("Receiving objects 45%", Some(0.45)).
pub fn progress_label(line: &str) -> (String, Option<f32>) {
    let pct = line.split_once('%').and_then(|(a, _)| a.rsplit(|c: char| !c.is_ascii_digit()).next()?.parse::<f32>().ok());
    match (line.split_once(':'), pct) {
        (Some((phase, _)), Some(p)) => (format!("{phase} {p}%"), Some(p / 100.0)),
        _ => (line.to_owned(), None),
    }
}

/// stdout as raw bytes.
pub fn run_raw(dir: &Path, args: &[&str], input: Option<&[u8]>) -> Res<Vec<u8>> {
    exec(cmd(dir), dir, args, input)
}

pub fn exec(mut c: Command, dir: &Path, args: &[&str], input: Option<&[u8]>) -> Res<Vec<u8>> {
    let cancel = Cancel::active();
    if cancel.is_cancelled() {
        return Err(CANCELLED.into());
    }
    let start = Instant::now();
    let prog = c.get_program().to_string_lossy().into_owned();
    #[cfg(windows)]
    let started = std::time::SystemTime::now();
    let progress = args.contains(&"--progress").then(|| cancel.1.clone());
    c.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    if input.is_some() {
        c.stdin(Stdio::piped());
    }
    let mut child = c.spawn().map_err(|e| format!("could not run {prog}: {e}"))?;
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        let data = data.to_vec();
        std::thread::spawn(move || stdin.write_all(&data));
    }
    let squash = progress.is_some();
    let (done, ended) = std::sync::mpsc::channel();
    let (out, err) = (read_all(child.stdout.take().unwrap(), None, done.clone()), read_all(child.stderr.take().unwrap(), progress, done));
    cancel.attach(child.id());
    let status = child.wait().map_err(|e| e.to_string());
    let cancelled = cancel.detach();
    let status = status?;
    if cancelled && !status.success() {
        // Don't wait for the pipes: helpers git started may still hold them.
        record(dir, &prog, args, start, status.code(), "");
        #[cfg(windows)]
        drop_index_lock(dir, started);
        return Err(CANCELLED.into());
    }
    // Background jobs (a shell line's `server &`, a hook's) inherit our pipes: don't wait for them to exit.
    let _ = ended.recv_timeout(std::time::Duration::from_millis(300));
    let take = |b: Arc<Mutex<Vec<u8>>>| std::mem::take(&mut *b.lock().unwrap());
    let (stdout, stderr) = (take(out), take(err));
    let mut stderr = String::from_utf8_lossy(&stderr).into_owned();
    if squash {
        stderr = squash_progress(&stderr);
    }
    // A custom command's output is what the user wants to see.
    let logged = if prog == "git" { stderr.clone() } else { format!("{}{stderr}", String::from_utf8_lossy(&stdout)) };
    record(dir, &prog, args, start, status.code(), &logged);
    if status.success() {
        return Ok(stdout);
    }
    let msg = format!("{}\n{}", stderr.trim(), String::from_utf8_lossy(&stdout).trim());
    let msg = msg.trim();
    Err(if msg.is_empty() { format!("{prog} {} failed ({status})", super::url::redact(&args.join(" "))) } else { msg.to_owned() })
}

/// Runs git, handing each NUL-terminated stdout record to `each` as it arrives (a long
/// history search shows its first hits at once); `each` returning false stops git.
/// Cancellable through [`Cancel::scope`] like [`run`].
pub fn stream(dir: &Path, args: &[&str], mut each: impl FnMut(&str) -> bool) -> Res<()> {
    let cancel = Cancel::active();
    if cancel.is_cancelled() {
        return Err(CANCELLED.into());
    }
    let start = Instant::now();
    let mut c = cmd(dir);
    c.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("could not run git: {e}"))?;
    let (done, ended) = std::sync::mpsc::channel();
    let err = read_all(child.stderr.take().unwrap(), None, done);
    let mut out = child.stdout.take().unwrap();
    cancel.attach(child.id());
    let (mut buf, mut chunk, mut stopped) = (vec![], [0u8; 65536], false);
    'read: while let Ok(n @ 1..) = out.read(&mut chunk) {
        buf.extend_from_slice(&chunk[..n]);
        let mut at = 0;
        while let Some(i) = buf[at..].iter().position(|&b| b == 0) {
            if !each(&String::from_utf8_lossy(&buf[at..at + i])) {
                stopped = true;
                let _ = child.kill();
                break 'read;
            }
            at += i + 1;
        }
        buf.drain(..at);
    }
    if !stopped && !buf.is_empty() {
        each(&String::from_utf8_lossy(&buf));
    }
    drop(out);
    let status = child.wait().map_err(|e| e.to_string());
    let cancelled = cancel.detach();
    let status = status?;
    let _ = ended.recv_timeout(std::time::Duration::from_millis(300));
    let stderr = String::from_utf8_lossy(&err.lock().unwrap()).into_owned();
    record(dir, "git", args, start, status.code(), &stderr);
    match () {
        _ if cancelled => Err(CANCELLED.into()),
        _ if stopped || status.success() => Ok(()),
        _ => Err(if stderr.trim().is_empty() { format!("git {} failed ({status})", super::url::redact(&args.join(" "))) } else { stderr.trim().to_owned() }),
    }
}

/// `taskkill /F` skips git's cleanup: remove the index.lock the killed git created.
#[cfg(windows)]
fn drop_index_lock(dir: &Path, since: std::time::SystemTime) {
    use std::path::PathBuf;
    let Ok(o) = cmd(dir).args(["rev-parse", "--absolute-git-dir"]).output() else { return };
    let lock = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()).join("index.lock");
    if std::fs::metadata(&lock).and_then(|m| m.modified()).is_ok_and(|t| t >= since) {
        let _ = std::fs::remove_file(lock);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{LOG, Logged};

    #[test]
    fn progress_parsing() {
        let s = "Enumerating objects: 5, done.\nReceiving objects:  10% (1/10)\rReceiving objects:  45% (450/1000), 1.2 MiB | 2 MiB/s\rReceiving obj";
        assert_eq!(last_progress(s), Some("Receiving objects:  45% (450/1000), 1.2 MiB | 2 MiB/s"));
        assert_eq!(last_progress("remote: Compressing objects:  50% (1/2)\r"), Some("Compressing objects:  50% (1/2)"));
        assert_eq!(last_progress("partial"), None);
        assert_eq!(progress_label("Receiving objects:  45% (450/1000), 1.2 MiB"), ("Receiving objects 45%".into(), Some(0.45)));
        assert_eq!(progress_label("Enumerating objects: 5, done."), ("Enumerating objects: 5, done.".into(), None));
        assert_eq!(squash_progress("Counting:  1%\rCounting: 100% (2/2), done.\r\nTo /x.git\n"), "Counting: 100% (2/2), done.\nTo /x.git");
    }

    #[test]
    fn runs_in_odd_paths_and_logs_without_stdin() {
        let dir = std::env::temp_dir().join(format!("glasir runner ünïcødé {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| run(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "T"]);
        g(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("ä b.txt"), "x\n").unwrap();
        g(&["add", "--", "ä b.txt"]);
        run_input(&dir, &["commit", "-q", "-F", "-"], Some(b"msg with sk-ant-secret\n")).unwrap();
        assert_eq!(g(&["log", "--format=%s"]).trim(), "msg with sk-ant-secret");
        assert_eq!(g(&["ls-files", "-z"]), "ä b.txt\0");
        assert!(run(&dir, &["rev-parse", "nope"]).is_err());
        assert!(run(&dir, &["-c", "alias.no=!false", "no", "https://u:tok@h.example/r"]).is_err_and(|e| e.contains("https://***@h.example/r") && !e.contains("tok")));
        let log = LOG.lock().unwrap();
        let mine: Vec<&Logged> = log.iter().filter(|e| e.cwd == dir).collect();
        assert!(mine.iter().any(|e| e.args == ["commit", "-q", "-F", "-"] && e.code == Some(0)));
        assert!(mine.iter().any(|e| e.args == ["rev-parse", "nope"] && e.code == Some(128) && !e.stderr.is_empty()));
        assert!(mine.iter().all(|e| !e.shell().contains("secret") && !e.stderr.contains("secret")));
        drop(log);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn cancel_stops_a_running_command() {
        use std::time::Duration;
        let dir = std::env::temp_dir();
        let cancel = Cancel::default();
        let c2 = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            c2.cancel();
        });
        let t = Instant::now();
        let res = cancel.scope(|| run(&dir, &["-c", "alias.slow=!sleep 5", "slow"]));
        assert_eq!(res, Err(CANCELLED.to_owned()));
        assert!(t.elapsed() < Duration::from_secs(4), "returned without waiting for the sleep");
        // Later commands in a cancelled scope don't start.
        assert_eq!(cancel.scope(|| run(&dir, &["--version"])), Err(CANCELLED.to_owned()));
        assert!(run(&dir, &["--version"]).is_ok(), "the scope ends with the closure");
    }

    #[cfg(unix)]
    #[test]
    fn shell_line_returns_while_its_background_job_runs() {
        let t = Instant::now();
        assert_eq!(run_shell(&std::env::temp_dir(), "sleep 3 &").unwrap(), "");
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(run_shell(&std::env::temp_dir(), "echo hi").unwrap(), "hi\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_hooks_background_job_does_not_hold_the_commit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("glasir-hook-bg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("hooks")).unwrap();
        run(&dir, &["init", "-q"]).unwrap();
        let hook = dir.join("hooks/post-commit");
        std::fs::write(&hook, "#!/bin/sh\necho hooked\nsleep 5 &\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hooks = format!("core.hooksPath={}", dir.join("hooks").display());
        let t = Instant::now();
        run(&dir, &["-c", &hooks, "-c", "user.email=t@t", "-c", "user.name=T", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "x"]).unwrap();
        assert!(t.elapsed() < std::time::Duration::from_secs(3), "{:?}", t.elapsed());
        assert!(LOG.lock().unwrap().iter().any(|e| e.cwd == dir && e.args.contains(&"commit".to_owned()) && e.stderr.contains("hooked")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
