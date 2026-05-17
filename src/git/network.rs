//! Remote-facing helpers: the never-prompting background fetch, classifying
//! its failures, and running an op with local changes auto-stashed.

use super::log::shell_quote;
use super::runner::exec;
use super::{Cancel, Res, cmd, run};
use std::path::Path;

/// The user's ssh command made to fail instead of asking (plink takes `-batch`).
fn batch_ssh(ssh: &str, variant: &str) -> String {
    match variant {
        "plink" | "putty" | "tortoiseplink" => format!("{ssh} -batch"),
        "simple" => ssh.to_owned(),
        "ssh" => format!("{ssh} -o BatchMode=yes -o ConnectTimeout=15"),
        _ if ssh.to_lowercase().contains("plink") => format!("{ssh} -batch"),
        _ => format!("{ssh} -o BatchMode=yes -o ConnectTimeout=15"),
    }
}

/// A git command that can never prompt (no askpass, no terminal, ssh in batch
/// mode), so missing credentials fail fast instead of hanging or popping up a window.
fn quiet(dir: &Path, args: &[&str]) -> Res<String> {
    let cfg = |k: &str| run(dir, &["config", "--get", k]).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let env = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
    // git's own precedence: GIT_SSH_COMMAND > core.sshCommand > GIT_SSH.
    let ssh = env("GIT_SSH_COMMAND").or_else(|| cfg("core.sshCommand")).or_else(|| env("GIT_SSH").map(|p| shell_quote(&p)));
    let variant = env("GIT_SSH_VARIANT").or_else(|| cfg("ssh.variant")).unwrap_or_default();
    let mut c = cmd(dir);
    for k in ["GIT_ASKPASS", "SSH_ASKPASS", "GLASIR_ASKPASS", "GIT_SSH"] {
        c.env_remove(k);
    }
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("GIT_SSH_COMMAND", batch_ssh(ssh.as_deref().unwrap_or("ssh"), &variant));
    let mut all = vec!["-c", "credential.interactive=false", "-c", "core.askPass="];
    all.extend(args);
    exec(c, dir, &all, None).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// `fetch --all --prune --no-tags` for background refreshes; never prompts.
pub fn fetch_quiet(dir: &Path) -> Res<String> {
    // Remote-tracking refs only: tags are local refs that undo snapshots, so a background
    // fetch adding or pruning one would invalidate the undo history. A manual Fetch gets them.
    quiet(dir, &["fetch", "--all", "--prune", "--no-tags", "--no-prune-tags", "--quiet"])
}

/// `err` lowercased without its quoted URLs and paths (`'https://…/saml-gw/'`), so a
/// repository or host name can't decide what went wrong.
fn unquoted(err: &str) -> String {
    let (mut out, mut quoted, mut prev) = (String::new(), false, ' ');
    for c in err.chars() {
        match c {
            '\'' if quoted => quoted = false,
            // Opens after a space or line start ("Couldn't" is no quote).
            '\'' if prev.is_whitespace() => quoted = true,
            _ if !quoted => out.push(c),
            _ => {}
        }
        prev = c;
    }
    out.to_lowercase()
}

/// Badge text for a failed background fetch.
pub fn fetch_failure(err: &str) -> &'static str {
    let e = unquoted(err);
    let auth = [
        "terminal prompts disabled",
        "could not read username",
        "permission denied",
        "host key verification failed",
        "authentication failed",
        "saml sso",
        "returned error: 401",
        "returned error: 403",
    ];
    if auth.iter().any(|k| e.contains(k)) { "Sign in required" } else { "Fetch failed" }
}

/// Runs `f` with all uncommitted changes (untracked too) stashed, then reapplies them.
/// If they conflict with the new state, the conflict markers stay and so does the stash.
pub fn with_autostash(dir: &Path, msg: &str, f: impl FnOnce() -> Res<String>) -> Res<String> {
    let top = || run(dir, &["rev-parse", "-q", "--verify", "refs/stash"]).unwrap_or_default();
    let before = top();
    run(dir, &["stash", "push", "-u", "-m", msg])?;
    if top() == before {
        return f(); // nothing was stashed: never pop someone else's entry
    }
    let res = f();
    // Outside any cancel scope: a cancelled op must still give the changes back.
    let pop = Cancel::default().scope(|| {
        run(dir, &["stash", "pop", "--index"]).or_else(|e| {
            // Staged changes that don't fit the new HEAD come back unstaged.
            if e.contains("--index") { run(dir, &["stash", "pop"]) } else { Err(e) }
        })
    });
    match (res, pop) {
        (res, Ok(_)) => res,
        (_, Err(e)) => Err(format!(
            "Your changes conflict with the new state: resolve the conflicts. They are also kept in the stash \"{msg}\"; drop it when you're done.\n\n{e}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::git::CANCELLED;

    #[test]
    fn background_fetch_never_prompts() {
        assert_eq!(batch_ssh("ssh", ""), "ssh -o BatchMode=yes -o ConnectTimeout=15");
        assert_eq!(batch_ssh("'C:/PuTTY/plink.exe'", ""), "'C:/PuTTY/plink.exe' -batch");
        assert_eq!(batch_ssh("ssh -i key", "plink"), "ssh -i key -batch");
        assert_eq!(fetch_failure("fatal: could not read Username for 'https://github.com': terminal prompts disabled"), "Sign in required");
        assert_eq!(fetch_failure("git@github.com: Permission denied (publickey)."), "Sign in required");
        assert_eq!(fetch_failure("ssh: Could not resolve hostname x: nodename nor servname provided"), "Fetch failed");
        assert_eq!(fetch_failure("fatal: '/srv/saml-gw.git' does not appear to be a git repository"), "Fetch failed");
    }

    /// A cancelled autostash op still gives the stashed changes back.
    #[cfg(unix)]
    #[test]
    fn cancelled_autostash_reapplies() {
        let dir = std::env::temp_dir().join(format!("glasir-cancel-autostash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| run(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "T"]);
        g(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("f"), "one\n").unwrap();
        g(&["add", "f"]);
        g(&["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("f"), "mine\n").unwrap();
        let cancel = Cancel::default();
        let c2 = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            c2.cancel();
        });
        // pull runs fetch, which runs upload-pack: cancel must stop git's children too.
        let late = dir.join("late");
        let pack = format!("remote.origin.uploadpack=sleep 1; touch '{}'; :", late.display());
        g(&["remote", "add", "origin", &dir.to_string_lossy()]);
        let res = cancel.scope(|| with_autostash(&dir, "auto", || run(&dir, &["-c", &pack, "pull", "origin", "main"])));
        assert_eq!(res, Err(CANCELLED.to_owned()));
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "mine\n");
        assert_eq!(g(&["stash", "list"]), "");
        std::thread::sleep(std::time::Duration::from_millis(1000));
        assert!(!late.exists(), "git's children kept running");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
