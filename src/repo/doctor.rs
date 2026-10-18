//! Connection doctor for one remote: a never-prompting `ls-remote`, its failure
//! classified with a fix hint, and the credential / proxy settings git uses for the URL.

use super::Task;
use super::sshkey::{KeyInfo, is_ssh};
use crate::git;
use std::path::{Path, PathBuf};

pub struct Report {
    /// The remote URL with any credentials hidden (shown and copied).
    pub url: String,
    /// `None`: connected. Otherwise (problem, fix hint) from `git::diagnose`.
    pub problem: Option<(&'static str, &'static str)>,
    /// git's output (stderr on failure).
    pub output: String,
    /// (setting, value) pairs git will use for this URL, with where each comes from.
    pub settings: Vec<(String, String)>,
}

pub struct Doctor {
    pub remote: String,
    pub report: Option<Report>,
    task: Option<Task<Report>>,
    pub keys: Vec<KeyInfo>,
    keys_task: Option<Task<Vec<KeyInfo>>>,
    pub tested: Option<(bool, String)>,
    pub testing: Option<Task<(bool, String)>>,
}


pub fn check(dir: &Path, remote: &str) -> Report {
    let get = |args: &[&str]| git::run(dir, args).unwrap_or_default();
    let url = get(&["remote", "get-url", remote]).trim().to_owned();
    let res = git::ls_remote_quiet(dir, remote);
    let out = get(&["config", "--show-origin", "-z", "--get-regexp", r"^credential\.(.+\.)?helper$"]);
    let mut settings: Vec<(String, String)> = helpers(&out, &url).into_iter().map(|v| ("credential.helper".into(), v)).collect();
    if settings.is_empty() {
        settings.push(("credential.helper".into(), "none: git can't remember HTTPS passwords or tokens".into()));
    }
    if is_ssh(&url) {
        let ssh = std::env::var("GIT_SSH_COMMAND").ok().or_else(|| Some(get(&["config", "core.sshCommand"]).trim().to_owned()).filter(|s| !s.is_empty())).or_else(|| std::env::var("GIT_SSH").ok());
        settings.push(("ssh command".into(), ssh.unwrap_or_else(|| "ssh (default)".into())));
    } else if url.starts_with("http") {
        // http.<url>.* resolved for this URL: proxy, sslCAInfo, sslBackend, extraHeader...
        for l in get(&["config", "--get-urlmatch", "http", &url]).lines() {
            let (k, v) = l.split_once(' ').unwrap_or((l, ""));
            let v = if k.eq_ignore_ascii_case("http.extraheader") { "(set, hidden)" } else { v };
            settings.push((k.into(), v.into()));
        }
    }
    for k in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy", "NO_PROXY", "no_proxy"] {
        if let Ok(v) = std::env::var(k).map(|v| v.trim().to_owned()).as_deref()
            && !v.is_empty()
        {
            settings.push((format!("${k}"), v.into()));
        }
    }
    // Tokens in the URL, a proxy's password or an ssh command's password must not reach the screen or a pasted report.
    for (k, v) in &mut settings {
        *v = match k.as_str() {
            "ssh command" => git::redact_command(v),
            _ if k.to_ascii_lowercase().ends_with("proxy") => redact_proxy(v),
            _ => git::redact(v),
        };
    }
    let problem = res.as_ref().err().map(|e| git::diagnose(e, is_ssh(&url)));
    let output = res.unwrap_or_else(|e| e).trim().into();
    Report { url: git::redact(&url), problem, output, settings }
}

/// A proxy may omit its scheme (`user:pw@host:port`, for git and curl alike).
fn redact_proxy(v: &str) -> String {
    if v.contains("://") { git::redact(v) } else { git::redact(&format!("http://{v}"))["http://".len()..].to_owned() }
}

fn helper(v: &str) -> String {
    if v.trim_start().starts_with('!') { "inline shell command (hidden: it may hold a token)".into() } else { v.into() }
}

fn helpers(out: &str, url: &str) -> Vec<String> {
    let (mut list, mut records) = (vec![], out.split('\0'));
    while let (Some(origin), Some(kv)) = (records.next(), records.next()) {
        let (k, v) = kv.split_once('\n').unwrap_or((kv, ""));
        if k.strip_prefix("credential.").and_then(|k| k.strip_suffix(".helper")).is_some_and(|p| !url_matches(p, url)) {
            continue;
        }
        if v.is_empty() {
            list.clear();
        } else {
            list.push(format!("{}   ({origin})", helper(v)));
        }
    }
    list
}

fn url_matches(pattern: &str, url: &str) -> bool {
    fn parts(u: &str) -> (Option<String>, Option<&str>, String, &str, &str) {
        let (scheme, rest) = u.split_once("://").map_or((None, u), |(s, r)| (Some(s.to_ascii_lowercase()), r));
        let (auth, path) = rest.split_once(['/', '?', '#']).unwrap_or((rest, ""));
        let (user, host) = auth.rsplit_once('@').map_or((None, auth), |(u, h)| (u.split(':').next(), h));
        let (host, port) = host.rsplit_once(':').unwrap_or((host, ""));
        let port = match (scheme.as_deref(), port) {
            (Some("https"), "443") | (Some("http"), "80") => "",
            _ => port,
        };
        (scheme, user, host.to_ascii_lowercase(), port, path.trim_matches('/'))
    }
    let (p, u) = (parts(pattern), parts(url));
    let host = p.2.split('.').count() == u.2.split('.').count() && p.2.split('.').zip(u.2.split('.')).all(|(a, b)| a == "*" || a == b);
    let path = p.4.is_empty() || p.4 == u.4 || (p.0.is_some() && u.4.strip_prefix(p.4).is_some_and(|r| r.starts_with('/')));
    p.0.is_none_or(|s| u.0 == Some(s)) && p.1.is_none_or(|n| u.1 == Some(n)) && host && p.3 == u.3 && path
}

impl Doctor {
    pub fn open(ctx: &egui::Context, dir: &Path, remote: &str) -> Self {
        let (dir, r) = (dir.to_path_buf(), remote.to_owned());
        Doctor { remote: remote.into(), report: None, task: Some(Task::spawn(ctx, move || check(&dir, &r))), keys: vec![], keys_task: Some(Task::spawn(ctx, super::sshkey::list)), tested: None, testing: None }
    }

    pub fn poll(&mut self) {
        if let Some(r) = Task::poll(&mut self.task) {
            self.report = Some(r);
        }
        if let Some(k) = Task::poll(&mut self.keys_task) {
            self.keys = k;
        }
        if let Some(t) = Task::poll(&mut self.testing) {
            self.tested = Some(t);
        }
    }

    pub fn test(&mut self, ctx: &egui::Context, dir: PathBuf, key: Option<String>, url: String) {
        self.tested = None;
        self.testing = Some(Task::spawn(ctx, move || super::sshkey::test(&dir, key.as_deref(), &url)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::testutil::{identity, scratch, sh};

    #[test]
    fn diagnoses_a_remote() {
        let dir = scratch("doctor");
        sh(&dir, &["init", "-q", "-b", "main"]);
        identity(&dir);
        sh(&dir, &["remote", "add", "gone", &dir.join("nope").to_string_lossy()]);
        sh(&dir, &["remote", "add", "self", &dir.to_string_lossy()]);
        sh(&dir, &["config", "credential.helper", "store"]);
        sh(&dir, &["config", "--add", "credential.helper", "!f() { echo password=ghp_abc; }; f"]);
        let r = check(&dir, "gone");
        assert_eq!(r.problem.map(|p| p.0), Some("Repository not found"));
        assert!(r.settings.iter().any(|(k, v)| k == "credential.helper" && v.starts_with("store ")), "{:?}", r.settings);
        assert!(r.settings.iter().any(|(_, v)| v.starts_with("inline shell command")) && r.settings.iter().all(|(_, v)| !v.contains("ghp_abc")), "{:?}", r.settings);
        assert_eq!((helper("osxkeychain"), helper("cache --timeout=3600")), ("osxkeychain".into(), "cache --timeout=3600".into()));
        assert!(!helper(" !echo password=ghp_abc").contains("ghp_"));
        let out = "file:/etc/gitconfig\0credential.helper\nosxkeychain\0file:/h/.gitconfig\0credential.https://github.com.helper\n\0file:/h/.gitconfig\0credential.https://github.com.helper\n!gh auth git-credential\0\
                   file:/h/.gitconfig\0credential.https://*.corp.example/team.helper\nstore\0file:/h/.gitconfig\0credential.ghe.corp.example.helper\ncache\0";
        let names = |url: &str| helpers(out, url).iter().map(|h| h.split("   (").next().unwrap().to_owned()).collect::<Vec<_>>().join(",");
        assert_eq!(names("https://github.com/o/r.git"), "inline shell command (hidden: it may hold a token)");
        assert_eq!(names("https://me@ghe.corp.example/team/app"), "osxkeychain,store,cache");
        assert_eq!(names("https://A.corp.example:443/team"), "osxkeychain,store");
        assert_eq!(names("https://ghe.corp.example/teams/app"), "osxkeychain,cache");
        for url in ["http://a.corp.example/team", "https://github.com:8443/o/r", "git@github.com:o/r.git", "https://a.b.corp.example/team/x"] {
            assert_eq!(names(url), "osxkeychain", "{url}");
        }
        assert!(helpers(out, "https://github.com/o")[0].ends_with("   (file:/h/.gitconfig)"));
        assert!(check(&dir, "self").problem.is_none());
        assert!(is_ssh("git@ghe.corp:o/r.git") && is_ssh("ssh://git@ghe.corp/o/r"));
        assert!(!is_ssh("https://ghe.corp/o/r") && !is_ssh("/srv/r.git") && !is_ssh("C:\\r") && !is_ssh("./a:b"));
        assert_eq!(redact_proxy("u:pw@proxy.corp:8080"), "***@proxy.corp:8080");
        assert_eq!(redact_proxy("http://u:pw@proxy.corp:8080"), "http://***@proxy.corp:8080");
        assert_eq!(redact_proxy("proxy.corp:3128"), "proxy.corp:3128");
        sh(&dir, &["remote", "add", "ssh1", "git@127.0.0.1:o/r.git"]);
        sh(&dir, &["config", "core.sshCommand", "echo sshpass -p hunter2"]);
        let shown = check(&dir, "ssh1").settings.into_iter().find(|(k, _)| k == "ssh command").unwrap().1;
        assert!(!shown.contains("hunter2"), "{shown}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
