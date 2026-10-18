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
    quiet_ssh(dir, args, input, None)
}

fn quiet_ssh(dir: &Path, args: &[&str], input: Option<&[u8]>, ssh_override: Option<&str>) -> Res<String> {
    let cfg = |k: &str| run(dir, &["config", "--get", k]).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let env = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
    // git's own precedence: GIT_SSH_COMMAND > core.sshCommand > GIT_SSH.
    let ssh = ssh_override.map(str::to_owned).or_else(|| env("GIT_SSH_COMMAND")).or_else(|| cfg("core.sshCommand")).or_else(|| env("GIT_SSH").map(|p| shell_quote(&p)));
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

/// The remotes `fetch --all` visits, each with its fetch refspecs that write under `refs/remotes/`
/// (and its negative ones), from [`config`](super::config)'s pairs; and whether one also maps refs
/// elsewhere (tags, notes), which `fetch --prune` would delete when that remote lacks them.
pub fn tracking(config: &[(String, String)]) -> (Vec<(String, Vec<String>)>, bool) {
    let mut all: Vec<(&str, Vec<String>, bool, bool)> = vec![];
    for (k, v) in config {
        let Some((name, key)) = k.strip_prefix("remote.").and_then(|r| r.rsplit_once('.')) else { continue };
        let i = all.iter().position(|r| r.0 == name).unwrap_or_else(|| {
            all.push((name, vec![], false, false));
            all.len() - 1
        });
        match (key, v.split_once(':')) {
            ("fetch", _) if v.starts_with('^') => all[i].1.push(v.clone()),
            ("fetch", Some((_, dst))) if dst.starts_with("refs/remotes/") => all[i].1.push(v.clone()),
            ("fetch", Some((_, dst))) if !dst.is_empty() => all[i].2 = true,
            ("skipfetchall" | "skipdefaultupdate", _) => all[i].3 = !matches!(v.to_ascii_lowercase().as_str(), "false" | "no" | "off" | "0"),
            _ => {}
        }
    }
    all.retain(|r| !r.3);
    let elsewhere = all.iter().any(|r| r.2);
    (all.into_iter().filter(|r| r.1.iter().any(|s| !s.starts_with('^'))).map(|r| (r.0.to_owned(), r.1)).collect(), elsewhere)
}

/// `fetch --prune --no-tags` of every remote's remote-tracking refs ([`tracking`]) for background
/// refreshes; never prompts. A remote that fails doesn't keep the others from being fetched.
pub fn fetch_quiet(dir: &Path) -> Res<String> {
    // Remote-tracking refs only: tags are local refs that undo snapshots, so a background
    // fetch adding or pruning one would invalidate the undo history. A manual Fetch gets them.
    let mut errs = vec![];
    for (remote, specs) in tracking(&super::config(dir)).0 {
        match fetch_specs(dir, &remote, &specs, true) {
            Err(e) if e == super::CANCELLED => return Err(e),
            Err(e) => errs.push(e),
            Ok(_) => {}
        }
    }
    if errs.is_empty() { Ok(String::new()) } else { Err(errs.join("\n")) }
}

fn fetch_specs(dir: &Path, remote: &str, specs: &[String], prune: bool) -> Res<String> {
    let mut a = vec!["fetch", "--no-tags", "--no-prune-tags", "--no-write-fetch-head", "--quiet"];
    a.extend(if prune && !specs.is_empty() { &["--prune", "--refmap="][..] } else { &["--no-prune"][..] });
    a.push(remote);
    a.extend(specs.iter().map(String::as_str));
    quiet(dir, &a, None)
}

pub fn fetch_remote_quiet(dir: &Path, remote: &str) -> Res<String> {
    let specs = tracking(&super::config(dir)).0.into_iter().find(|r| r.0 == remote).map(|r| r.1).unwrap_or_default();
    fetch_specs(dir, remote, &specs, false)
}

/// Talks to `remote` the way the background fetch does (never prompting): its HEAD.
pub fn ls_remote_quiet(dir: &Path, remote: &str) -> Res<String> {
    quiet(dir, &["ls-remote", remote, "HEAD"], None)
}

pub fn ls_remote_via(dir: &Path, url: &str, ssh: Option<&str>) -> Res<String> {
    quiet_ssh(dir, &["ls-remote", url, "HEAD"], None, ssh)
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
/// If they conflict with the new state, the conflict markers stay and so does the stash;
/// if `f` fails mid-way (a merge stopped on conflicts) or they can't be applied, they stay only in the stash.
pub fn with_autostash(dir: &Path, msg: &str, f: impl FnOnce() -> Res<String>) -> Res<String> {
    let top = || Cancel::default().scope(|| run(dir, &["rev-parse", "-q", "--verify", "refs/stash"])).unwrap_or_default();
    let state = || Cancel::default().scope(|| super::git_dir(dir)).map(|g| super::state(&g)).ok();
    let (before, was) = (top(), state());
    let pushed = run(dir, &["stash", "push", "-u", "-m", msg]);
    let mine = top();
    if mine == before {
        return pushed.and_then(|_| f()); // nothing was stashed: never pop someone else's entry
    }
    let failed = pushed.is_err();
    let res = pushed.and_then(|_| f());
    if let Err(p) = &res
        && !failed
        && state() != was
    {
        return Err(format!("{p}\n\nYour uncommitted changes are kept only in the stash \"{msg}\": apply it once you've finished or aborted this, then drop it."));
    }
    // Outside any cancel scope: a cancelled op must still give the changes back.
    let pop = Cancel::default().scope(|| {
        let name = super::stashes(dir)?.into_iter().find(|s| s.id == mine.trim()).ok_or("The stash holding your changes is gone from the stash list.")?.name;
        run(dir, &["stash", "pop", "--index", &name]).or_else(|e| {
            // Staged changes that don't fit the new HEAD come back unstaged.
            if e.contains("--index") { run(dir, &["stash", "pop", &name]) } else { Err(e) }
        })
    });
    match (res, pop) {
        (res, Ok(_)) => res,
        (Err(p), Err(e)) if failed => Err(format!(
            "{p}\n\nYour changes are kept in the stash \"{msg}\", and your files may hold some of them: check both before you drop it.\n\n{e}"
        )),
        (_, Err(e)) if e.contains("could not restore untracked files") => Err(format!(
            "Some untracked files couldn't come back: the new state has files of the same name. They are kept only in the stash \"{msg}\", so don't drop it.\n\n{e}"
        )),
        (_, Err(e)) if e.contains("CONFLICT") => Err(format!(
            "Your changes conflict with the new state: resolve the conflicts. They are also kept in the stash \"{msg}\"; drop it when you're done.\n\n{e}"
        )),
        (res, Err(e)) => Err(format!(
            "{}Your changes couldn't be reapplied, so they are kept only in the stash \"{msg}\": apply it once you can, then drop it.\n\n{e}",
            res.err().map(|p| format!("{p}\n\n")).unwrap_or_default()
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
    fn single_remote_fetch_keeps_local_tags() {
        let base = std::env::temp_dir().join(format!("glasir-fetch-tags-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (up, dir) = (base.join("up"), base.join("work"));
        std::fs::create_dir_all(&up).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let g = |d: &Path, a: &[&str]| run(d, a).unwrap();
        g(&up, &["init", "-q"]);
        g(&up, &["-c", "user.email=t@t", "-c", "user.name=T", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "one"]);
        g(&dir, &["clone", "-q", up.to_str().unwrap(), "."]);
        g(&dir, &["tag", "local-only"]);
        for k in ["fetch.prune", "fetch.pruneTags"] {
            g(&dir, &["config", k, "true"]);
        }
        fetch_remote_quiet(&dir, "origin").unwrap();
        assert_eq!(g(&dir, &["tag", "-l"]).trim(), "local-only");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn remote_tracking_refspecs() {
        let cfg = |kv: &[(&str, &str)]| kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
        let (remotes, elsewhere) = tracking(&cfg(&[
            ("remote.origin.url", "git@h:o/r.git"),
            ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
            ("remote.origin.fetch", "+refs/tags/*:refs/tags/*"),
            ("remote.origin.fetch", "^refs/heads/huge"),
            ("remote.my.fork.fetch", "refs/heads/main:refs/remotes/my.fork/main"),
            ("remote.mirror.fetch", "+refs/*:refs/*"),
            ("remote.only.fetch", "refs/heads/x"),
            ("remote.slow.fetch", "+refs/heads/*:refs/remotes/slow/*"),
            ("remote.slow.skipfetchall", ""),
            ("remote.pushdefault", "origin"),
        ]));
        let specs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(remotes, [("origin".into(), specs(&["+refs/heads/*:refs/remotes/origin/*", "^refs/heads/huge"])), ("my.fork".into(), specs(&["refs/heads/main:refs/remotes/my.fork/main"]))]);
        assert!(elsewhere);
        let (remotes, elsewhere) = tracking(&cfg(&[("remote.o.fetch", "+refs/heads/*:refs/remotes/o/*"), ("remote.t.fetch", "+refs/tags/*:refs/tags/*"), ("remote.t.skipfetchall", "true")]));
        assert_eq!((remotes.len(), elsewhere), (1, false));
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

    #[test]
    fn autostash_pops_its_own_stash() {
        let dir = std::env::temp_dir().join(format!("glasir-autostash-own-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| run(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "T"]);
        g(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("f"), "one\n").unwrap();
        std::fs::write(dir.join("g"), "one\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("f"), "mine\n").unwrap();
        let res = with_autostash(&dir, "auto", || {
            std::fs::write(dir.join("g"), "theirs\n").unwrap();
            g(&["stash", "push", "-q", "-m", "other"]);
            Ok(String::new())
        });
        assert!(res.is_ok(), "{res:?}");
        assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "mine\n");
        assert_eq!(std::fs::read_to_string(dir.join("g")).unwrap(), "one\n");
        assert!(g(&["stash", "list"]).contains("other"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn autostash_keeps_changes_in_stash_when_op_stops_midway() {
        let dir = std::env::temp_dir().join(format!("glasir-autostash-midway-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| run(&dir, args).unwrap();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "T"]);
        g(&["config", "commit.gpgsign", "false"]);
        let commit = |f: &str, s: &str| {
            std::fs::write(dir.join(f), s).unwrap();
            g(&["commit", "-qam", s]);
        };
        std::fs::write(dir.join("c"), "base\n").unwrap();
        std::fs::write(dir.join("d"), "base\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-qm", "init"]);
        g(&["checkout", "-qb", "src"]);
        commit("c", "src\n");
        g(&["checkout", "-qb", "dst", "main"]);
        commit("c", "dst\n");
        g(&["checkout", "-q", "main"]);
        std::fs::write(dir.join("d"), "mine\n").unwrap();
        let res = with_autostash(&dir, "auto", || run(&dir, &["checkout", "dst", "--"]).and_then(|_| run(&dir, &["merge", "src"])));
        let e = res.unwrap_err();
        assert!(e.contains("Merge conflict in c") && e.contains("only in the stash \"auto\"") && !e.contains("drop it when you're done"), "{e}");
        assert!(g(&["stash", "list"]).contains("auto"));
        g(&["merge", "--abort"]);
        g(&["stash", "pop"]);
        assert_eq!(std::fs::read_to_string(dir.join("d")).unwrap(), "mine\n");
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

    #[cfg(unix)]
    #[test]
    fn autostash_push_that_fails_after_saving_reapplies() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("glasir-autostash-held-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("held")).unwrap();
        run(&dir, &["init", "-q"]).unwrap();
        run(&dir, &["-c", "user.email=t@t", "-c", "user.name=T", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "init"]).unwrap();
        std::fs::write(dir.join("held/x"), "x\n").unwrap();
        std::fs::write(dir.join("notes"), "precious\n").unwrap();
        let held = |mode| std::fs::set_permissions(dir.join("held"), std::fs::Permissions::from_mode(mode)).unwrap();
        held(0o555);
        let res = with_autostash(&dir, "auto", || Err("ran".into()));
        held(0o755);
        assert!(res.as_ref().is_err_and(|e| e.contains("kept in the stash \"auto\"")), "{res:?}");
        assert_eq!(std::fs::read_to_string(dir.join("notes")).unwrap(), "precious\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn autostash_cancelled_after_saving_reapplies() {
        let dir = std::env::temp_dir().join(format!("glasir-autostash-stalled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| run(&dir, args).unwrap();
        let stall = dir.join(".git/stall");
        g(&["init", "-q"]);
        g(&["config", "filter.stall.smudge", &format!("rm '{}' 2>/dev/null && sleep 3; cat", stall.display())]);
        std::fs::write(dir.join(".gitattributes"), "f filter=stall\n").unwrap();
        std::fs::write(dir.join("f"), "one\n").unwrap();
        g(&["add", "-A"]);
        g(&["-c", "user.email=t@t", "-c", "user.name=T", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("f"), "mine\n").unwrap();
        std::fs::write(dir.join("u"), "u\n").unwrap();
        std::fs::write(&stall, "").unwrap();
        let cancel = Cancel::default();
        let c2 = cancel.clone();
        std::thread::spawn(move || {
            while stall.exists() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            c2.cancel();
        });
        let res = cancel.scope(|| with_autostash(&dir, "auto", || run(&dir, &["-c", "alias.slow=!sleep 5", "slow"])));
        assert_eq!(res, Err(CANCELLED.to_owned()));
        let read = |p: &str| std::fs::read_to_string(dir.join(p)).unwrap_or_default();
        assert_eq!((read("f"), read("u"), g(&["stash", "list"])), ("mine\n".into(), "u\n".into(), String::new()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
