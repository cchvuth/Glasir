//! Remote-facing helpers: the never-prompting background fetch and connection
//! check, classifying their failures, and running an op with local changes auto-stashed.

use super::log::shell_quote;
use super::runner::exec;
use super::{Cancel, Res, cmd, run};
use std::path::Path;

/// The user's ssh command made to fail instead of asking (plink takes `-batch`).
fn batch_ssh(ssh: &str, variant: &str) -> String {
    let is_plink = match variant {
        "plink" | "putty" | "tortoiseplink" => true,
        "simple" => return ssh.to_owned(),
        "ssh" => false,
        _ => ssh.to_lowercase().contains("plink"),
    };
    if is_plink { format!("{ssh} -batch") } else { format!("{ssh} -o BatchMode=yes -o ConnectTimeout=15") }
}

/// A git command that can never prompt (no askpass, no terminal, ssh in batch
/// mode), so missing credentials fail fast instead of hanging or popping up a window.
fn quiet(dir: &Path, args: &[&str], input: Option<&[u8]>) -> Res<String> {
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
    exec(c, dir, &all, input).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// `fetch --all --prune --no-tags` for background refreshes; never prompts.
pub fn fetch_quiet(dir: &Path) -> Res<String> {
    // Remote-tracking refs only: tags are local refs that undo snapshots, so a background
    // fetch adding or pruning one would invalidate the undo history. A manual Fetch gets them.
    quiet(dir, &["fetch", "--all", "--prune", "--no-tags", "--no-prune-tags", "--quiet"], None)
}

pub fn fetch_remote_quiet(dir: &Path, remote: &str) -> Res<String> {
    quiet(dir, &["fetch", "--no-tags", "--quiet", remote], None)
}

/// Talks to `remote` the way the background fetch does (never prompting): its HEAD.
pub fn ls_remote_quiet(dir: &Path, remote: &str) -> Res<String> {
    quiet(dir, &["ls-remote", remote, "HEAD"], None)
}

/// The token (password) git's credential helper holds for `https://<host>`, without
/// ever prompting. Only stderr reaches the command log, never the answer on stdout.
pub fn credential_token(dir: &Path, host: &str) -> Option<String> {
    let out = quiet(dir, &["credential", "fill"], Some(format!("protocol=https\nhost={host}\n\n").as_bytes())).ok()?;
    out.lines().find_map(|l| l.strip_prefix("password=")).filter(|p| !p.is_empty()).map(str::to_owned)
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

/// What a failed connection means and how to fix it: (problem, hint).
pub fn diagnose(err: &str, ssh: bool) -> (&'static str, &'static str) {
    let e = unquoted(err);
    let has = |ks: &[&str]| ks.iter().any(|k| e.contains(k));
    if has(&["saml sso", "enforced saml", "single sign-on"]) {
        ("SAML SSO not authorized", "The organization enforces single sign-on and your token or SSH key isn't authorized for it. On GitHub: Settings → Developer settings → Personal access tokens (or Settings → SSH keys) → Configure SSO → Authorize for the organization, then check again.")
    } else if has(&["ssl certificate", "certificate verify", "local issuer", "self signed", "self-signed", "server certificate verification", "schannel", "unable to get issuer"]) {
        ("TLS certificate not trusted", "Git doesn't trust the server's certificate, usually a company CA or a TLS-inspecting proxy. Point git at your CA bundle: git config --global http.sslCAInfo /path/to/company-ca.pem (on Windows: git config --global http.sslBackend schannel to use the Windows store). Never disable http.sslVerify.")
    } else if has(&["host key verification failed", "remote host identification has changed", "no matching host key", "host key for"]) {
        ("Unknown or changed SSH host key", "SSH doesn't know this server's host key yet (or it changed). Run ssh -T git@<host> once in a terminal and compare the fingerprint with the one your admin or GitHub publishes before accepting; if the server was re-keyed, remove the old line with ssh-keygen -R <host>.")
    } else if has(&["could not resolve host", "could not resolve hostname", "nodename nor servname", "name or service not known", "no such host", "temporary failure in name resolution"]) {
        ("Host not found (DNS)", "The host name doesn't resolve. Check the remote URL for typos and connect to the VPN: company GitHub Enterprise hosts usually resolve only inside the network.")
    } else if has(&["proxy connect", "could not resolve proxy", "from proxy", "connect tunnel failed", "could not connect to server", "couldn't connect to server", "failed to connect", "connection timed out", "operation timed out", "connection refused", "network is unreachable", "connection reset"]) {
        ("Can't connect (network, proxy or firewall)", "The server can't be reached. Check the VPN; if your network needs a proxy, set it for git: git config --global http.proxy http://proxy.example:8080 (or the HTTPS_PROXY variable). SSH on port 22 is often blocked: try the HTTPS URL.")
    } else if ssh && has(&["permission denied", "publickey"]) {
        ("SSH key rejected", "The server didn't accept any of your SSH keys. Load your key into the agent (ssh-add), add its public half to your account (Settings → SSH and GPG keys), and make sure the remote's user is git.")
    } else if has(&["terminal prompts disabled", "could not read username", "could not read password", "authentication failed", "invalid username or password", "returned error: 401", "returned error: 403", "permission denied", "access denied"]) {
        ("Sign-in required", "Git has no working credentials for this host. Fetch or push once from Glasir (it asks for your username and a personal access token, which your credential helper then remembers), or install Git Credential Manager. Check credential.helper below.")
    } else if has(&["repository not found", "does not appear to be a git repository", "not found"]) {
        ("Repository not found", "Check the remote URL. GitHub also answers this way when the account you're signed in with has no access to the repository.")
    } else {
        ("Connection failed", "See git's output below.")
    }
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
        (_, Err(e)) if e.contains("could not restore untracked files") => Err(format!(
            "Some untracked files couldn't come back: the new state has files of the same name. They are kept only in the stash \"{msg}\", so don't drop it.\n\n{e}"
        )),
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
    }

    #[test]
    fn classifies_connection_failures() {
        let d = |e: &str, ssh: bool| diagnose(e, ssh).0;
        assert_eq!(d("remote: The 'acme' organization has enabled or enforced SAML SSO. To access this repository, you must re-authorize the OAuth Application.\nfatal: unable to access 'https://github.com/acme/x/': The requested URL returned error: 403", false), "SAML SSO not authorized");
        assert_eq!(d("fatal: unable to access 'https://ghe.corp/x/': SSL certificate problem: unable to get local issuer certificate", false), "TLS certificate not trusted");
        assert_eq!(d("Host key verification failed.\nfatal: Could not read from remote repository.", true), "Unknown or changed SSH host key");
        assert_eq!(d("ssh: Could not resolve hostname ghe.corp: nodename nor servname provided, or not known", true), "Host not found (DNS)");
        assert_eq!(d("fatal: unable to access 'https://ghe.corp/x/': Could not resolve host: ghe.corp", false), "Host not found (DNS)");
        assert_eq!(d("fatal: unable to access 'https://ghe.corp/x/': Failed to connect to proxy.corp port 8080 after 2 ms: Couldn't connect to server", false), "Can't connect (network, proxy or firewall)");
        assert_eq!(d("git@ghe.corp: Permission denied (publickey).\nfatal: Could not read from remote repository.", true), "SSH key rejected");
        assert_eq!(d("fatal: could not read Username for 'https://ghe.corp': terminal prompts disabled", false), "Sign-in required");
        assert_eq!(d("remote: Repository not found.\nfatal: repository 'https://github.com/a/b/' not found", false), "Repository not found");
        assert_eq!(d("fatal: '/nope' does not appear to be a git repository", false), "Repository not found");
        assert_eq!(d("something else", false), "Connection failed");
        // Repository and host names don't count.
        assert_eq!(d("fatal: '/srv/acme/saml-gateway.git' does not appear to be a git repository", false), "Repository not found");
        assert_eq!(d("remote: Repository not found.\nfatal: repository 'https://github.com/acme/envoy-proxy/' not found", false), "Repository not found");
        assert_eq!(d("fatal: unable to access 'https://proxy.corp/self-signed/': Couldn't connect to server", false), "Can't connect (network, proxy or firewall)");
        assert_eq!(d("fatal: unable to access 'https://ghe.corp/x/': Received HTTP code 407 from proxy after CONNECT", false), "Can't connect (network, proxy or firewall)");
        assert_eq!(fetch_failure("fatal: '/srv/saml-gw.git' does not appear to be a git repository"), "Fetch failed");
    }

    #[cfg(unix)]
    #[test]
    fn credential_token_from_helper_only() {
        let dir = std::env::temp_dir().join(format!("glasir-credential-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q"]).unwrap();
        // The empty value drops the user's own helpers (their keychain stays out of it).
        run(&dir, &["config", "credential.helper", ""]).unwrap();
        assert_eq!(credential_token(&dir, "ghe.example"), None, "no helper: no token and no prompt");
        let helper = "!f() { test \"$1\" = get && cat >/dev/null && echo username=x && echo password=tok-123; }; f";
        run(&dir, &["config", "--add", "credential.helper", helper]).unwrap();
        assert_eq!(credential_token(&dir, "ghe.example").as_deref(), Some("tok-123"));
        let log = crate::git::LOG.lock().unwrap();
        let fills: Vec<_> = log.iter().filter(|l| l.cwd == dir && l.args.iter().any(|a| a == "fill")).collect();
        assert!(!fills.is_empty() && !fills.iter().any(|l| l.stderr.contains("tok-123")));
        drop(log);
        let _ = std::fs::remove_dir_all(&dir);
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
